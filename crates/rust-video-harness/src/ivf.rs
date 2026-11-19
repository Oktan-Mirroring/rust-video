use std::{fs, path::Path};

/// IVF frames in file order.
pub struct Ivf {
    /// Coded frame payloads in file order.
    pub frames: Vec<Vec<u8>>,
}

impl Ivf {
    /// IVF payloads from `path`.
    ///
    /// # Panics
    ///
    /// Panics when `path` is unreadable or fails the IVF shape.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        let bytes = fs::read(path).unwrap_or_else(|_| panic!("unreadable ivf: {}", path.display()));
        Self {
            frames: Self::split(&bytes),
        }
    }

    /// IVF payloads from `data` in file order, without IVF headers.
    ///
    /// # Panics
    ///
    /// Panics when `data` fails the IVF shape.
    #[must_use]
    pub fn split(data: &[u8]) -> Vec<Vec<u8>> {
        assert!(data.len() >= 32, "short ivf header");
        assert!(&data[0..4] == b"DKIF", "bad ivf magic");
        let mut frames = Vec::new();
        let mut at = 32;
        while at < data.len() {
            assert!(at + 12 <= data.len(), "short ivf frame header");
            let size = u32::from_le_bytes(data[at..at + 4].try_into().expect("ivf size")) as usize;
            at += 12;
            assert!(at + size <= data.len(), "short ivf frame payload");
            frames.push(data[at..at + size].to_vec());
            at += size;
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;

    use super::Ivf;

    #[test]
    fn read_returns_payloads_in_order() {
        let bytes = two_frame_ivf();
        let path = env::temp_dir().join(format!("rust-video-ivf-{}.ivf", std::process::id()));
        fs::write(&path, &bytes).expect("ivf writable");
        let ivf = Ivf::read(&path);
        let _removed = fs::remove_file(&path);
        assert_eq!(ivf.frames, vec![vec![0xAA, 0xBB], vec![0xCC]]);
    }

    #[test]
    fn split_matches_read() {
        let bytes = two_frame_ivf();
        assert_eq!(Ivf::split(&bytes), vec![vec![0xAA, 0xBB], vec![0xCC]]);
    }

    /// Two-frame IVF stream: `[0xAA, 0xBB]` then `[0xCC]`.
    fn two_frame_ivf() -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[0..4].copy_from_slice(b"DKIF");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&8u64.to_le_bytes());
        bytes.extend_from_slice(&[0xCC]);
        bytes
    }
}
