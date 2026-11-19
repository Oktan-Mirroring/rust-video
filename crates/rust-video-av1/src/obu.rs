//! AV1 OBU ingest plus tile assembly.

use crate::bits::BitReader;
use crate::error::{Error, ParseReason, UnsupportedReason};
use crate::frame::{parse_frame_header, FrameHeader, FrameType, RefInfo};
use crate::sequence::{parse_sequence_header, SequenceHeader};
use crate::syntax::read_leb128;
use crate::{NUM_REF_FRAMES, SUPERRES_NUM};

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

/// Tile grid ceiling behind the DXVA grid.
const MAX_TILES: usize = 256;

/// One decoded tile: payload plus grid position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TileData {
    /// Tile payload bytes without the size field.
    pub bytes: Vec<u8>,
    /// Tile row.
    pub row: u16,
    /// Tile column.
    pub column: u16,
}

/// One coded frame: header plus tiles in tile order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Parsed frame header.
    pub header: FrameHeader,
    /// Tile payloads in tile order.
    pub tiles: Vec<TileData>,
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
            return Err(Error::parse(ParseReason::ObuForbiddenBit));
        }
        let unit_type = (header >> 3) & 0x0F;
        let extension = header >> 2 & 1 != 0;
        let has_size = header >> 1 & 1 != 0;
        if header & 1 != 0 {
            return Err(Error::parse(ParseReason::ObuReservedBit));
        }
        offset += 1;
        let (temporal_id, spatial_id) = if extension {
            let extension = *data.get(offset).ok_or({
                Error::parse(ParseReason::Truncated {
                    syntax: "obu_extension_header",
                })
            })?;
            if extension & 7 != 0 {
                return Err(Error::parse(ParseReason::ObuExtensionReservedBits));
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
                .map_err(|_| Error::parse(ParseReason::Range { syntax: "obu_size" }))?;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= data.len())
                .ok_or(Error::parse(ParseReason::ObuSizeExceedsPayload))?;
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
/// # Arguments
///
/// * `payload` - tile group OBU payload.
/// * `tile_count` - tiles of the frame grid.
/// * `cols_log2` - tile column shift.
/// * `rows_log2` - tile row shift.
///
/// # Errors
///
/// Returns `Err` on a range outside the grid or data short of
/// the alignment.
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
            .map_err(|_| Error::parse(ParseReason::Range { syntax: "tg_start" }))?;
        end = usize::try_from(reader.read_bits(width, "tg_end")?)
            .map_err(|_| Error::parse(ParseReason::Range { syntax: "tg_end" }))?;
        if start > end || end >= tile_count {
            return Err(Error::parse(ParseReason::Range {
                syntax: "tile group",
            }));
        }
    }
    let consumed_bits = reader.position();
    let aligned = consumed_bits.next_multiple_of(8);
    let data = payload
        .get(aligned / 8..)
        .ok_or(Error::parse(ParseReason::TileGroupWithoutData))?;
    Ok(TileGroupHead {
        start,
        end,
        consumed_bits,
        data,
    })
}

/// Byte offset and length of tiles `start` through `end` in `data`.
///
/// The last tile runs to the data end; earlier tiles carry a
/// little-endian size field of `size_bytes` bytes holding one less
/// than the tile length.
///
/// # Arguments
///
/// * `data` - tile data past the group header.
/// * `start` - first tile index of the group.
/// * `end` - last tile index of the group.
/// * `size_bytes` - tile size field width in bytes.
///
/// # Errors
///
/// Returns `Err` when a size field or tile body runs past `data`.
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
            .ok_or(Error::parse(ParseReason::Truncated {
                syntax: "tile data",
            }))?;
        if tile == end {
            tiles.push((offset, tail.len()));
            return Ok(tiles);
        }
        let size = tail
            .get(..width)
            .ok_or(Error::parse(ParseReason::Truncated {
                syntax: "tile size",
            }))?;
        let mut value: u64 = 0;
        for (index, byte) in size.iter().enumerate() {
            value |= u64::from(*byte) << (index * 8);
        }
        let length = usize::try_from(value + 1).map_err(|_| {
            Error::parse(ParseReason::Range {
                syntax: "tile size",
            })
        })?;
        let body = tail
            .get(width..)
            .and_then(|tail| tail.get(..length))
            .ok_or(Error::parse(ParseReason::Truncated {
                syntax: "tile data",
            }))?;
        tiles.push((offset + width, body.len()));
        offset = offset
            .checked_add(width)
            .and_then(|offset| offset.checked_add(length))
            .ok_or(Error::parse(ParseReason::Range {
                syntax: "tile size",
            }))?;
    }
    Ok(tiles)
}

