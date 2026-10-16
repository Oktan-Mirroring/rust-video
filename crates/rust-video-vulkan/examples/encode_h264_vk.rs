//! Vulkan H.264 IDR encode over a real input file.
//!
//! Reads `VK_VECTOR` (raw NV12 at `VK_WIDTH`x`VK_HEIGHT`), encodes each
//! frame as IDR on the Vulkan tier, and writes Annex-B to `VK_OUT`.
//! Loud skip without hardware or input.

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::traits::Encoder;
use rust_video_vulkan::{CodecOp, H264Encoder, VulkanDevice};

fn main() {
    let width: u32 = std::env::var("VK_WIDTH")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(320);
    let height: u32 = std::env::var("VK_HEIGHT")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(240);
    let vector = std::env::var("VK_VECTOR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("nv12-320x240.yuv")
            .to_string_lossy()
            .into_owned()
    });
    let out_path = std::env::var("VK_OUT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("vk_h264.h264")
            .to_string_lossy()
            .into_owned()
    });
    let Ok(bytes) = std::fs::read(&vector) else {
        println!("encode_h264_vk: no vector at {vector}; skipped");
        return;
    };
    let device = match VulkanDevice::open(CodecOp::H264Encode) {
        Ok(Some(device)) => device,
        Ok(None) => {
            println!("encode_h264_vk: no vulkan device; skipped");
            return;
        }
        Err(error) => {
            eprintln!("encode_h264_vk: negotiation failed ({error})");
            std::process::exit(1);
        }
    };
    let mut encoder = match H264Encoder::open(&device, width, height) {
        Ok(encoder) => encoder,
        Err(error) => {
            eprintln!("encode_h264_vk: open failed ({error})");
            std::process::exit(1);
        }
    };
    let frame_len = width as usize * height as usize * 3 / 2;
    let mut out = Vec::new();
    let mut count = 0;
    for chunk in bytes.chunks(frame_len) {
        if chunk.len() < frame_len {
            break;
        }
        let Some(frame) = nv12_frame(width, height, chunk) else {
            eprintln!("encode_h264_vk: plane build failed");
            std::process::exit(1);
        };
        for packet in encoder.encode(&frame).expect("encode") {
            out.extend_from_slice(packet.data());
            count += 1;
        }
    }
    for packet in encoder.flush().expect("flush") {
        out.extend_from_slice(packet.data());
        count += 1;
    }
    std::fs::write(&out_path, &out).expect("write");
    eprintln!("packets={count} bytes={}", out.len());
}

/// NV12 frame over one tight `chunk`.
fn nv12_frame(width: u32, height: u32, chunk: &[u8]) -> Option<CpuFrame> {
    let size = PictureSize::new(width, height)?;
    let y_len = width as usize * height as usize;
    let bytes = Bytes::copy_from_slice(chunk);
    CpuFrame::new(
        size,
        PixelFormat::Nv12,
        vec![
            Plane::new(bytes.slice(..y_len), width)?,
            Plane::new(bytes.slice(y_len..), width)?,
        ],
    )
}
