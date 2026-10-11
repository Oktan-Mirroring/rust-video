use std::{fmt, fs, path::Path};

use crate::{Error, FrameHash};

/// Expected frame hashes for one case.
pub struct Golden {
    hashes: Vec<FrameHash>,
}

impl Golden {
    /// Load one lowercase hex hash per line.
    ///
    /// Blank lines and `#` comments are skipped.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the file is unreadable or a line is malformed.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path)?;
        Self::parse(&text).ok_or_else(|| Error::Golden(format!("malformed: {}", path.display())))
    }

    /// Expected frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// True when no frames are expected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// Compare decoded hashes in output order.
    #[must_use]
    pub fn check(&self, actual: &[FrameHash]) -> Vec<Mismatch> {
        let longest = self.hashes.len().max(actual.len());
        let mut gaps = Vec::new();
        for index in 0..longest {
            let expected = self.hashes.get(index).copied();
            let got = actual.get(index).copied();
            if expected != got {
                gaps.push(Mismatch {
                    index,
                    expected,
                    actual: got,
                });
            }
        }
        gaps
    }

    fn parse(text: &str) -> Option<Self> {
        let mut hashes = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            hashes.push(FrameHash::parse(line)?);
        }
        Some(Self { hashes })
    }
}

/// One divergent frame.
pub struct Mismatch {
    index: usize,
    expected: Option<FrameHash>,
    actual: Option<FrameHash>,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show =
            |hash: Option<FrameHash>| hash.map_or_else(|| "-".to_owned(), |found| found.hex());
        write!(
            f,
            "frame {}: expected {}, got {}",
            self.index,
            show(self.expected),
            show(self.actual)
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::FrameHash;

    use super::Golden;

    #[test]
    fn parse_skips_blanks_and_comments() {
        let hash = FrameHash::of(b"frame").hex();
        let text = format!("# golden\n\n{hash}\n\n");
        let golden = Golden::parse(&text).expect("valid");
        assert_eq!(golden.len(), 1);
    }

    #[test]
    fn parse_rejects_malformed_line() {
        assert!(Golden::parse("not-hex\n").is_none());
    }

    #[test]
    fn check_reports_divergent_index() {
        let text = format!("{}\n", FrameHash::of(b"a").hex());
        let golden = Golden::parse(&text).expect("valid");
        assert!(golden.check(&[FrameHash::of(b"a")]).is_empty());
        let gaps = golden.check(&[FrameHash::of(b"b")]);
        assert_eq!(gaps.len(), 1);
    }

    #[test]
    fn check_reports_length_divergence() {
        let text = format!("{}\n", FrameHash::of(b"a").hex());
        let golden = Golden::parse(&text).expect("valid");
        assert_eq!(golden.check(&[]).len(), 1);
        assert_eq!(
            golden
                .check(&[FrameHash::of(b"a"), FrameHash::of(b"b")])
                .len(),
            1
        );
    }
}
