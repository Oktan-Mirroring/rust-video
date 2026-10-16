//! VA-API H.264 decode: file in, frames out, hashes printed.
//!
//! Reads MP4 (demuxed) or raw Annex B; pass a corpus vector:
//! `decode_h264_vaapi corpus/synth/h264/h264-high.mp4`.

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
    use rust_video_vaapi::{H264Decoder, VaApiDevice, VaApiError};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: decode_h264_vaapi <input.mp4|input.h264>");
        std::process::exit(2);
    });
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
    let samples = input::samples("va-api h264", &path);
    println!("va-api h264: {} packets from {path}", samples.len());
    let mut decoder = H264Decoder::open(&device);
    let mut frames_out = 0;
    for sample in samples {
        let packet = Packet::new(Bytes::from(sample));
        match decoder.decode(&packet) {
            Ok(frames) => frames::print_frames(&frames, &mut frames_out),
            Err(error) => {
                eprintln!("va-api h264: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => frames::print_frames(&frames, &mut frames_out),
        Err(error) => {
            eprintln!("va-api h264: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("va-api h264: {frames_out} frames");
}

#[cfg(not(target_os = "linux"))]
fn main() {}
