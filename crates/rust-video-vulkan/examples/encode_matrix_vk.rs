//! Vulkan encode matrix over CBR/CQP and the preset range.

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::{EncodeConfig, Level, Profile, Quality, RateControl, SpeedPreset};
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::traits::Encoder;
use rust_video_vulkan::{CodecOp, H264Encoder, H265Encoder, VulkanDevice};

fn main() {
    let frame = gray_nv12(320, 240);
    if let Some(device) = probe_device(CodecOp::H264Encode, "h264-encode") {
        for (rate_name, rate) in rates() {
            for preset in presets() {
                let cell = format!("h264/{rate_name}/{preset:?}");
                run(
                    &cell,
                    H264Encoder::open(&device, 320, 240, h264_config(rate, preset)),
                    &frame,
                );
            }
        }
    }
    if let Some(device) = probe_device(CodecOp::H265Encode, "h265-encode") {
        for (rate_name, rate) in rates() {
            for preset in presets() {
                let cell = format!("h265/{rate_name}/{preset:?}");
                run(
                    &cell,
                    H265Encoder::open(&device, 320, 240, h265_config(rate, preset)),
                    &frame,
                );
            }
        }
    }
}

/// Mid-grey NV12 frame at `width`x`height`.
fn gray_nv12(width: u32, height: u32) -> CpuFrame {
    let y_len = width as usize * height as usize;
    let bytes = Bytes::from(vec![0x80; y_len * 3 / 2]);
    let size = PictureSize::new(width, height).expect("nonzero extent");
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(bytes.slice(..y_len), width).expect("luma plane"),
            Plane::new(bytes.slice(y_len..), width).expect("chroma plane"),
        ],
    )
    .expect("nv12 frame")
}

/// Matrix rate cells: CBR target plus constant-QP.
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
fn presets() -> [SpeedPreset; 3] {
    [
        SpeedPreset::Realtime,
        SpeedPreset::Balanced,
        SpeedPreset::Quality,
    ]
}

/// H.264 matrix config under `rate` and `preset`.
fn h264_config(rate: RateControl, preset: SpeedPreset) -> EncodeConfig {
    EncodeConfig {
        rate_control: rate,
        quality: Quality { preset },
        ..Default::default()
    }
}

/// H.265 matrix config on Main auto under `rate` and `preset`.
fn h265_config(rate: RateControl, preset: SpeedPreset) -> EncodeConfig {
    EncodeConfig {
        rate_control: rate,
        quality: Quality { preset },
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..Default::default()
    }
}

/// Device for `op`, or `None` after a loud skip.
fn probe_device(op: CodecOp, name: &str) -> Option<VulkanDevice> {
    match VulkanDevice::open(op) {
        Ok(Some(device)) => Some(device),
        Ok(None) => {
            println!("{name}: no vulkan device; skipped");
            None
        }
        Err(error) => {
            println!("{name}: negotiation failed ({error})");
            None
        }
    }
}

/// One matrix cell over `frame` through an opened `encoder`.
fn run<E: Encoder>(cell: &str, opened: Result<E, E::Error>, frame: &CpuFrame)
where
    E::Error: std::fmt::Display,
{
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
