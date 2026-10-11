//! Candidate benchmark probe: decode against goldens, encode for throughput.

#[cfg(any(
    feature = "h264-openh264",
    feature = "h264-ffmpeg",
    feature = "av1-dav1d",
    feature = "vpx"
))]
mod common;

#[cfg(feature = "h264-openh264")]
mod h264;

#[cfg(feature = "h264-ffmpeg")]
mod ffmpeg;

#[cfg(feature = "av1-dav1d")]
mod av1;

#[cfg(feature = "vpx")]
mod vpx;

#[cfg(any(
    feature = "h264-openh264",
    feature = "h264-ffmpeg",
    feature = "av1-dav1d",
    feature = "vpx"
))]
use std::path::Path;

/// Probe entry point.
///
/// # Panics
///
/// Panics on bad arguments or unreadable inputs.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map_or("", String::as_str);
    match cmd {
        #[cfg(feature = "h264-openh264")]
        "h264dec" => {
            let max = args.get(3).map_or(usize::MAX, |s| s.parse().expect("max"));
            let dump = args.get(4).map(Path::new);
            h264::run_decode(Path::new(&args[2]), max, dump);
        }
        #[cfg(feature = "h264-ffmpeg")]
        "h264ff" => {
            let max = args.get(3).map_or(usize::MAX, |s| s.parse().expect("max"));
            let threads = args.get(4).map_or(4, |s| s.parse().expect("threads"));
            let dump = args.get(5).map(Path::new);
            ffmpeg::run_decode(Path::new(&args[2]), max, threads, dump);
        }
        #[cfg(feature = "h264-openh264")]
        "h264enc" => {
            let w = args[3].parse().expect("width");
            let h = args[4].parse().expect("height");
            let frames = args[5].parse().expect("frames");
            h264::run_encode(Path::new(&args[2]), w, h, frames, Path::new(&args[6]));
        }
        #[cfg(feature = "av1-dav1d")]
        "av1dec" => {
            let max = args.get(3).map_or(usize::MAX, |s| s.parse().expect("max"));
            let threads = args.get(4).map_or(4, |s| s.parse().expect("threads"));
            let dump = args.get(5).map(Path::new);
            av1::run_decode(Path::new(&args[2]), max, threads, dump);
        }
        #[cfg(feature = "vpx")]
        "vpxdec" => {
            let codec = match args.get(2).map(String::as_str) {
                Some("vp8") => vpx::Codec::Vp8,
                Some("vp9") => vpx::Codec::Vp9,
                other => panic!("codec vp8|vp9, got {other:?}"),
            };
            let max = args.get(4).map_or(usize::MAX, |s| s.parse().expect("max"));
            let threads = args.get(5).map_or(4, |s| s.parse().expect("threads"));
            let dump = args.get(6).map(Path::new);
            vpx::run_decode(codec, Path::new(&args[3]), max, threads, dump);
        }
        #[cfg(feature = "vpx")]
        "vpxenc" => {
            let w = args[3].parse().expect("width");
            let h = args[4].parse().expect("height");
            let frames = args[5].parse().expect("frames");
            vpx::run_encode(Path::new(&args[2]), w, h, frames, Path::new(&args[6]));
        }
        _ => panic!("usage: probe h264dec|h264enc|h264ff|av1dec|vpxdec|vpxenc ..."),
    }
}
