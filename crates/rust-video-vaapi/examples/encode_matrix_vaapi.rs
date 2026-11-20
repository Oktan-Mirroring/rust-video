//! VA-API encode matrix over CBR/CQP and the preset range.

#[cfg(target_os = "linux")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "linux")]
use rust_video_caps::encode::{EncodeConfig, Level, Profile, Quality, RateControl, SpeedPreset};
#[cfg(target_os = "linux")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "linux")]
use rust_video_harness::Synth;
#[cfg(target_os = "linux")]
use rust_video_vaapi::{Codec, VaApiDevice, VaApiEncoder, VaApiError};

#[cfg(target_os = "linux")]
fn main() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api encode matrix ({reason})");
            return;
        }
        Err(error) => {
            println!("SKIP va-api encode matrix: device failed ({error})");
            return;
        }
    };
    println!("va-api encode matrix: device {}", device.name());
    let Some(size) = PictureSize::new(320, 240) else {
        println!("SKIP va-api encode matrix: zero extent");
        return;
    };
    let frame = Synth::gradient(size, 0);
    for (codec_name, codec) in [("h264", Codec::H264), ("h265", Codec::H265)] {
        for (rate_name, rate) in rates() {
            for preset in presets() {
                let cell = format!("{codec_name}/{rate_name}/{preset:?}");
                let config = match codec {
                    Codec::H264 => h264_config(rate, preset),
                    Codec::H265 => h265_config(rate, preset),
                    _ => unreachable!("matrix codecs have configs"),
                };
                run(&cell, VaApiEncoder::open(&device, size, config), &frame);
            }
        }
    }
}

/// Matrix rate cells: CBR target plus constant-QP.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn presets() -> [SpeedPreset; 3] {
    [
        SpeedPreset::Realtime,
        SpeedPreset::Balanced,
        SpeedPreset::Quality,
    ]
}

/// H.264 matrix config on High auto under `rate` and `preset`.
#[cfg(target_os = "linux")]
fn h264_config(rate: RateControl, preset: SpeedPreset) -> EncodeConfig {
    EncodeConfig {
        rate_control: rate,
        quality: Quality { preset },
        profile: Profile::H264(rust_video_caps::h264::EncodeProfile::High),
        level: Level::H264(rust_video_caps::h264::EncodeLevel::Auto),
        ..EncodeConfig::default()
    }
}

/// H.265 matrix config on Main auto under `rate` and `preset`.
#[cfg(target_os = "linux")]
fn h265_config(rate: RateControl, preset: SpeedPreset) -> EncodeConfig {
    EncodeConfig {
        rate_control: rate,
        quality: Quality { preset },
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..EncodeConfig::default()
    }
}

/// One matrix cell over `frame` through an opened `encoder`.
#[cfg(target_os = "linux")]
fn run(cell: &str, opened: Result<VaApiEncoder, VaApiError>, frame: &CpuFrame) {
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

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
