//! VA-API H.265 decode: file in, frames out, hashes printed.
//!
//! Reads MP4 (demuxed) or raw Annex B; the frontend is staged, so the
//! decode declines loud until it is wired:
//! `decode_h265_vaapi corpus/synth/h265/h265-main.mp4`.

#[cfg(target_os = "linux")]
#[path = "common/frames.rs"]
mod frames;

#[cfg(target_os = "linux")]
#[path = "common/input.rs"]
mod input;

#[cfg(target_os = "linux")]
fn main() {
    use bytes::Bytes;
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::{Decoder, Device};
    use rust_video_vaapi::{H265Decoder, VaApiDevice, VaApiError};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: decode_h265_vaapi <input.mp4|input.h265>");
        std::process::exit(2);
    });
    match VaApiDevice::open() {
        Ok(device) => println!("va-api h265: device {}", device.name()),
        Err(VaApiError::Skipped(reason)) => {
            println!("va-api h265: skipped ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h265: device failed ({error})");
            std::process::exit(1);
        }
    }
    let samples = input::samples("va-api h265", &path);
    println!("va-api h265: {} packets from {path}", samples.len());
    let mut decoder = H265Decoder;
    let mut frames_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match decoder.decode(&packet) {
            Ok(frames) => frames::print_frames(&frames, &mut frames_out),
            Err(VaApiError::Unsupported(message)) => {
                println!("va-api h265: unsupported ({message})");
                return;
            }
            Err(error) => {
                eprintln!("va-api h265: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => frames_out += frames.len(),
        Err(error) => {
            eprintln!("va-api h265: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api h265: {frames_out} frames");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
