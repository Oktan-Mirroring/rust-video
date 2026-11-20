//! D3D11 encode matrix over CBR/CQP and the preset range.
//!
//! CQP cells decline: the MFT tier maps bitrate modes only.

#[cfg(target_os = "windows")]
use rust_video_caps::encode::{EncodeConfig, Level, Profile, Quality, RateControl, SpeedPreset};
#[cfg(target_os = "windows")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Device, Encoder};
#[cfg(target_os = "windows")]
use rust_video_d3d11::{D3d11Device, D3d11Encoder, D3d11Error};
#[cfg(target_os = "windows")]
use rust_video_harness::Synth;

#[cfg(target_os = "windows")]
fn main() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d11 device ({error})");
            return;
        }
    };
    println!("device: {}", device.name());
    let frame = Synth::grey_nv12(64, 48);
    for (codec_name, identity) in [
        ("h264", h264_identity()),
        ("h265", h265_identity()),
        ("av1", av1_identity()),
        ("vp9", vp9_identity()),
    ] {
        for (rate_name, rate) in rates() {
            for preset in presets() {
                let cell = format!("{codec_name}/{rate_name}/{preset:?}");
                let config = EncodeConfig {
                    rate_control: rate,
                    quality: Quality { preset },
                    profile: identity.0,
                    level: identity.1,
                    ..Default::default()
                };
                run(&cell, D3d11Encoder::open(&device, config), &frame);
            }
        }
    }
}

/// Matrix rate cells: CBR target plus constant-QP.
#[cfg(target_os = "windows")]
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
#[cfg(target_os = "windows")]
fn presets() -> [SpeedPreset; 3] {
    [
        SpeedPreset::Realtime,
        SpeedPreset::Balanced,
        SpeedPreset::Quality,
    ]
}

/// H.264 identity: Main profile, auto level.
#[cfg(target_os = "windows")]
fn h264_identity() -> (Profile, Level) {
    (
        Profile::H264(rust_video_caps::h264::EncodeProfile::Main),
        Level::H264(rust_video_caps::h264::EncodeLevel::Auto),
    )
}

/// H.265 identity: Main profile, auto level.
#[cfg(target_os = "windows")]
fn h265_identity() -> (Profile, Level) {
    (
        Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
    )
}

/// AV1 identity: Main profile, auto level.
#[cfg(target_os = "windows")]
fn av1_identity() -> (Profile, Level) {
    (
        Profile::Av1(rust_video_caps::av1::EncodeProfile::Main),
        Level::Av1(rust_video_caps::av1::EncodeLevel::Auto),
    )
}

/// VP9 identity: P0 profile, auto level.
#[cfg(target_os = "windows")]
fn vp9_identity() -> (Profile, Level) {
    (
        Profile::Vp9(rust_video_caps::vp9::EncodeProfile::P0),
        Level::Vp9(rust_video_caps::vp9::EncodeLevel::Auto),
    )
}

/// One matrix cell over `frame` through an opened `encoder`.
#[cfg(target_os = "windows")]
fn run(cell: &str, opened: Result<D3d11Encoder, D3d11Error>, frame: &CpuFrame) {
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

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("SKIP: d3d11 needs windows");
}
