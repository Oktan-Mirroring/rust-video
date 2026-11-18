//! VA-API VP9 decode: file in, frames out, hashes printed.
//!
//! Reads MP4 (demuxed) or a raw VP9 chunk; pass a corpus vector:
//! `decode_vp9_vaapi corpus/vp9/vp90-2-03-size-352x288.webm`.

#[path = "common/frames.rs"]
#[cfg(target_os = "linux")]
mod frames;

#[path = "common/input.rs"]
#[cfg(target_os = "linux")]
mod input;

#[cfg(target_os = "linux")]
fn main() {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::{Decoder, Device};
    use rust_video_vaapi::{VaApiDevice, VaApiError, Vp9Decoder};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: decode_vp9_vaapi <input.mp4|input.chunk>");
        std::process::exit(2);
    });
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api vp9 ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api vp9: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api vp9: device {}", device.name());
    let samples = input::samples("va-api vp9", &path);
    println!("va-api vp9: {} packets from {path}", samples.len());
    let mut decoder = Vp9Decoder::open(&device);
    let mut frames_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match decoder.decode(&packet) {
            Ok(frames) => frames::print_frames(&frames, &mut frames_out),
            Err(error) => {
                eprintln!("va-api vp9: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => frames::print_frames(&frames, &mut frames_out),
        Err(error) => {
            eprintln!("va-api vp9: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api vp9: {frames_out} frames");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
