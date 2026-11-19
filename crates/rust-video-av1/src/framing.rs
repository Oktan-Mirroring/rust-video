//! AV1 OBU framing plus tile assembly.

use crate::bits::BitReader;
use crate::error::{Error, ParseKind};
use crate::syntax::read_leb128;

/// Sequence header OBU type.
pub const OBU_SEQUENCE_HEADER: u8 = 1;

/// Temporal delimiter OBU type.
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;

/// Frame header OBU type.
pub const OBU_FRAME_HEADER: u8 = 3;

/// Tile group OBU type.
pub const OBU_TILE_GROUP: u8 = 4;

/// Metadata OBU type.
pub const OBU_METADATA: u8 = 5;

/// Frame OBU type.
pub const OBU_FRAME: u8 = 6;

/// Redundant frame header OBU type.
pub const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// Tile list OBU type.
pub const OBU_TILE_LIST: u8 = 8;

/// Padding OBU type.
pub const OBU_PADDING: u8 = 15;

/// Byte offset of the tile group past frame-header alignment, 5.10.
///
/// # Errors
///
/// Returns `Err` on truncated payload or invalid alignment bits.
///
/// # Arguments
///
/// * `payload` - frame OBU payload bytes.
/// * `consumed_bits` - header bits consumed by the frame parse.
pub fn frame_obu_tile_offset<E: From<Error>>(
    payload: &[u8],
    consumed_bits: usize,
) -> Result<usize, E> {
    let total_bits = payload.len().saturating_mul(8);
    if consumed_bits >= total_bits {
        return Err(Error::parse(ParseKind::Truncated {
            syntax: "alignment_bit",
        }));
    }
    let end = consumed_bits / 8 * 8 + 8;
    if end > total_bits {
        return Err(Error::parse(ParseKind::Truncated {
            syntax: "alignment_bit",
        }));
    }
    if payload[consumed_bits / 8] & (0x80 >> (consumed_bits % 8)) == 0 {
        return Err(Error::parse(ParseKind::Range {
            syntax: "alignment_bit",
        }));
    }
    for bit in consumed_bits + 1..end {
        if payload[bit / 8] & (0x80 >> (bit % 8)) != 0 {
            return Err(Error::parse(ParseKind::Range {
                syntax: "alignment_bit",
            }));
        }
    }
    Ok(end / 8)
}

/// OBU header with its payload span.
pub struct Obu<'a> {
    /// OBU type.
    pub unit_type: u8,
    /// Temporal id.
    pub temporal_id: u8,
    /// Spatial id.
    pub spatial_id: u8,
    /// Payload bytes.
    pub payload: &'a [u8],
}

/// Payload OBUs of `data` in order.
///
/// # Errors
///
/// Returns `Err` on forbidden or reserved header bits, a short
/// extension header, or a size field past the payload.
pub fn split_obus<E: From<Error>>(data: &[u8]) -> Result<Vec<Obu<'_>>, E> {
    let mut obus = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let header = data[offset];
        if header >> 7 != 0 {
            return Err(Error::parse(ParseKind::ObuForbiddenBit));
        }
        let unit_type = (header >> 3) & 0x0F;
        let extension = header >> 2 & 1 != 0;
        let has_size = header >> 1 & 1 != 0;
        if header & 1 != 0 {
            return Err(Error::parse(ParseKind::ObuReservedBit));
        }
        offset += 1;
        let (temporal_id, spatial_id) = if extension {
            let extension = *data.get(offset).ok_or({
                Error::parse(ParseKind::Truncated {
                    syntax: "obu_extension_header",
                })
            })?;
            if extension & 7 != 0 {
                return Err(Error::parse(ParseKind::ObuExtensionReservedBits));
            }
            offset += 1;
            (extension >> 5, (extension >> 3) & 3)
        } else {
            (0, 0)
        };
        let payload = if has_size {
            let mut size_reader = BitReader::new(&data[offset..]);
            let size = read_leb128(&mut size_reader, "obu_size")?;
            let consumed = size_reader.position() / 8;
            offset += consumed;
            let size = usize::try_from(size)
                .map_err(|_| Error::parse(ParseKind::Range { syntax: "obu_size" }))?;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= data.len())
                .ok_or(Error::parse(ParseKind::ObuSizeExceedsPayload))?;
            let payload = &data[offset..end];
            offset = end;
            payload
        } else {
            let payload = &data[offset..];
            offset = data.len();
            payload
        };
        obus.push(Obu {
            unit_type,
            temporal_id,
            spatial_id,
            payload,
        });
    }
    Ok(obus)
}

