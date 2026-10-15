//! IVF splitter shared by the probe example and the e2e suite.

use std::fs;
use std::path::Path;

/// IVF frames in file order.
pub struct Ivf {
    /// Coded frame payloads in file order.
    pub frames: Vec<Vec<u8>>,
}

/// IVF payloads from `path`.
///
/// # Panics
///
/// Panics when `path` is unreadable or fails the IVF shape.
#[must_use]
pub fn read_ivf(path: &Path) -> Ivf {
    let bytes = fs::read(path).unwrap_or_else(|_| panic!("unreadable ivf: {}", path.display()));
    assert!(bytes.len() >= 32, "short header: {}", path.display());
    assert!(&bytes[0..4] == b"DKIF", "bad magic: {}", path.display());
    let mut frames = Vec::new();
    let mut at = 32;
    while at < bytes.len() {
        assert!(
            at + 12 <= bytes.len(),
            "short frame header: {}",
            path.display()
        );
        let size = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("ivf size")) as usize;
        at += 12;
        assert!(
            at + size <= bytes.len(),
            "short frame payload: {}",
            path.display()
        );
        frames.push(bytes[at..at + size].to_vec());
        at += size;
    }
    Ivf { frames }
}
