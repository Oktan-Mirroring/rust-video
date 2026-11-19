//! Exp-Golomb bit reader over RBSP bytes.

use std::borrow::Cow;

use memchr::memchr_iter;

/// Raw byte sequence payload with emulation prevention removed.
pub(crate) struct Rbsp<'a> {
    /// Payload bytes after `0x03` removal, borrowed when clean.
    bytes: Cow<'a, [u8]>,
}

impl<'a> Rbsp<'a> {
    /// Payload with `0x000003` collapsed to `0x0000`.
    ///
    /// Borrows `raw` when it holds no prevention pattern.
    #[must_use]
    pub fn strip(raw: &'a [u8]) -> Self {
        let mut hits = memchr_iter(0x03, raw).filter(|index| Self::is_prevention(raw, *index));
        let Some(first) = hits.next() else {
            return Self {
                bytes: Cow::Borrowed(raw),
            };
        };
        let mut bytes = Vec::with_capacity(raw.len());
        let mut start = 0;
        let mut hit = first;
        loop {
            bytes.extend_from_slice(&raw[start..hit]);
            start = hit + 1;
            match hits.next() {
                Some(next) => hit = next,
                None => break,
            }
        }
        bytes.extend_from_slice(&raw[start..]);
        Self {
            bytes: Cow::Owned(bytes),
        }
    }

    /// True when `raw[index]` is a prevention byte after two zeros.
    fn is_prevention(raw: &[u8], index: usize) -> bool {
        index >= 2
            && raw[index - 2] == 0
            && raw[index - 1] == 0
            && (index < 3 || raw[index - 3] != 0)
    }

    /// Stripped payload bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::Rbsp;

    #[test]
    fn rbsp_collapses_prevention_bytes() {
        let stripped = Rbsp::strip(&[0, 0, 3, 0, 0, 3, 1, 0, 0, 3, 3]);
        assert_eq!(stripped.bytes(), &[0, 0, 0, 0, 1, 0, 0, 3]);
    }
}
