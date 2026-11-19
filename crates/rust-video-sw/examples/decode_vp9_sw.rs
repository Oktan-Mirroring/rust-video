//! VP9 decode on the software tier, from the committed vector to frames.
//!
//! `decode_vp9_sw [vector]`; defaults to the committed 320x240 vector.

#[cfg(feature = "ffmpeg")]
use bytes::Bytes;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::frame::CpuFrame;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::traits::Decoder;
#[cfg(feature = "ffmpeg")]
use rust_video_harness::{decode_dir, Ivf};
#[cfg(feature = "ffmpeg")]
use rust_video_sw::Vp9Decoder;

#[cfg(feature = "ffmpeg")]
fn main() {
    let path = std::env::args().nth(1).map_or_else(
        || decode_dir().join("valid_320x240.ivf"),
        std::path::PathBuf::from,
    );
    let inputs = Ivf::read(&path).frames;
    let mut decoder = Vp9Decoder::open(4).expect("decoder init");
    let mut frames_out = 0;
    let mut bytes_out = 0;
    for bytes in &inputs {
        let packet = Packet::new(Bytes::from(bytes.clone()));
        match decoder.decode(&packet) {
            Ok(frames) => tally(&frames, &mut frames_out, &mut bytes_out),
            Err(error) => {
                eprintln!("sw vp9: decode failed ({error})");
                std::process::exit(1);
            }
        }
    }
    match decoder.flush() {
        Ok(frames) => tally(&frames, &mut frames_out, &mut bytes_out),
        Err(error) => {
            eprintln!("sw vp9: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("sw vp9: {frames_out} frames, {bytes_out} bytes");
}

/// Frames and packed plane bytes tallied into the counters.
#[cfg(feature = "ffmpeg")]
fn tally(frames: &[CpuFrame], frames_out: &mut usize, bytes_out: &mut usize) {
    for frame in frames {
        *bytes_out += frame
            .planes()
            .iter()
            .map(|plane| plane.data().len())
            .sum::<usize>();
        *frames_out += 1;
    }
}

#[cfg(not(feature = "ffmpeg"))]
fn main() {
    println!("SKIP: decode_vp9_sw needs --features ffmpeg");
}
