//! Vulkan decode matrix over the shipped H.264/H.265 decoders.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{
    CodecOp, H264Decoder, H265Decoder, VulkanDevice, VulkanError, VulkanFrame,
};

fn main() {
    if let Some(device) = probe_device(CodecOp::H264Decode, "h264") {
        run("h264", H264Decoder::open(&device));
    }
    if let Some(device) = probe_device(CodecOp::H265Decode, "h265") {
        run("h265", H265Decoder::open(&device));
    }
}

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

fn run<D>(name: &str, opened: Result<D, VulkanError>)
where
    D: Decoder<Frame = VulkanFrame, Error = VulkanError>,
{
    let mut decoder = match opened {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("{name}: open failed ({error})");
            return;
        }
    };
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("{name}: empty packet -> {} frames", frames.len()),
        Err(error) => println!("{name}: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("{name}: flush -> {} frames", frames.len()),
        Err(error) => println!("{name}: flush rejected ({error})"),
    }
}
