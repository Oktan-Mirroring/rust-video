//! VA-API H.265 decode: file in, frames out, hashes printed.
//!
//! Reads MP4 (demuxed) or raw Annex B; pass a corpus vector:
//! `decode_h265_vaapi corpus/synth/h265/h265-main.mp4`.

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
    use rust_video_vaapi::{H265Decoder, VaApiDevice, VaApiError};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: decode_h265_vaapi <input.mp4|input.h265>");
        std::process::exit(2);
    });
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(VaApiError::Skipped(reason)) => {
            println!("SKIP va-api h265 ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("va-api h265: device failed ({error})");
            std::process::exit(1);
        }
    };
    println!("va-api h265: device {}", device.name());
    let samples = input::samples("va-api h265", &path);
    println!("va-api h265: {} packets from {path}", samples.len());
    let mut decoder = H265Decoder::open(&device);
    let mut frames_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match decoder.decode(&packet) {
            Ok(frames) => frames::print_frames(&frames, &mut frames_out),
            Err(error) => {
                eprintln!("va-api h265: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => frames::print_frames(&frames, &mut frames_out),
        Err(error) => {
            eprintln!("va-api h265: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api h265: {frames_out} frames");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("SKIP: vaapi needs linux");
}
