//! Production AV1 decoder probe.

use std::path::Path;
use std::time::Instant;

use bytes::Bytes;
use rust_video::packet::Packet;
use rust_video::traits::Decoder;
use rust_video_sw::Av1Decoder;

use crate::common::read_ivf;

/// Decode IVF `stream` on `threads`, at most `max` frames, packed raw to `dump`.
pub fn run_decode(stream: &Path, max: usize, threads: u32, dump: Option<&Path>) {
    let ivf = read_ivf(stream);
    println!("av1dec: {} frames in stream", ivf.frames.len());
    let mut decoder = Av1Decoder::open(threads).expect("decoder init");
    let mut packed_all = Vec::new();
    let mut frames = 0usize;
    let start = Instant::now();
    for frame in &ivf.frames {
        if frames >= max {
            break;
        }
        let packet = Packet::new(Bytes::from(frame.clone()));
        for out in decoder.decode(&packet).expect("decode") {
            for plane in out.planes() {
                packed_all.extend_from_slice(plane.data());
            }
            frames += 1;
        }
    }
    for out in decoder.flush().expect("flush") {
        if frames >= max {
            break;
        }
        for plane in out.planes() {
            packed_all.extend_from_slice(plane.data());
        }
        frames += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "av1dec: {frames} frames in {secs:.2}s = {:.1} fps",
        crate::common::fps(frames, secs)
    );
    if let Some(path) = dump {
        std::fs::write(path, &packed_all).expect("dump written");
        println!("av1dec: {} bytes dumped", packed_all.len());
    }
}
