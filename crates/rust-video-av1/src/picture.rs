//! AV1 frame ingest: OBU dispatch plus tile assembly.

use crate::error::{Error, ParseKind, UnsupportedKind};
use crate::frame::{parse_frame_header, Frame, FrameHeader, FrameType, RefInfo, TileData};
use crate::framing::{
    split_obus, split_tiles, tile_group_range, Obu, OBU_FRAME, OBU_FRAME_HEADER, OBU_METADATA,
    OBU_PADDING, OBU_REDUNDANT_FRAME_HEADER, OBU_SEQUENCE_HEADER, OBU_TEMPORAL_DELIMITER,
    OBU_TILE_GROUP, OBU_TILE_LIST,
};
use crate::sequence::{parse_sequence_header, SequenceHeader};
use crate::{NUM_REF_FRAMES, SUPERRES_NUM};

/// Tile grid ceiling behind the DXVA grid.
const MAX_TILES: usize = 256;

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
            OBU_TILE_LIST => Err(Error::unsupported(UnsupportedKind::TileList)),
            _ => Err(Error::parse(ParseKind::ReservedObuType)),
        }
    }

    /// Sequence header into the store, resetting slots on change.
    fn ingest_sequence<E: From<Error>>(&mut self, payload: &[u8]) -> Result<(), E> {
        let seq = parse_sequence_header(payload)?;
        let depth = seq.bit_depth();
        if depth == 10 || depth == 12 {
            return Err(Error::unsupported(
                UnsupportedKind::Av1BitDepthNeedsTarget { depth },
            ));
        }
        if seq.mono_chrome() {
            return Err(Error::unsupported(UnsupportedKind::Av1Monochrome));
        }
        if !seq.subsampling_x() || !seq.subsampling_y() {
            return Err(Error::unsupported(UnsupportedKind::Av1ChromaOutside420));
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
                    return Err(Error::parse(ParseKind::RedundantHeaderMismatch));
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
            return Err(Error::parse(ParseKind::TileGroupOrder));
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
            .ok_or(Error::parse(ParseKind::TileGroupWithoutFrameHeader))?;
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
            return Err(Error::parse(ParseKind::FrameWithoutTileData));
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
            return Err(Error::unsupported(UnsupportedKind::SpatialLayers));
        }
        let seq = self
            .seq
            .as_ref()
            .ok_or(Error::parse(ParseKind::FrameBeforeSequenceHeader))?;
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
                UnsupportedKind::RenderSizeDiffersFromFrameSize,
            ));
        }
        if header.superres_denom != SUPERRES_NUM {
            return Err(Error::unsupported(UnsupportedKind::SuperresUpscale));
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
                .map_err(|_| Error::parse(ParseKind::Range { syntax: "tile row" }))?;
            let column = u16::try_from(tile % cols).map_err(|_| {
                Error::parse(ParseKind::Range {
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
            return Err(Error::parse(ParseKind::IncompleteTileGroup));
        }
        if self.pending_tiles.len() > MAX_TILES {
            return Err(Error::unsupported(UnsupportedKind::TileCountAbove256));
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
