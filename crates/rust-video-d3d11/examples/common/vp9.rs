//! VP9 test vectors: committed stream plus per-frame packets.
//!
//! Thin delegate over the harness [`Ivf`](rust_video_harness::Ivf) parse:
//! the d3d11 tests `#[path]`-include this file, so the names stay while the
//! single implementation lives in the harness.

use std::path::PathBuf;

/// Committed 320x240 VP9 vector path.
pub fn vector() -> PathBuf {
    rust_video_harness::decode_dir().join("valid_320x240.ivf")
}

/// Frame payloads of `data` in file order, without IVF headers.
pub fn frame_packets(data: &[u8]) -> Vec<Vec<u8>> {
    rust_video_harness::Ivf::split(data)
}
