//! `MediaCodec` encode matrix over CBR/CQP and the preset range.

#[cfg(target_os = "android")]
use rust_video_caps::encode::{EncodeConfig, Quality, RateControl, SpeedPreset};
#[cfg(target_os = "android")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "android")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "android")]
use rust_video_harness::Synth;
#[cfg(target_os = "android")]
use rust_video_mediacodec::{EncodeSpec, MediaCodecDevice, MediaCodecEncoder, MediaCodecError};

#[cfg(target_os = "android")]
fn main() {
    let device = MediaCodecDevice::probe();
    println!(
        "mediacodec: device {} available={}",
        device.name(),
        device.available()
    );
    let frame = Synth::black_yuv420p(64, 64);
    for (name, spec) in [
        ("h264", EncodeSpec::h264(64, 64, 1_000_000, 30, 1)),
        ("h265", EncodeSpec::h265(64, 64, 1_000_000, 30, 1)),
        ("av1", EncodeSpec::av1(64, 64, 1_000_000, 30, 1)),
        ("vp9", EncodeSpec::vp9(64, 64, 1_000_000, 30, 1)),
        ("vp8", EncodeSpec::vp8(64, 64, 1_000_000, 30, 1)),
    ] {
        let Some(spec) = spec else {
            println!("{name}: spec rejected");
            continue;
        };
        println!("{name}: mime={} min_sdk={}", spec.mime(), spec.min_sdk());
        let identity = (spec.config().profile, spec.config().level);
        for (rate_name, rate) in rates() {
            for preset in presets() {
                let cell = format!("{name}/{rate_name}/{preset:?}");
                let config = EncodeConfig {
                    rate_control: rate,
                    quality: Quality { preset },
                    profile: identity.0,
                    level: identity.1,
                    ..Default::default()
                };
                run(
                    &cell,
                    MediaCodecEncoder::open(&spec.clone().with_config(config)),
                    &frame,
                );
            }
        }
    }
}

/// Matrix rate cells: CBR target plus constant-QP.
#[cfg(target_os = "android")]
fn rates() -> [(&'static str, RateControl); 2] {
    [
        (
            "cbr",
            RateControl::Cbr {
                bitrate: 500_000,
                maxrate: 500_000,
                bufsize: 500_000,
            },
        ),
        ("cqp", RateControl::Cqp { qp: 23 }),
    ]
}

/// Matrix presets in fastest-first order.
#[cfg(target_os = "android")]
fn presets() -> [SpeedPreset; 3] {
    [
        SpeedPreset::Realtime,
        SpeedPreset::Balanced,
        SpeedPreset::Quality,
    ]
}

/// One matrix cell over `frame` through an opened `encoder`.
#[cfg(target_os = "android")]
fn run(cell: &str, opened: Result<MediaCodecEncoder, MediaCodecError>, frame: &CpuFrame) {
    let mut encoder = match opened {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{cell}: open declined ({error})");
            return;
        }
    };
    let mut packets = match encoder.encode(frame) {
        Ok(packets) => packets,
        Err(error) => {
            println!("{cell}: encode rejected ({error})");
            return;
        }
    };
    match encoder.flush() {
        Ok(tail) => packets.extend(tail),
        Err(error) => println!("{cell}: flush rejected ({error})"),
    }
    let bytes: usize = packets.iter().map(|packet| packet.data().len()).sum();
    let keyed = packets.iter().filter(|packet| packet.is_keyframe()).count();
    println!(
        "{cell}: {} packets, {bytes} bytes, {keyed} keyframes",
        packets.len()
    );
}

#[cfg(not(target_os = "android"))]
fn main() {
    println!("SKIP: mediacodec needs android");
}
