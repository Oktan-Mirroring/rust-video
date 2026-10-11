//! Native ffmpeg decode probe.

use std::fs;
use std::path::Path;
use std::time::Instant;

use rust_video_sw::{Codec, FfmpegDecoder};

/// Codec selector.
#[derive(Clone, Copy)]
pub enum ProbeCodec {
    /// Byte-stream input through the parser.
    AnnexB(Codec),
    /// IVF input fed frame by frame.
    Ivf(Codec),
}

/// Decode `stream` for `codec`, at most `max` frames, packed raw to `dump`.
pub fn run_codec(
    selected: ProbeCodec,
    stream: &Path,
    max: usize,
    threads: u32,
    dump: Option<&Path>,
    tag: &str,
) {
    use bytes::Bytes;
    use rust_video::packet::Packet;
    use rust_video::traits::Decoder as _;

    let inputs = match selected {
        ProbeCodec::AnnexB(_) => vec![fs::read(stream).expect("stream readable")],
        ProbeCodec::Ivf(_) => crate::common::read_ivf(stream).frames,
    };
    let inner = match selected {
        ProbeCodec::AnnexB(codec) | ProbeCodec::Ivf(codec) => codec,
    };
    println!("{tag}: {} input packets", inputs.len());
    let mut decoder = FfmpegDecoder::open(inner, threads).expect("decoder init");
    let mut packed_all = Vec::new();
    let mut frames = 0usize;
    let start = Instant::now();
    for bytes in &inputs {
        if frames >= max {
            break;
        }
        let packet = Packet::new(Bytes::from(bytes.clone()));
        for frame in decoder.decode(&packet).expect("decode") {
            if frames >= max {
                break;
            }
            for plane in frame.planes() {
                packed_all.extend_from_slice(plane.data());
            }
            frames += 1;
        }
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
        "{tag}: {frames} frames in {secs:.2}s = {:.1} fps",
        crate::common::fps(frames, secs)
    );
    if let Some(path) = dump {
        fs::write(path, &packed_all).expect("dump written");
        println!("{tag}: {} bytes dumped", packed_all.len());
    }
}
