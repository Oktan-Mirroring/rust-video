//! Annex B framing over NAL units.

use std::borrow::Cow;

/// Sequence set count, H.265 7.4.3.2.1.
pub const MAX_SPS: usize = 16;

/// Picture set count, H.265 7.4.3.3.1.
pub const MAX_PPS: usize = 64;

/// Reference set count, H.265 7.4.8.
pub const MAX_RPS: usize = 64;

/// Pictures per reference set, H.265 7.4.8.
pub const MAX_RPS_PICS: usize = 16;

/// Sequence long-term picture count, H.265 7.4.3.2.1.
pub const MAX_LT_SPS: usize = 32;

/// Slice-header scan bound in RBSP bytes.
pub const HEADER_PREFIX: usize = 1024;

/// VPS NAL type, H.265 7.4.2.2.
pub(crate) const NAL_VPS: u8 = 32;

/// SPS NAL type.
pub(crate) const NAL_SPS: u8 = 33;

/// PPS NAL type.
pub(crate) const NAL_PPS: u8 = 34;

/// Access unit delimiter NAL type.
pub(crate) const NAL_AUD: u8 = 35;

/// End-of-sequence NAL type.
pub(crate) const NAL_EOS: u8 = 36;

/// End-of-bitstream NAL type.
pub(crate) const NAL_EOB: u8 = 37;

/// Filler NAL type.
pub(crate) const NAL_FD: u8 = 38;

/// Prefix SEI NAL type.
pub(crate) const NAL_PREFIX_SEI: u8 = 39;

/// Suffix SEI NAL type.
pub(crate) const NAL_SUFFIX_SEI: u8 = 40;

/// First VCL NAL type.
pub(crate) const VCL_FIRST: u8 = 0;

/// Last VCL NAL type.
pub(crate) const VCL_LAST: u8 = 31;

/// First random access NAL type.
const IRAP_FIRST: u8 = 16;

/// Last random access NAL type.
const IRAP_LAST: u8 = 23;

/// First IDR NAL type.
const IDR_FIRST: u8 = 19;

/// Last IDR NAL type.
const IDR_LAST: u8 = 20;

/// First RASL NAL type.
const RASL_FIRST: u8 = 8;

/// Last RASL NAL type.
const RASL_LAST: u8 = 9;

/// CRA NAL type.
pub(crate) const NAL_CRA: u8 = 21;

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

/// True for random access NAL types.
#[must_use]
pub fn is_irap(nal_type: u8) -> bool {
    (IRAP_FIRST..=IRAP_LAST).contains(&nal_type)
}

/// True for IDR NAL types.
#[must_use]
pub fn is_idr(nal_type: u8) -> bool {
    (IDR_FIRST..=IDR_LAST).contains(&nal_type)
}

/// True for RASL NAL types.
#[must_use]
pub fn is_rasl(nal_type: u8) -> bool {
    (RASL_FIRST..=RASL_LAST).contains(&nal_type)
}

/// True for CRA and BLA NAL types.
#[must_use]
pub fn is_cra_bla(nal_type: u8) -> bool {
    matches!(nal_type, 16..=18 | NAL_CRA)
}

/// True for BLA NAL types.
#[must_use]
pub fn is_bla(nal_type: u8) -> bool {
    (16..=18).contains(&nal_type)
}

/// True for the CRA NAL type.
#[must_use]
pub fn is_cra(nal_type: u8) -> bool {
    nal_type == NAL_CRA
}

/// True for RADL NAL types.
#[must_use]
pub fn is_radl(nal_type: u8) -> bool {
    matches!(nal_type, 6 | 7)
}

/// True for sub-layer non-reference NAL types.
#[must_use]
pub fn is_slnr(nal_type: u8) -> bool {
    matches!(nal_type, 0 | 2 | 4 | 6 | 8 | 10 | 12 | 14)
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