/// OBU ingest: sequence header plus frame assembly.
pub struct Ingest {
    /// Active sequence header.
    seq: Option<SequenceHeader>,
    /// Reference slots behind header parsing.
    refs: [RefInfo; NUM_REF_FRAMES],
    /// Pending frame header.
    pending_header: Option<FrameHeader>,
    /// Pending header payload bytes behind redundant comparison.
    pending_bytes: Option<Vec<u8>>,
    /// Pending tile payloads in tile order.
    pending_tiles: Vec<TileData>,
    /// Pending tiles expected from the grid.
    pending_expected: usize,
}

impl Default for Ingest {
    /// Empty ingest.
    fn default() -> Self {
        Self::new()
    }
}

impl Ingest {
    /// Empty ingest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: None,
            refs: core::array::from_fn(|_| RefInfo::default()),
            pending_header: None,
            pending_bytes: None,
            pending_tiles: Vec::new(),
            pending_expected: 0,
        }
    }

    /// Active sequence header, if stored.
    #[must_use]
    pub fn seq(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Frames completed by `data`; trailing tiles complete the frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` on malformed OBUs, on depth and chroma outside
    /// 8-bit 4:2:0, and on spatial layers, superres upscale, render
    /// size drift, tile lists, and tile counts above 256.
    pub fn ingest_packet<E: From<Error>>(&mut self, data: &[u8]) -> Result<Vec<Frame>, E> {
        let mut frames = Vec::new();
        for obu in split_obus(data)? {
            self.ingest_obu(&obu, &mut frames)?;
        }
        if let Some(frame) = self.complete_frame()? {
            frames.push(frame);
        }
        Ok(frames)
    }

    /// Open frame, if any header arrived without a boundary.
    ///
    /// # Errors
    ///
    /// Returns `Err` when pending tiles fall short of the grid.
    pub fn finish<E: From<Error>>(&mut self) -> Result<Option<Frame>, E> {
        self.complete_frame()
    }

    /// Reference slots reset after a mid-stream failure.
    pub fn reset_refs(&mut self) {
        self.refs = core::array::from_fn(|_| RefInfo::default());
        self.pending_header = None;
        self.pending_bytes = None;
        self.pending_tiles.clear();
        self.pending_expected = 0;
    }

    /// One OBU into the ingest state.
    fn ingest_obu<E: From<Error>>(&mut self, obu: &Obu, frames: &mut Vec<Frame>) -> Result<(), E> {
        match obu.unit_type {
            OBU_SEQUENCE_HEADER => self.ingest_sequence(obu.payload),
            OBU_TEMPORAL_DELIMITER => {
                if let Some(frame) = self.complete_frame()? {
                    frames.push(frame);
                }
                Ok(())
            }
            OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER => self.ingest_frame_header(obu, frames),
            OBU_TILE_GROUP => self.ingest_tile_group(obu, frames),
            OBU_FRAME => self.ingest_frame(obu, frames),
            OBU_METADATA | OBU_PADDING => Ok(()),
            OBU_TILE_LIST => Err(Error::unsupported(UnsupportedReason::TileList)),
            _ => Err(Error::parse(ParseReason::ReservedObuType)),
        }
    }

    /// Sequence header into the store, resetting slots on change.
    fn ingest_sequence<E: From<Error>>(&mut self, payload: &[u8]) -> Result<(), E> {
        let seq = parse_sequence_header(payload)?;
        let depth = seq.bit_depth();
        if depth == 10 || depth == 12 {
            return Err(Error::unsupported(
                UnsupportedReason::Av1BitDepthNeedsTarget { depth },
            ));
        }
        if seq.mono_chrome() {
            return Err(Error::unsupported(UnsupportedReason::Av1Monochrome));
        }
        if !seq.subsampling_x() || !seq.subsampling_y() {
            return Err(Error::unsupported(UnsupportedReason::Av1ChromaOutside420));
        }
        if self.seq.as_ref() != Some(&seq) {
            self.refs = core::array::from_fn(|_| RefInfo::default());
        }
        self.seq = Some(seq);
        Ok(())
    }

    /// Frame header into the pending frame.
    fn ingest_frame_header<E: From<Error>>(
        &mut self,
        obu: &Obu,
        frames: &mut Vec<Frame>,
    ) -> Result<(), E> {
        if obu.unit_type == OBU_REDUNDANT_FRAME_HEADER {
            if let Some(previous) = &self.pending_bytes {
                if previous.as_slice() != obu.payload {
                    return Err(Error::parse(ParseReason::RedundantHeaderMismatch));
                }
                return Ok(());
            }
        }
        if self.pending_header.is_some() {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        let (header, _) = self.parse_header(obu)?;
        if header.show_existing_frame() {
            frames.push(self.show_existing_frame(&header));
            return Ok(());
        }
        self.pending_expected = usize::from(header.tile_cols) * usize::from(header.tile_rows);
        self.pending_bytes = Some(obu.payload.to_vec());
        self.pending_header = Some(header);
        Ok(())
    }

    /// Tile group appended to the pending frame.
    fn ingest_tile_group<E: From<Error>>(
        &mut self,
        obu: &Obu,
        frames: &mut Vec<Frame>,
    ) -> Result<(), E> {
        let (tile_cols, tile_rows, cols_log2, rows_log2, tile_size_bytes) = self.pending_grid()?;
        let tile_count = usize::from(tile_cols) * usize::from(tile_rows);
        let head = tile_group_range(obu.payload, tile_count, cols_log2, rows_log2)?;
        if head.start != self.pending_tiles.len() {
            return Err(Error::parse(ParseReason::TileGroupOrder));
        }
        Self::push_tiles(
            &mut self.pending_tiles,
            tile_cols,
            tile_size_bytes,
            head.data,
            head.start,
            head.end,
        )?;
        if self.pending_tiles.len() == tile_count {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        Ok(())
    }

    /// Pending tile grid geometry.
    fn pending_grid<E: From<Error>>(&self) -> Result<(u8, u8, u8, u8, u8), E> {
        let header = self
            .pending_header
            .as_ref()
            .ok_or(Error::parse(ParseReason::TileGroupWithoutFrameHeader))?;
        Ok((
            header.tile_cols,
            header.tile_rows,
            header.tile_cols_log2,
            header.tile_rows_log2,
            header.tile_size_bytes,
        ))
    }

    /// Frame OBU as one complete frame.
    fn ingest_frame<E: From<Error>>(
        &mut self,
        obu: &Obu,
        frames: &mut Vec<Frame>,
    ) -> Result<(), E> {
        if self.pending_header.is_some() {
            if let Some(frame) = self.complete_frame()? {
                frames.push(frame);
            }
        }
        let (header, consumed_bits) = self.parse_header(obu)?;
        if header.show_existing_frame() {
            frames.push(self.show_existing_frame(&header));
            return Ok(());
        }
        let consumed = consumed_bits.div_ceil(8);
        let Some(rest) = obu.payload.get(consumed..) else {
            return Err(Error::parse(ParseReason::FrameWithoutTileData));
        };
        let tile_count = usize::from(header.tile_cols) * usize::from(header.tile_rows);
        let head = tile_group_range(
            rest,
            tile_count,
            header.tile_cols_log2,
            header.tile_rows_log2,
        )?;
        self.pending_expected = tile_count;
        self.pending_bytes = Some(obu.payload.to_vec());
        Self::push_tiles(
            &mut self.pending_tiles,
            header.tile_cols,
            header.tile_size_bytes,
            head.data,
            head.start,
            head.end,
        )?;
        self.pending_header = Some(header);
        if let Some(frame) = self.complete_frame()? {
            frames.push(frame);
        }
        Ok(())
    }

    /// Frame header plus consumed bit count against the active sequence header.
    fn parse_header<E: From<Error>>(&mut self, obu: &Obu) -> Result<(FrameHeader, usize), E> {
        if obu.spatial_id != 0 {
            return Err(Error::unsupported(UnsupportedReason::SpatialLayers));
        }
        let seq = self
            .seq
            .as_ref()
            .ok_or(Error::parse(ParseReason::FrameBeforeSequenceHeader))?;
        let (header, consumed) = parse_frame_header(
            obu.payload,
            seq,
            &mut self.refs,
            obu.temporal_id,
            obu.spatial_id,
        )?;
        if header.render_and_frame_size_different()
            && (header.render_width != header.frame_width
                || header.render_height != header.frame_height)
        {
            return Err(Error::unsupported(
                UnsupportedReason::RenderSizeDiffersFromFrameSize,
            ));
        }
        if header.superres_denom != SUPERRES_NUM {
            return Err(Error::unsupported(UnsupportedReason::SuperresUpscale));
        }
        Ok((header, consumed))
    }

    /// Tiles `start` through `end` appended from `data`.
    fn push_tiles<E: From<Error>>(
        pending: &mut Vec<TileData>,
        tile_cols: u8,
        tile_size_bytes: u8,
        data: &[u8],
        start: usize,
        end: usize,
    ) -> Result<(), E> {
        let cols = usize::from(tile_cols);
        for (tile, (offset, length)) in
            (start..).zip(split_tiles(data, start, end, tile_size_bytes)?)
        {
            let row = u16::try_from(tile / cols)
                .map_err(|_| Error::parse(ParseReason::Range { syntax: "tile row" }))?;
            let column = u16::try_from(tile % cols).map_err(|_| {
                Error::parse(ParseReason::Range {
                    syntax: "tile column",
                })
            })?;
            pending.push(TileData {
                bytes: data[offset..offset + length].to_vec(),
                row,
                column,
            });
        }
        Ok(())
    }

    /// Display-only frame for an existing slot.
    fn show_existing_frame(&mut self, header: &FrameHeader) -> Frame {
        if header.frame_type == FrameType::Key {
            let shown = self.refs[usize::from(header.frame_to_show_map_idx)].clone();
            for slot in &mut self.refs {
                *slot = shown.clone();
            }
        }
        Frame {
            header: header.clone(),
            tiles: Vec::new(),
        }
    }

    /// Pending header plus tiles as one validated frame.
    fn complete_frame<E: From<Error>>(&mut self) -> Result<Option<Frame>, E> {
        let Some(header) = self.pending_header.take() else {
            return Ok(None);
        };
        self.pending_bytes = None;
        if self.pending_tiles.len() != self.pending_expected {
            self.pending_tiles.clear();
            self.pending_expected = 0;
            return Err(Error::parse(ParseReason::IncompleteTileGroup));
        }
        if self.pending_tiles.len() > MAX_TILES {
            return Err(Error::unsupported(UnsupportedReason::TileCountAbove256));
        }
        let tiles = core::mem::take(&mut self.pending_tiles);
        self.pending_expected = 0;
        self.refresh_refs(&header);
        Ok(Some(Frame { header, tiles }))
    }

    /// Reference slots refreshed from the decoded header.
    fn refresh_refs(&mut self, header: &FrameHeader) {
        for (index, slot) in self.refs.iter_mut().enumerate() {
            if header.refresh_frame_flags >> index & 1 == 0 {
                continue;
            }
            slot.valid = true;
            slot.frame_type = header.frame_type;
            slot.frame_width = header.frame_width;
            slot.frame_height = header.frame_height;
            slot.upscaled_width = header.upscaled_width;
            slot.render_width = header.render_width;
            slot.render_height = header.render_height;
            slot.order_hint = header.order_hint;
            slot.frame_id = header.current_frame_id;
            slot.loop_ref_deltas = header.loop_ref_deltas;
            slot.loop_mode_deltas = header.loop_mode_deltas;
            slot.seg_enabled = header.seg_enabled;
            slot.seg_value = header.seg_value;
            for (position, motion) in header.global_motion.iter().enumerate() {
                slot.gm_params[position] = motion.params;
            }
            slot.grain = header.film_grain;
        }
    }
}
