//! Production vpx encoder probe.

use std::fs;
use std::path::Path;
use std::time::Instant;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_sw::Vp9Encoder;

/// Encode `frames` I420 frames from `raw` at `w`x`h` into IVF `out` as VP9.
pub fn run_encode(raw: &Path, w: u32, h: u32, frames: usize, out: &Path) {
    let bytes = fs::read(raw).expect("raw readable");
    let size = w as usize * h as usize * 3 / 2;
    assert!(bytes.len() >= size * frames, "raw holds frames");
    let mut encoder = Vp9Encoder::open(w, h, 8_000, 4).expect("encoder");
    let mut coded = ivf_header(w, h);
    let picture = PictureSize::new(w, h).expect("size");
    let start = Instant::now();
    for (n, chunk) in bytes.chunks_exact(size).take(frames).enumerate() {
        let frame = split_frame(chunk, picture);
        for packet in encoder.encode(&frame).expect("encode") {
            push_packet(&mut coded, &packet, n as u64);
        }
    }
    for packet in encoder.flush().expect("flush") {
        push_packet(&mut coded, &packet, frames as u64);
    }
    let secs = start.elapsed().as_secs_f64();
    fs::write(out, &coded).expect("bitstream written");
    println!(
        "vpxenc: {frames} frames in {secs:.2}s = {:.1} fps, {} bytes",
        crate::common::fps(frames, secs),
        coded.len()
    );
}

/// CPU frame borrowing one packed I420 `chunk`.
fn split_frame(chunk: &[u8], picture: PictureSize) -> CpuFrame {
    let width = picture.width();
    let pitch = width as usize * picture.height() as usize;
    let luma = &chunk[0..pitch];
    let chroma_u = &chunk[pitch..pitch + pitch / 4];
    let chroma_v = &chunk[pitch + pitch / 4..pitch * 3 / 2];
    let planes = [
        Plane::new(Bytes::copy_from_slice(luma), width).expect("plane"),
        Plane::new(Bytes::copy_from_slice(chroma_u), width / 2).expect("plane"),
        Plane::new(Bytes::copy_from_slice(chroma_v), width / 2).expect("plane"),
    ];
    CpuFrame::new(picture, PixelFormat::Yuv420P, planes.into()).expect("frame")
}

/// One IVF frame from a coded packet.
fn push_packet(coded: &mut Vec<u8>, packet: &Packet, pts: u64) {
    let size = u32::try_from(packet.data().len()).expect("probe packet size");
    coded.extend_from_slice(&size.to_le_bytes());
    coded.extend_from_slice(&pts.to_le_bytes());
    coded.extend_from_slice(packet.data());
}

/// 32-byte IVF header for VP9 `width`x`height`.
fn ivf_header(width: u32, height: u32) -> Vec<u8> {
    let narrow = u16::try_from(width).expect("probe width");
    let short = u16::try_from(height).expect("probe height");
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"VP90");
    out.extend_from_slice(&narrow.to_le_bytes());
    out.extend_from_slice(&short.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&60u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}
