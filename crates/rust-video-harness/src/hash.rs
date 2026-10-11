use sha2::{Digest, Sha256};

/// SHA-256 over decoded frame bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FrameHash([u8; 32]);

impl FrameHash {
    /// Hash of raw frame bytes.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&digest);
        Self(raw)
    }

    /// Lowercase hex encoding.
    ///
    /// # Panics
    ///
    /// Never panics: writing to a `String` cannot fail.
    #[must_use]
    pub fn hex(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(64);
        for byte in &self.0 {
            write!(out, "{byte:02x}").expect("String write");
        }
        out
    }

    /// Parse lowercase hex encoding.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let raw = hex.as_bytes();
        if raw.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (slot, pair) in out.iter_mut().zip(raw.chunks_exact(2)) {
            let text = std::str::from_utf8(pair).ok()?;
            *slot = u8::from_str_radix(text, 16).ok()?;
        }
        Some(Self(out))
    }
}

#[cfg(test)]
mod tests {
    use super::FrameHash;

    #[test]
    fn of_is_deterministic() {
        assert_eq!(FrameHash::of(b"frame"), FrameHash::of(b"frame"));
    }

    #[test]
    fn of_differs_per_input() {
        assert_ne!(FrameHash::of(b"a"), FrameHash::of(b"b"));
    }

    #[test]
    fn parse_roundtrips_hex() {
        let hash = FrameHash::of(b"frame");
        assert_eq!(FrameHash::parse(&hash.hex()), Some(hash));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert_eq!(FrameHash::parse("short"), None);
        assert_eq!(FrameHash::parse(&"zz".repeat(32)), None);
        assert_eq!(FrameHash::parse(&"00".repeat(31)), None);
    }
}
