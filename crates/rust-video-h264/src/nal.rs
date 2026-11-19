//! Annex B framing over NAL units.

use std::borrow::Cow;

/// Sequence set count, H.264 7.4.2.1.
pub const MAX_SPS: usize = 32;

/// Picture set count, H.264 7.4.2.2.
pub const MAX_PPS: usize = 256;

/// Slice-header scan bound in RBSP bytes.
pub const HEADER_PREFIX: usize = 1024;

/// NAL units in one Annex B packet, start codes stripped.
#[must_use]
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < data.len() {
        let mut next = index;
        while next < data.len() && data[next] != 1 {
            next += 1;
        }
        if next == data.len() {
            break;
        }
        let mut zeros = 0;
        while next - zeros > index && data[next - zeros - 1] == 0 {
            zeros += 1;
        }
        if zeros >= 2 {
            let end = next - zeros;
            if end > start {
                nals.push(&data[start..end]);
            }
            start = next + 1;
        }
        index = next + 1;
    }
    if start < data.len() {
        nals.push(&data[start..]);
    }
    nals
}

/// NAL with emulation prevention bytes removed.
#[must_use]
pub fn rbsp(nal: &[u8]) -> Cow<'_, [u8]> {
    if !has_prevention(nal) {
        return Cow::Borrowed(nal);
    }
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    Cow::Owned(out)
}

/// RBSP bytes of `nal` stripped into `buf`, stopping when `buf` fills.
pub fn rbsp_prefix<'b>(nal: &[u8], buf: &'b mut [u8]) -> &'b [u8] {
    let mut zeros = 0;
    let mut len = 0;
    for &byte in nal {
        if len == buf.len() {
            break;
        }
        if zeros == 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        buf[len] = byte;
        len += 1;
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    &buf[..len]
}

/// True when `nal` holds an emulation prevention byte.
fn has_prevention(nal: &[u8]) -> bool {
    let mut zeros = 0;
    for &byte in nal {
        if zeros == 2 && byte == 3 {
            return true;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    false
}

/// Visible rectangle in luma samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Crop {
    /// Cropped luma columns on the left.
    pub left: u32,
    /// Cropped luma columns on the right.
    pub right: u32,
    /// Cropped luma rows on top.
    pub top: u32,
    /// Cropped luma rows at the bottom.
    pub bottom: u32,
}

#[cfg(test)]
mod tests {
    use super::{rbsp, rbsp_prefix, split_nals};

    #[test]
    fn split_nals_strips_start_codes() {
        let nals = split_nals(b"\0\0\0\x01\x67abc\0\0\x01\x68de");
        assert_eq!(nals, vec![b"\x67abc".as_slice(), b"\x68de".as_slice()]);
    }

    #[test]
    fn split_nals_edges_match() {
        assert_eq!(split_nals(b""), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\0\0\x01"), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\0\0\x01\0\0\x01"), [] as [&[u8]; 0]);
        assert_eq!(split_nals(b"\x01\0\0\x01"), vec![b"\x01".as_slice()]);
        assert_eq!(
            split_nals(b"\x67\0\x01\x68\0\0\0\x01\x69"),
            vec![b"\x67\0\x01\x68".as_slice(), b"\x69".as_slice()]
        );
    }

    #[test]
    fn rbsp_removes_prevention_bytes() {
        assert_eq!(
            rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 5]).into_owned(),
            vec![0, 0, 1, 0, 0, 0, 5]
        );
    }

    #[test]
    fn rbsp_borrows_without_prevention_bytes() {
        let nal = [0x65, 0x88, 0x00, 0x02];
        assert!(matches!(rbsp(&nal), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn rbsp_prefix_matches_full_strip() {
        let nal = [0x65, 0, 0, 3, 1, 0, 0, 3, 0, 5, 9, 9, 9];
        let full = rbsp(&nal).into_owned();
        let mut buf = [0u8; 8];
        assert_eq!(rbsp_prefix(&nal, &mut buf), &full[..8]);
        let mut buf = [0u8; 64];
        assert_eq!(rbsp_prefix(&nal, &mut buf), full.as_slice());
    }
}
