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

/// NAL unit type of `header`, H.264 7.4.1.
#[must_use]
pub const fn nal_unit_type(header: u8) -> u8 {
    header & 0x1F
}

/// NAL reference idc of `header`, H.264 7.4.1.
#[must_use]
pub const fn nal_ref_idc(header: u8) -> u8 {
    (header >> 5) & 3
}

/// True for IDR slice NAL types.
#[must_use]
pub const fn is_idr(nal_type: u8) -> bool {
    nal_type == 5
}

/// One owned NAL unit, start code stripped.
#[derive(Clone, Debug)]
pub struct Nalu {
    /// Header plus payload.
    bytes: Vec<u8>,
}

impl Nalu {
    /// Header plus payload.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Start-code scanner buffering partial NAL units across pushes.
#[derive(Default)]
pub struct NaluSplitter {
    /// Bytes not yet emitted.
    buffer: Vec<u8>,
    /// Rescan floor; keeps a split start code reparseable.
    floor: usize,
}

impl NaluSplitter {
    /// Empty splitter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            floor: 0,
        }
    }

    /// Complete NAL units out of `chunk`.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Nalu> {
        self.buffer.extend_from_slice(chunk);
        self.trim_junk();
        let mut out = Vec::new();
        self.drain_complete(&mut out);
        self.floor = self.buffer.len().saturating_sub(4);
        out
    }

    /// Trailing bytes as one final NAL unit, if any.
    pub fn flush(&mut self) -> Vec<Nalu> {
        self.trim_junk();
        let mut out = Vec::new();
        self.drain_complete(&mut out);
        if unit_contains_payload(&self.buffer) {
            out.push(Nalu {
                bytes: strip_code(std::mem::take(&mut self.buffer)),
            });
        } else {
            self.buffer.clear();
        }
        self.floor = 0;
        out
    }
}

impl NaluSplitter {
    /// Complete units out of the buffer into `out`.
    ///
    /// One split per unit: the retained tail parts first, then each
    /// cut splits from the back, so consumed bytes move once.
    fn drain_complete(&mut self, out: &mut Vec<Nalu>) {
        let mut cuts = Vec::new();
        let mut from = self.floor;
        while let Some(cut) = find_start(&self.buffer, from) {
            cuts.push(cut);
            from = cut + 3;
        }
        let Some(last) = cuts.last().copied() else {
            return;
        };
        let tail = self.buffer.split_off(last);
        let mut units = Vec::with_capacity(cuts.len());
        for cut in cuts[..cuts.len() - 1].iter().rev() {
            units.push(self.buffer.split_off(*cut));
        }
        units.push(std::mem::take(&mut self.buffer));
        for unit in units.into_iter().rev() {
            if unit_contains_payload(&unit) {
                out.push(Nalu {
                    bytes: strip_code(unit),
                });
            }
        }
        self.buffer = tail;
        self.floor = 0;
    }

    /// Drop bytes before the first start code.
    fn trim_junk(&mut self) {
        if starts_with_code(&self.buffer) {
            return;
        }
        let mut i = 0;
        while i + 2 < self.buffer.len() {
            if self.buffer[i] == 0 && self.buffer[i + 1] == 0 && self.buffer[i + 2] == 1 {
                let start = if i > 0 && self.buffer[i - 1] == 0 {
                    i - 1
                } else {
                    i
                };
                self.buffer.drain(..start);
                self.floor = 0;
                return;
            }
            i += 1;
        }
        let keep = self.buffer.len().min(3);
        self.buffer.drain(..self.buffer.len() - keep);
        self.floor = 0;
    }
}

/// Unit without its leading 3- or 4-byte start code.
fn strip_code(mut unit: Vec<u8>) -> Vec<u8> {
    let code = if unit.len() >= 4 && unit[2] == 0 {
        4
    } else {
        3
    };
    unit.drain(..code.min(unit.len()));
    unit
}

/// True when `buffer` opens with a 3- or 4-byte start code.
fn starts_with_code(buffer: &[u8]) -> bool {
    if buffer.len() >= 4 && buffer[..4] == [0, 0, 0, 1] {
        return true;
    }
    buffer.len() >= 3 && buffer[..3] == [0, 0, 1]
}

/// Offset of the next start code at or after `from`, past the one opening the unit.
fn find_start(buffer: &[u8], from: usize) -> Option<usize> {
    let mut i = from.max(3);
    while i + 2 < buffer.len() {
        if buffer[i] == 0 && buffer[i + 1] == 0 && buffer[i + 2] == 1 {
            if buffer[i - 1] == 0 {
                return Some(i - 1);
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True when `unit` holds a start code plus at least one payload byte.
fn unit_contains_payload(unit: &[u8]) -> bool {
    if unit.len() >= 4 && unit[0] == 0 && unit[1] == 0 && unit[2] == 0 && unit[3] == 1 {
        return unit.len() > 4;
    }
    if unit.len() >= 3 && unit[0] == 0 && unit[1] == 0 && unit[2] == 1 {
        return unit.len() > 3;
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
    use super::{is_idr, nal_ref_idc, nal_unit_type, rbsp, rbsp_prefix, split_nals, NaluSplitter};

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

    #[test]
    fn header_classifiers_split_idc_and_type() {
        assert_eq!(nal_unit_type(0x65), 5);
        assert_eq!(nal_ref_idc(0x65), 3);
        assert!(is_idr(nal_unit_type(0x65)));
        assert_eq!(nal_unit_type(0x41), 1);
        assert_eq!(nal_ref_idc(0x41), 2);
        assert!(!is_idr(nal_unit_type(0x41)));
        assert!(!is_idr(nal_unit_type(0x67)));
    }

    /// Payload bytes of each emitted unit.
    fn payloads(units: &[super::Nalu]) -> Vec<Vec<u8>> {
        units.iter().map(|unit| unit.bytes().to_vec()).collect()
    }

    #[test]
    fn splits_mixed_start_codes() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
        assert_eq!(payloads(&units), vec![vec![0x67, 1]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68, 2]]);
    }

    #[test]
    fn start_code_split_across_pushes_survives() {
        let mut splitter = NaluSplitter::new();
        assert!(splitter.push(&[0, 0, 0, 1, 0x67]).is_empty());
        let units = splitter.push(&[9, 0, 0]);
        assert!(units.is_empty());
        let units = splitter.push(&[1, 0x68]);
        assert_eq!(payloads(&units), vec![vec![0x67, 9]]);
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x68]]);
    }

    #[test]
    fn leading_junk_before_first_start_code_drops() {
        let mut splitter = NaluSplitter::new();
        let units = splitter.push(&[0xFF, 0xFF, 0, 0, 1, 0x65]);
        assert!(units.is_empty());
        let tail = splitter.flush();
        assert_eq!(payloads(&tail), vec![vec![0x65]]);
    }

    #[test]
    fn flush_of_empty_emits_nothing() {
        let mut splitter = NaluSplitter::default();
        assert!(splitter.flush().is_empty());
    }
}
