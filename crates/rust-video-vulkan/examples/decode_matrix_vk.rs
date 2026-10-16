//! Vulkan codec matrix over the shipped decoders and encoders.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{
    CodecOp, H264Decoder, H265Decoder, Transcoder, VulkanDevice, VulkanError, VulkanFrame,
};

fn main() {
    if let Some(device) = probe_device(CodecOp::H264Decode, "h264-decode") {
        run_decode("h264", H264Decoder::open(&device));
    }
    if let Some(device) = probe_device(CodecOp::H265Decode, "h265-decode") {
        run_decode("h265", H265Decoder::open(&device));
    }
    if probe_device(CodecOp::H264Encode, "h264-encode").is_some() {
        println!("h264-encode: session negotiates");
    }
    if probe_device(CodecOp::H265Encode, "h265-encode").is_some() {
        println!("h265-encode: session negotiates");
    }
    probe_transcode();
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

fn run_decode<D>(name: &str, opened: Result<D, VulkanError>)
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

fn probe_transcode() {
    let Some(decode) = probe_device(CodecOp::H264Decode, "xcode-decode") else {
        return;
    };
    let Some(encode) = probe_device(CodecOp::H264Encode, "xcode-encode") else {
        return;
    };
    match Transcoder::open(&decode, CodecOp::H264Decode, &encode, CodecOp::H264Encode) {
        Ok(transcoder) => println!(
            "transcode: route={:?} cause={:?}",
            transcoder.plan().route(),
            transcoder.plan().cause(),
        ),
        Err(error) => println!("transcode: open failed ({error})"),
    }
}
