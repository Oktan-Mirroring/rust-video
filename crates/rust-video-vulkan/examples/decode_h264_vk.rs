//! Vulkan H.264 decode over a real input file.
//!
//! Reads `VK_VECTOR` (Annex-B H.264), decodes on the Vulkan tier, and
//! writes planar bytes to `VK_OUT`. Loud skip without hardware or input.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{CodecOp, H264Decoder, VulkanDevice};

fn main() {
    let vector = std::env::var("VK_VECTOR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("h264-high.h264")
            .to_string_lossy()
            .into_owned()
    });
    let out_path = std::env::var("VK_OUT").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("vk_h264.yuv")
            .to_string_lossy()
            .into_owned()
    });
    let Ok(bytes) = std::fs::read(&vector) else {
        println!("decode_h264_vk: no vector at {vector}; skipped");
        return;
    };
    let device = match VulkanDevice::open(CodecOp::H264Decode) {
        Ok(Some(device)) => device,
        Ok(None) => {
            println!("decode_h264_vk: no vulkan device; skipped");
            return;
        }
        Err(error) => {
            eprintln!("decode_h264_vk: negotiation failed ({error})");
            std::process::exit(1);
        }
    };
    let mut decoder = match H264Decoder::open(&device) {
        Ok(decoder) => decoder,
        Err(error) => {
            eprintln!("decode_h264_vk: open failed ({error})");
            std::process::exit(1);
        }
    };
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut count = 0;
    for frame in decoder
        .decode(&Packet::new(Bytes::from(bytes)))
        .expect("decode")
    {
        frame.yuv420p_into(&mut scratch);
        out.extend_from_slice(&scratch);
        count += 1;
    }
    for frame in decoder.flush().expect("flush") {
        frame.yuv420p_into(&mut scratch);
        out.extend_from_slice(&scratch);
        count += 1;
    }
    std::fs::write(&out_path, &out).expect("write");
    eprintln!("decoded={count} bytes={}", out.len());
}
