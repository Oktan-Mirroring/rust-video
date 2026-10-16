//! File input and frame printing for the VA-API examples.

use std::path::Path;

#[path = "../../tests/support/mp4.rs"]
mod mp4;

/// Samples of `path`: MP4 demuxed, else raw Annex B as one sample.
///
/// Exits the process when the file cannot be read.
pub fn samples(tool: &str, path: &str) -> Vec<Vec<u8>> {
    match mp4::demux(Path::new(path)) {
        Ok(stream) => stream.samples,
        Err(_) => match std::fs::read(path) {
            Ok(bytes) => vec![bytes],
            Err(error) => {
                eprintln!("{tool}: cannot read {path} ({error})");
                std::process::exit(1);
            }
        },
    }
}
