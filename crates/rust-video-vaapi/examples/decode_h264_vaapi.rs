//! VA-API H.264 decode over the first capable render node.

#[cfg(target_os = "linux")]
use bytes::Bytes;

#[cfg(target_os = "linux")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "linux")]
use rust_video_vaapi::{H264Decoder, VaApiDevice, VaApiError};

#[cfg(target_os = "linux")]
fn main() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("va-api h264: skipped ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h264: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api h264: device {}", device.name());
    let mut decoder = H264Decoder::open(&device);
    let empty = Packet::new(Bytes::new());
    match decoder.decode(&empty) {
        Ok(frames) => println!("va-api h264: empty packet -> {} frames", frames.len()),
        Err(error) => println!("va-api h264: empty packet rejected ({error})"),
    }
    match decoder.flush() {
        Ok(frames) => println!("va-api h264: flush -> {} frames", frames.len()),
        Err(error) => println!("va-api h264: flush rejected ({error})"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {}
