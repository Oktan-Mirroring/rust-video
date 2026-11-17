//! VA-API AV1 decode: file in, frames out, hashes printed.
//!
//! Reads MP4 (demuxed) or raw OBU; pass a corpus vector:
//! `decode_av1_vaapi corpus/synth/av1/av1-main.mp4`.

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
    use rust_video_vaapi::{Av1Decoder, VaApiDevice, VaApiError};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: decode_av1_vaapi <input.mp4|input.obu>");
        std::process::exit(2);
    });
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api av1 ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api av1: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api av1: device {}", device.name());
    let samples = input::samples("va-api av1", &path);
    println!("va-api av1: {} packets from {path}", samples.len());
    let mut decoder = Av1Decoder::open(&device);
    let mut frames_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match decoder.decode(&packet) {
            Ok(frames) => frames::print_frames(&frames, &mut frames_out),
            Err(error) => {
                eprintln!("va-api av1: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => frames::print_frames(&frames, &mut frames_out),
        Err(error) => {
            eprintln!("va-api av1: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api av1: {frames_out} frames");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
