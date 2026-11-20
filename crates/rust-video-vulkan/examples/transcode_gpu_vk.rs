//! Vulkan GPU transcode over a real input file.
//!
//! Reads `VK_VECTOR` (Annex-B of `VK_DECODE`, h264 or h265), decodes on
//! the Vulkan tier, re-encodes to `VK_ENCODE` under the default config
//! with a no-op surface filter and a no-op CPU callback, and writes
//! Annex-B to `VK_OUT` with the executed plan on stderr. Loud skip
//! without hardware or input.

use bytes::Bytes;

use rust_video_caps::encode::{EncodeConfig, Level, Profile};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_vulkan::{CodecOp, Transcoder, VulkanDevice, VulkanFrame};

fn main() {
    let decode = match std::env::var("VK_DECODE").as_deref() {
        Ok("h265") => CodecOp::H265Decode,
        _ => CodecOp::H264Decode,
    };
    let encode = match std::env::var("VK_ENCODE").as_deref() {
        Ok("h265") => CodecOp::H265Encode,
        _ => CodecOp::H264Encode,
    };
    let config = match encode {
        CodecOp::H265Encode => EncodeConfig {
            profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
            level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
            ..Default::default()
        },
        _ => EncodeConfig::default(),
    };
    let vector = std::env::var("VK_VECTOR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("h264-high.h264")
            .to_string_lossy()
            .into_owned()
    });
    let out_path = std::env::var("VK_OUT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("vk_xcode.bin")
            .to_string_lossy()
            .into_owned()
    });
    let Ok(bytes) = std::fs::read(&vector) else {
        println!("transcode_gpu_vk: no vector at {vector}; skipped");
        return;
    };
    let Some(decode_device) = open_device(decode, "decode") else {
        return;
    };
    let Some(encode_device) = open_device(encode, "encode") else {
        return;
    };
    let mut transcoder = match Transcoder::open(&decode_device, decode, &encode_device, encode) {
        Ok(transcoder) => transcoder.with_config(config),
        Err(error) => {
            eprintln!("transcode_gpu_vk: open failed ({error})");
            std::process::exit(1);
        }
    };
    let mut surface_filter = |_surface: &mut VulkanFrame| {};
    let packets = transcoder
        .transcode_surface_with(&Packet::new(Bytes::from(bytes)), &mut surface_filter)
        .expect("transcode");
    let mut out = Vec::new();
    for packet in &packets {
        out.extend_from_slice(packet.data());
    }
    let mut cpu_callback = |_frame: &mut CpuFrame| {};
    let flushed = transcoder.flush_with(&mut cpu_callback).expect("flush");
    for packet in &flushed {
        out.extend_from_slice(packet.data());
    }
    std::fs::write(&out_path, &out).expect("write");
    eprintln!(
        "route={:?} cause={:?} bytes={}",
        transcoder.plan().route(),
        transcoder.plan().cause(),
        out.len()
    );
}

/// Device for `op`, or `None` after a loud skip.
fn open_device(op: CodecOp, name: &str) -> Option<VulkanDevice> {
    match VulkanDevice::open(op) {
        Ok(Some(device)) => Some(device),
        Ok(None) => {
            println!("transcode_gpu_vk: no vulkan {name} device; skipped");
            None
        }
        Err(error) => {
            eprintln!("transcode_gpu_vk: {name} negotiation failed ({error})");
            std::process::exit(1);
        }
    }
}
