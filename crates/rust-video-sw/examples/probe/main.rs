//! Candidate benchmark probe: decode against goldens, encode for throughput.

#[cfg(any(feature = "h264-openh264", feature = "ffmpeg"))]
mod common;

#[cfg(feature = "h264-openh264")]
mod h264;

#[cfg(feature = "ffmpeg")]
mod ffmpeg;

#[cfg(feature = "ffmpeg")]
mod vpx;

#[cfg(any(feature = "h264-openh264", feature = "ffmpeg"))]
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
        #[cfg(feature = "h264-openh264")]
        "h264enc" => {
            let w = args[3].parse().expect("width");
            let h = args[4].parse().expect("height");
            let frames = args[5].parse().expect("frames");
            h264::run_encode(Path::new(&args[2]), w, h, frames, Path::new(&args[6]));
        }
        #[cfg(feature = "ffmpeg")]
        "ffdec" => {
            use rust_video_sw::Codec;
            let codec = match args.get(2).map(String::as_str) {
                Some("h264") => ffmpeg::ProbeCodec::AnnexB(Codec::H264),
                Some("h265") => ffmpeg::ProbeCodec::AnnexB(Codec::H265),
                Some("vp8") => ffmpeg::ProbeCodec::Ivf(Codec::Vp8),
                Some("vp9") => ffmpeg::ProbeCodec::Ivf(Codec::Vp9),
                Some("av1") => ffmpeg::ProbeCodec::Ivf(Codec::Av1),
                other => panic!("codec h264|h265|vp8|vp9|av1, got {other:?}"),
            };
            let max = args.get(4).map_or(usize::MAX, |s| s.parse().expect("max"));
            let threads = args.get(5).map_or(4, |s| s.parse().expect("threads"));
            let dump = args.get(6).map(Path::new);
            ffmpeg::run_codec(codec, Path::new(&args[3]), max, threads, dump, "ffdec");
        }
        #[cfg(feature = "ffmpeg")]
        "vpxenc" => {
            let w = args[3].parse().expect("width");
            let h = args[4].parse().expect("height");
            let frames = args[5].parse().expect("frames");
            vpx::run_encode(Path::new(&args[2]), w, h, frames, Path::new(&args[6]));
        }
        _ => panic!("usage: probe h264dec|h264enc|ffdec|vpxenc ..."),
    }
}
