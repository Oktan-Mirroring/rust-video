//! Shared probe helpers: Annex B splitter, timing reports, packing.

use std::fs;
use std::path::Path;

/// Timed-loop report plus optional raw `dump` write.
pub fn report(tag: &str, frames: usize, secs: f64, packed: &[u8], dump: Option<&Path>) {
    println!(
        "{tag}: {frames} frames in {secs:.2}s = {:.1} fps",
        fps(frames, secs)
    );
    if let Some(path) = dump {
        fs::write(path, packed).expect("dump written");
        println!("{tag}: {} bytes dumped", packed.len());
    }
}

/// NAL units from Annex B `bytes`, start codes kept.
#[cfg(feature = "h264-openh264")]
#[must_use]
pub fn split_nalus(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        if bytes[i] == 0
            && bytes[i + 1] == 0
            && (bytes[i + 2] == 1 || (bytes[i + 2] == 0 && bytes[i + 3] == 1))
        {
            let len = if bytes[i + 2] == 1 { 3 } else { 4 };
            starts.push(i);
            i += len;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, at)| {
            let end = starts.get(n + 1).copied().unwrap_or(bytes.len());
            bytes[*at..end].to_vec()
        })
        .filter(|nal| nal.len() > 4)
        .collect()
}

/// Frames per second from a frame count and elapsed seconds.
///
/// # Panics
///
/// Panics when `frames` exceeds `u32`.
#[must_use]
pub fn fps(frames: usize, secs: f64) -> f64 {
    let count = u32::try_from(frames).expect("probe frame count");
    f64::from(count) / secs
}

/// Tightly packed I420 bytes from strided planes.
#[cfg(feature = "h264-openh264")]
#[must_use]
pub fn pack_i420(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
    y_stride: usize,
    c_stride: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 3 / 2);
    for row in 0..height {
        out.extend_from_slice(&y[row * y_stride..row * y_stride + width]);
    }
    for row in 0..height / 2 {
        out.extend_from_slice(&u[row * c_stride..row * c_stride + width / 2]);
    }
    for row in 0..height / 2 {
        out.extend_from_slice(&v[row * c_stride..row * c_stride + width / 2]);
    }
    out
}
