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
        Self { frames }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;

    use super::Ivf;

    #[test]
    fn read_returns_payloads_in_order() {
        let mut bytes = vec![0; 32];
        bytes[0..4].copy_from_slice(b"DKIF");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&8u64.to_le_bytes());
        bytes.extend_from_slice(&[0xCC]);
        let path = env::temp_dir().join(format!("rust-video-ivf-{}.ivf", std::process::id()));
        fs::write(&path, &bytes).expect("ivf writable");
        let ivf = Ivf::read(&path);
        let _removed = fs::remove_file(&path);
        assert_eq!(ivf.frames, vec![vec![0xAA, 0xBB], vec![0xCC]]);
    }
}
