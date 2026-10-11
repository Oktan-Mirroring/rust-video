//! Native ffmpeg H.264 decode probe.

use std::fs;
use std::path::Path;
use std::time::Instant;

/// Decode Annex B `stream` whole, at most `max` frames, packed raw to `dump`.
pub fn run_decode(stream: &Path, max: usize, threads: u32, dump: Option<&Path>) {
    use bytes::Bytes;
    use rust_video::packet::Packet;
    use rust_video::traits::Decoder as _;
    use rust_video_sw::H264Decoder;

    let bytes = fs::read(stream).expect("stream readable");
    println!("h264ff: {} input bytes", bytes.len());
    let mut decoder = H264Decoder::open(threads).expect("decoder init");
    let mut packed_all = Vec::new();
    let mut frames = 0usize;
    let start = Instant::now();
    let packet = Packet::new(Bytes::from(bytes));
    for frame in decoder.decode(&packet).expect("decode") {
        if frames >= max {
            break;
        }
        for plane in frame.planes() {
            packed_all.extend_from_slice(plane.data());
        }
        frames += 1;
    }
    for frame in decoder.flush().expect("flush") {
        if frames >= max {
            break;
        }
        for plane in frame.planes() {
            packed_all.extend_from_slice(plane.data());
        }
        frames += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "h264ff: {frames} frames in {secs:.2}s = {:.1} fps",
        crate::common::fps(frames, secs)
    );
    if let Some(path) = dump {
        fs::write(path, &packed_all).expect("dump written");
        println!("h264ff: {} bytes dumped", packed_all.len());
    }
}