/// Tile group header: tile range plus data past the alignment.
pub struct TileGroupHead<'a> {
    /// First tile index of the group.
    pub start: usize,
    /// Last tile index of the group.
    pub end: usize,
    /// Header bits consumed before alignment.
    pub consumed_bits: usize,
    /// Tile data past the group header.
    pub data: &'a [u8],
}

/// Tile range plus data behind the tile group header.
///
/// # Errors
///
/// Returns `Err` on a range outside the grid or data short of
/// the alignment.
///
/// # Arguments
///
/// * `payload` - tile group OBU payload.
/// * `tile_count` - tiles of the frame grid.
/// * `cols_log2` - tile column shift.
/// * `rows_log2` - tile row shift.
pub fn tile_group_range<E: From<Error>>(
    payload: &[u8],
    tile_count: usize,
    cols_log2: u8,
    rows_log2: u8,
) -> Result<TileGroupHead<'_>, E> {
    let mut reader = BitReader::new(payload);
    let mut start = 0usize;
    let mut end = tile_count - 1;
    if tile_count > 1 && reader.read_bit("tile_start_and_end_present_flag")? {
        let width = u32::from(cols_log2) + u32::from(rows_log2);
        start = usize::try_from(reader.read_bits(width, "tg_start")?)
            .map_err(|_| Error::parse(ParseKind::Range { syntax: "tg_start" }))?;
        end = usize::try_from(reader.read_bits(width, "tg_end")?)
            .map_err(|_| Error::parse(ParseKind::Range { syntax: "tg_end" }))?;
        if start > end || end >= tile_count {
            return Err(Error::parse(ParseKind::Range {
                syntax: "tile group",
            }));
        }
    }
    let consumed_bits = reader.position();
    let aligned = consumed_bits.next_multiple_of(8);
    let data = payload
        .get(aligned / 8..)
        .ok_or(Error::parse(ParseKind::TileGroupWithoutData))?;
    Ok(TileGroupHead {
        start,
        end,
        consumed_bits,
        data,
    })
}

/// Byte offset and length of tiles `start` through `end` in `data`.
///
/// # Errors
///
/// Returns `Err` when a size field or tile body runs past `data`.
///
/// # Arguments
///
/// * `data` - tile data past the group header.
/// * `start` - first tile index of the group.
/// * `end` - last tile index of the group.
/// * `size_bytes` - little-endian tile size field width in bytes; each
///   holds one less than the tile length.
///
/// # Returns
///
/// Offsets with lengths; the last tile runs to the data end.
pub fn split_tiles<E: From<Error>>(
    data: &[u8],
    start: usize,
    end: usize,
    size_bytes: u8,
) -> Result<Vec<(usize, usize)>, E> {
    let mut tiles = Vec::new();
    let mut offset = 0usize;
    let width = usize::from(size_bytes);
    for tile in start..=end {
        let tail = data
            .get(offset..)
            .ok_or(Error::parse(ParseKind::Truncated {
                syntax: "tile data",
            }))?;
        if tile == end {
            tiles.push((offset, tail.len()));
            return Ok(tiles);
        }
        let size = tail.get(..width).ok_or(Error::parse(ParseKind::Truncated {
            syntax: "tile size",
        }))?;
        let mut value: u64 = 0;
        for (index, byte) in size.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        let length = usize::try_from(value + 1).map_err(|_| {
            Error::parse(ParseKind::Range {
                syntax: "tile size",
            })
        })?;
        let body = tail
            .get(width..)
            .and_then(|tail| tail.get(..length))
            .ok_or(Error::parse(ParseKind::Truncated {
                syntax: "tile data",
            }))?;
        tiles.push((offset + width, body.len()));
        offset = offset
            .checked_add(width)
            .and_then(|offset| offset.checked_add(length))
            .ok_or(Error::parse(ParseKind::Range {
                syntax: "tile size",
            }))?;
    }
    Ok(tiles)
}
