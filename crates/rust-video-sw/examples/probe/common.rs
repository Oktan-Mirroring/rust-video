//! Shared probe helpers: IVF/Annex B splitters, hashing, goldens.

#[cfg(feature = "ffmpeg")]
use std::fs;
#[cfg(feature = "ffmpeg")]
use std::path::Path;

/// IVF frames plus header geometry.
#[cfg(feature = "ffmpeg")]
pub struct Ivf {
    /// Coded frame payloads in file order.
    pub frames: Vec<Vec<u8>>,
}

/// IVF payloads and geometry from `path`.
///
/// # Panics
///
/// Panics when `path` is unreadable or fails the IVF shape.
#[cfg(feature = "ffmpeg")]
#[must_use]
pub fn read_ivf(path: &Path) -> Ivf {
    let bytes = fs::read(path).expect("ivf readable");
    assert!(bytes.len() >= 32, "ivf header");
    assert_eq!(&bytes[0..4], b"DKIF", "ivf magic");
    let mut frames = Vec::new();
    let mut at = 32usize;
    while at + 12 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("ivf size")) as usize;
        at += 12;
        assert!(at + size <= bytes.len(), "ivf frame bounds");
        frames.push(bytes[at..at + size].to_vec());
        at += size;
    }
    Ivf { frames }
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
