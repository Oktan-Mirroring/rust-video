//! AV1 decode on the software tier, from the committed vector to frames.
//!
//! `decode_av1_sw [vector]`; defaults to the committed 320x240 vector.

#[cfg(feature = "ffmpeg")]
use bytes::Bytes;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::frame::CpuFrame;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::traits::Decoder;
#[cfg(feature = "ffmpeg")]
use rust_video_harness::decode_dir;
#[cfg(feature = "ffmpeg")]
use rust_video_sw::Av1Decoder;

#[cfg(feature = "ffmpeg")]
fn main() {
    let path = std::env::args().nth(1).map_or_else(
        || decode_dir().join("valid_320x240.obu"),
        std::path::PathBuf::from,
    );
    let bytes = std::fs::read(&path).expect("vector readable");
    let mut decoder = Av1Decoder::open(4).expect("decoder init");
    let mut frames_out = 0;
    let mut bytes_out = 0;
    let packet = Packet::new(Bytes::from(bytes));
    match decoder.decode(&packet) {
        Ok(frames) => tally(&frames, &mut frames_out, &mut bytes_out),
        Err(error) => {
            eprintln!("sw av1: decode failed ({error})");
            std::process::exit(1);
        }
    }
    match decoder.flush() {
        Ok(frames) => tally(&frames, &mut frames_out, &mut bytes_out),
        Err(error) => {
            eprintln!("sw av1: flush failed ({error})");
            std::process::exit(1);
        }
    }
    println!("sw av1: {frames_out} frames, {bytes_out} bytes");
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
    println!("SKIP: decode_av1_sw needs --features ffmpeg");
}
