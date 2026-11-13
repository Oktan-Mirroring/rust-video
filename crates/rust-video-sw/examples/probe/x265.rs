//! Production x265 encoder probe.

use std::fs;
use std::path::Path;
use std::time::Instant;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::traits::Encoder as _;
use rust_video_sw::H265Encoder;

/// Encode `frames` I420 frames from `raw` at `width`x`height` into Annex B `out`.
pub fn run_encode(raw: &Path, width: u32, height: u32, frames: usize, out: &Path) {
    let pitch = width as usize * height as usize;
    let bytes = fs::read(raw).expect("raw readable");
    let size = pitch * 3 / 2;
    assert!(bytes.len() >= size * frames, "raw holds frames");
    let mut encoder = H265Encoder::open(width, height, 8_000, 4).expect("encoder init");
    let picture = PictureSize::new(width, height).expect("size");
    let mut coded = Vec::new();
    let start = Instant::now();
    for chunk in bytes.chunks_exact(size).take(frames) {
        let luma = &chunk[0..pitch];
        let chroma_u = &chunk[pitch..pitch + pitch / 4];
        let chroma_v = &chunk[pitch + pitch / 4..pitch * 3 / 2];
        let planes = [
            Plane::new(Bytes::copy_from_slice(luma), width).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_u), width / 2).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_v), width / 2).expect("plane"),
        ];
        let frame = CpuFrame::new(picture, PixelFormat::Yuv420P, planes.into()).expect("frame");
        for packet in encoder.encode(&frame).expect("encode") {
            coded.extend_from_slice(packet.data());
        }
    }
    for packet in encoder.flush().expect("flush") {
        coded.extend_from_slice(packet.data());
    }
    let secs = start.elapsed().as_secs_f64();
    fs::write(out, &coded).expect("bitstream written");
    println!(
        "x265enc: {frames} frames in {secs:.2}s = {:.1} fps, {} bytes",
        crate::common::fps(frames, secs),
        coded.len()
    );
}
