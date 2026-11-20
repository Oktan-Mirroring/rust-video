//! Frequency scaling lists in zig-zag scan order.

use crate::bits::BitReader;
use crate::error::{H264Error, ParseReason};

/// Default 4x4 inter scaling list, H.264 Table 7-2, zig-zag scan order.
pub(crate) const DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34,
];

/// Default 4x4 intra scaling list, H.264 Table 7-2, zig-zag scan order.
pub(crate) const DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42,
];

/// Default 8x8 inter scaling list, H.264 Table 7-2, zig-zag scan order.
pub(crate) const DEFAULT_8X8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
    27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

/// Default 8x8 intra scaling list, H.264 Table 7-2, zig-zag scan order.
pub(crate) const DEFAULT_8X8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];

/// Scaling list presence behind one set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScalingMap {
    /// Lists present per index: bits 0..6 4x4, bits 6..12 8x8.
    pub present_mask: u16,
    /// Lists using the default matrix per index.
    pub default_mask: u16,
}

/// Scaling lists in zig-zag scan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScalingLists {
    /// Six 4x4 lists.
    pub four: [[u8; 16]; 6],
    /// Six 8x8 lists; indexes 2..6 stay flat outside 4:4:4.
    pub eight: [[u8; 64]; 6],
}

pub use self::ScalingLists as ScalingSets;

/// Flat scaling lists, H.264 7.4.2.1.1 absent-matrix inference.
pub(crate) fn fallback_scaling() -> ScalingLists {
    ScalingLists {
        four: [[16; 16]; 6],
        eight: [[16; 64]; 6],
    }
}

/// Scaling list from the reader with its default-matrix escape flag.
fn scaling_list<const N: usize, E: H264Error>(
    reader: &mut BitReader<E>,
    default: &[u8; N],
) -> Result<([u8; N], bool), E> {
    let mut zigzagged = [0u8; N];
    let mut last = 8i32;
    let mut next = 8i32;
    for slot in zigzagged.iter_mut().enumerate() {
        let (position, value) = slot;
        if next != 0 {
            let delta = reader.read_se("scaling delta")?;
            next = (last + delta).rem_euclid(256);
            if position == 0 && next == 0 {
                return Ok((*default, true));
            }
        }
        let scale = u8::try_from(if next == 0 { last } else { next }).map_err(|_| {
            E::parse(ParseReason::Range {
                syntax: "scaling value",
            })
        })?;
        *value = scale;
        last = i32::from(scale);
    }
    Ok((zigzagged, false))
}

/// Scaling lists resolved against the Table 7-2 defaults.
pub(crate) fn sequence_scaling<E: H264Error>(
    reader: &mut BitReader<E>,
    chroma: u8,
) -> Result<(ScalingLists, ScalingMap), E> {
    let mut sets = fallback_scaling();
    let mut map = ScalingMap {
        present_mask: 0,
        default_mask: 0,
    };
    for index in 0..6 {
        let (list, present) = sequence_list_4x4(reader, index, &sets.four)?;
        sets.four[index] = list;
        if let Some(escaped) = present {
            map.mark(index, escaped);
        }
    }
    let count = if chroma == 3 { 6 } else { 2 };
    for (index, list) in sets.eight.iter_mut().take(count).enumerate() {
        let (parsed, present) = sequence_list_8x8(reader, index)?;
        *list = parsed;
        if let Some(escaped) = present {
            map.mark(6 + index, escaped);
        }
    }
    Ok((sets, map))
}

/// One sequence 4x4 list: parsed, or the fallback chain value.
fn sequence_list_4x4<E: H264Error>(
    reader: &mut BitReader<E>,
    index: usize,
    four: &[[u8; 16]; 6],
) -> Result<([u8; 16], Option<bool>), E> {
    if reader.read_bit("seq_scaling_list_present_flag")? {
        let default = if index < 3 {
            &DEFAULT_4X4_INTRA
        } else {
            &DEFAULT_4X4_INTER
        };
        let (list, escaped) = scaling_list(reader, default)?;
        return Ok((list, Some(escaped)));
    }
    let list = match index {
        0 => DEFAULT_4X4_INTRA,
        3 => DEFAULT_4X4_INTER,
        _ => four[index - 1],
    };
    Ok((list, None))
}

/// One sequence 8x8 list: parsed, or the Table 7-2 default.
fn sequence_list_8x8<E: H264Error>(
    reader: &mut BitReader<E>,
    index: usize,
) -> Result<([u8; 64], Option<bool>), E> {
    if reader.read_bit("seq_scaling_list_present_flag")? {
        let default = if index.is_multiple_of(2) {
            &DEFAULT_8X8_INTRA
        } else {
            &DEFAULT_8X8_INTER
        };
        let (list, escaped) = scaling_list(reader, default)?;
        return Ok((list, Some(escaped)));
    }
    if index.is_multiple_of(2) {
        Ok((DEFAULT_8X8_INTRA, None))
    } else {
        Ok((DEFAULT_8X8_INTER, None))
    }
}

/// Scaling lists resolved against the sequence lists.
pub(crate) fn picture_scaling<E: H264Error>(
    reader: &mut BitReader<E>,
    base: &ScalingLists,
    chroma: u8,
) -> Result<(ScalingLists, ScalingMap), E> {
    let mut sets = fallback_scaling();
    let mut map = ScalingMap {
        present_mask: 0,
        default_mask: 0,
    };
    for index in 0..6 {
        let (list, present) = picture_list_4x4(reader, index, base, &sets.four)?;
        sets.four[index] = list;
        if let Some(escaped) = present {
            map.mark(index, escaped);
        }
    }
    let count = if chroma == 3 { 6 } else { 2 };
    for (index, list) in sets.eight.iter_mut().take(count).enumerate() {
        let (parsed, present) = picture_list_8x8(reader, index, base)?;
        *list = parsed;
        if let Some(escaped) = present {
            map.mark(6 + index, escaped);
        }
    }
    Ok((sets, map))
}

/// One picture 4x4 list: parsed, or the sequence-seeded fallback.
fn picture_list_4x4<E: H264Error>(
    reader: &mut BitReader<E>,
    index: usize,
    base: &ScalingLists,
    four: &[[u8; 16]; 6],
) -> Result<([u8; 16], Option<bool>), E> {
    if reader.read_bit("pic_scaling_list_present_flag")? {
        let default = if index < 3 {
            &DEFAULT_4X4_INTRA
        } else {
            &DEFAULT_4X4_INTER
        };
        let (list, escaped) = scaling_list(reader, default)?;
        return Ok((list, Some(escaped)));
    }
    let list = match index {
        0 => base.four[0],
        3 => base.four[3],
        _ => four[index - 1],
    };
    Ok((list, None))
}

/// One picture 8x8 list: parsed, or the sequence list.
fn picture_list_8x8<E: H264Error>(
    reader: &mut BitReader<E>,
    index: usize,
    base: &ScalingLists,
) -> Result<([u8; 64], Option<bool>), E> {
    if reader.read_bit("pic_scaling_list_present_flag")? {
        let default = if index.is_multiple_of(2) {
            &DEFAULT_8X8_INTRA
        } else {
            &DEFAULT_8X8_INTER
        };
        let (list, escaped) = scaling_list(reader, default)?;
        return Ok((list, Some(escaped)));
    }
    Ok((base.eight[index], None))
}

impl ScalingMap {
    /// Presence of `index` with its default-matrix escape flag.
    fn mark(&mut self, index: usize, escaped: bool) {
        self.present_mask |= 1u16 << index;
        if escaped {
            self.default_mask |= 1u16 << index;
        }
    }
}
