//! VP9 frame ingest: uncompressed headers plus tile assembly.

//! Reference sizes behind frame-size inheritance; tiers own surfaces.

use std::marker::PhantomData;

use crate::bits::BitReader;
use crate::error::Error;
use crate::frame::{
    Frame, FrameHeader, FrameType, TileData, FH_ERROR_RESILIENT, FH_FRAME_PARALLEL,
    FH_HIGH_PRECISION_MV, FH_INTRA_ONLY, FH_LF_DELTA_ENABLED, FH_LF_DELTA_UPDATE,
    FH_REFRESH_CONTEXT, FH_RENDER_DIFFERS, FH_SEG_ABS_DELTA, FH_SEG_ENABLED,
    FH_SEG_TEMPORAL_UPDATE, FH_SEG_UPDATE_DATA, FH_SEG_UPDATE_MAP, FH_SHOW_EXISTING, FH_SHOW_FRAME,
    FH_SUBSAMPLING_X, FH_SUBSAMPLING_Y, INTERP_SWITCHABLE, MAX_SEGMENTS, NUM_REF_FRAMES,
    PRED_PROBS, REFS_PER_FRAME, SEG_LEVELS, TREE_PROBS,
};
use crate::framing::split_superframe;

/// Frame marker bits opening every frame.
const FRAME_MARKER: u32 = 2;

/// Keyframe sync code.
const SYNC_CODE: u32 = 0x49_83_42;

/// Smallest tile width in superblocks.
const MIN_TILE_WIDTH_SB: u32 = 4;

/// Largest tile width in superblocks.
const MAX_TILE_WIDTH_SB: u32 = 64;

/// Tile column shift ceiling.
const MAX_TILE_COLS_LOG2: u8 = 6;

/// Tile row shift ceiling.
const MAX_TILE_ROWS_LOG2: u8 = 2;

/// Literal to filter id per S6.2.
const INTERP_LITERAL: [u8; 4] = [1, 0, 2, 3];

/// sRGB color space id.
const COLOR_SRGB: u8 = 7;

/// BT.601 color space id behind profile 0 intra-only frames.
const COLOR_BT601: u8 = 1;

/// Persistent loop filter state across frames.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LoopFilter {
    /// Filter level.
    level: u8,
    /// Sharpness level.
    sharpness: u8,
    /// Mode and reference deltas apply.
    delta_enabled: bool,
    /// Deltas update this frame.
    delta_update: bool,
    /// Reference deltas.
    ref_deltas: [i8; 4],
    /// Mode deltas.
    mode_deltas: [i8; 2],
}

impl LoopFilter {
    /// Past-independence defaults behind intra frames.
    fn reset_past(&mut self) {
        self.delta_enabled = true;
        self.ref_deltas = [1, 0, -1, -1];
        self.mode_deltas = [0, 0];
    }

    /// Loop filter parameters parsed from `reader`.
    fn parse<E: From<Error>>(&mut self, reader: &mut BitReader) -> Result<(), E> {
        self.level = read_small(reader, 6, "loop_filter_level")?;
        self.sharpness = read_small(reader, 3, "loop_filter_sharpness")?;
        self.delta_enabled = reader.read_bit("loop_filter_delta_enabled")?;
        self.delta_update = false;
        if !self.delta_enabled {
            return Ok(());
        }
        self.delta_update = reader.read_bit("loop_filter_delta_update")?;
        if !self.delta_update {
            return Ok(());
        }
        for delta in &mut self.ref_deltas {
            if reader.read_bit("loop_filter_ref_deltas")? {
                *delta = read_delta(reader, 6, "loop_filter_ref_deltas")?;
            }
        }
        for delta in &mut self.mode_deltas {
            if reader.read_bit("loop_filter_mode_deltas")? {
                *delta = read_delta(reader, 6, "loop_filter_mode_deltas")?;
            }
        }
        Ok(())
    }
}

/// Persistent segmentation state across frames.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Segmentation {
    /// Packed segmentation flags.
    flags: u8,
    /// Tree probabilities.
    tree_probs: [u8; TREE_PROBS],
    /// Prediction probabilities.
    pred_probs: [u8; PRED_PROBS],
    /// Feature enables per segment and level.
    feature_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Feature values per segment and level.
    feature_data: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
}

impl Segmentation {
    /// Segmentation applies flag bit.
    const ENABLED: u8 = 1;

    /// Map update flag bit.
    const UPDATE_MAP: u8 = 1 << 1;

    /// Temporal update flag bit.
    const TEMPORAL_UPDATE: u8 = 1 << 2;

    /// Feature data update flag bit.
    const UPDATE_DATA: u8 = 1 << 3;

    /// Absolute values flag bit.
    const ABS_DELTA: u8 = 1 << 4;

    /// Feature value widths per level.
    const FEATURE_BITS: [u32; SEG_LEVELS] = [8, 6, 2, 0];

    /// Feature sign bits per level.
    const FEATURE_SIGNED: [bool; SEG_LEVELS] = [true, true, false, false];

    /// Segmentation applies.
    fn enabled(&self) -> bool {
        self.flags & Self::ENABLED != 0
    }

    /// Map updates this frame.
    fn update_map(&self) -> bool {
        self.flags & Self::UPDATE_MAP != 0
    }

    /// Map updates against the previous map.
    fn temporal_update(&self) -> bool {
        self.flags & Self::TEMPORAL_UPDATE != 0
    }

    /// Feature data updates this frame.
    fn update_data(&self) -> bool {
        self.flags & Self::UPDATE_DATA != 0
    }

    /// Feature data holds absolute values.
    fn abs_delta(&self) -> bool {
        self.flags & Self::ABS_DELTA != 0
    }

    /// Flag `flag` set to `on`.
    fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
    }

    /// Past-independence defaults behind intra frames.
    fn reset_past(&mut self) {
        self.feature_enabled = [[false; SEG_LEVELS]; MAX_SEGMENTS];
        self.feature_data = [[0; SEG_LEVELS]; MAX_SEGMENTS];
        self.set(Self::ABS_DELTA, false);
    }

    /// Segmentation parameters parsed from `reader`.
    fn parse<E: From<Error>>(&mut self, reader: &mut BitReader) -> Result<(), E> {
        self.set(Self::UPDATE_MAP, false);
        self.set(Self::UPDATE_DATA, false);
        self.set(Self::ENABLED, reader.read_bit("segmentation_enabled")?);
        if !self.enabled() {
            return Ok(());
        }
        self.set(
            Self::UPDATE_MAP,
            reader.read_bit("segmentation_update_map")?,
        );
        if self.update_map() {
            for prob in &mut self.tree_probs {
                *prob = read_prob(reader, "segmentation_tree_probs")?;
            }
            self.set(
                Self::TEMPORAL_UPDATE,
                reader.read_bit("segmentation_temporal_update")?,
            );
            let temporal = self.temporal_update();
            for prob in &mut self.pred_probs {
                *prob = if temporal {
                    read_prob(reader, "segmentation_pred_probs")?
                } else {
                    255
                };
            }
        }
        self.set(
            Self::UPDATE_DATA,
            reader.read_bit("segmentation_update_data")?,
        );
        if !self.update_data() {
            return Ok(());
        }
        self.set(
            Self::ABS_DELTA,
            reader.read_bit("segmentation_abs_or_delta_update")?,
        );
        for segment in 0..MAX_SEGMENTS {
            for level in 0..SEG_LEVELS {
                let on = reader.read_bit("segmentation_feature_enabled")?;
                self.feature_enabled[segment][level] = on;
                if !on {
                    continue;
                }
                let mut value = i16::try_from(
                    reader.read_bits(Self::FEATURE_BITS[level], "segmentation_feature_data")?,
                )
                .map_err(|_| {
                    E::from(Error::Range {
                        syntax: "segmentation_feature_data",
                    })
                })?;
                if Self::FEATURE_SIGNED[level] && reader.read_bit("segmentation_feature_sign")? {
                    value = -value;
                }
                self.feature_data[segment][level] = value;
            }
        }
        Ok(())
    }
}

/// VP9 frame ingest: uncompressed headers plus tile assembly.
pub struct Ingest<E = Error> {
    /// Cached bit depth behind inter frames.
    bit_depth: u8,
    /// Cached horizontal subsampling behind inter frames.
    subsampling_x: bool,
    /// Cached vertical subsampling behind inter frames.
    subsampling_y: bool,
    /// Cached color space behind inter frames.
    color_space: u8,
    /// Cached color range behind inter frames.
    color_range: u8,
    /// Superblock columns behind tile info.
    sb64_cols: u32,
    /// Persistent loop filter state.
    lf: LoopFilter,
    /// Persistent segmentation state.
    seg: Segmentation,
    /// Reference frame sizes by slot.
    ref_dims: [(u32, u32); NUM_REF_FRAMES],
    /// Error type behind generic parsing.
    error: PhantomData<E>,
}

impl<E> Ingest<E> {
    /// Empty ingest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bit_depth: 8,
            subsampling_x: true,
            subsampling_y: true,
            color_space: 0,
            color_range: 0,
            sb64_cols: 0,
            lf: LoopFilter::default(),
            seg: Segmentation::default(),
            ref_dims: [(0, 0); NUM_REF_FRAMES],
            error: PhantomData,
        }
    }

    /// Reference state reset after a mid-stream failure.
    pub fn reset_refs(&mut self) {
        *self = Self::new();
    }
}

impl<E> Default for Ingest<E> {
    /// Empty ingest.
    fn default() -> Self {
        Self::new()
    }
}

impl<E: From<Error>> Ingest<E> {
    /// Frames completed by `data`; every frame completes inline.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `data` violates the VP9 grammar; each [`Error`] converts into `E`.
    pub fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Frame>, E> {
        let mut frames = Vec::new();
        for coded in split_superframe(data)? {
            frames.push(self.parse_frame(coded)?);
        }
        Ok(frames)
    }

    /// Open frame, if any; inline completion buffers none.
    ///
    /// # Errors
    ///
    /// Infallible; `Ok` lets tiers flush every codec through one call.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub fn finish(&mut self) -> Result<Option<Frame>, E> {
        Ok(None)
    }

    /// Frame parsed from `data` with tiles split.
    fn parse_frame(&mut self, data: &[u8]) -> Result<Frame, E> {
        let mut reader = BitReader::new(data);
        let mut header = FrameHeader::default();
        if reader.read_bits(2, "frame_marker")? != FRAME_MARKER {
            return Err(E::from(Error::FrameMarkerMismatch));
        }
        header.profile = parse_profile(&mut reader)?;
        if reader.read_bit("show_existing_frame")? {
            header.set(FH_SHOW_EXISTING, true);
            header.frame_to_show_map_idx = read_small(&mut reader, 3, "frame_to_show_map_idx")?;
            return Ok(Frame {
                header,
                bytes: data.to_vec(),
                tiles: Vec::new(),
            });
        }
        header.frame_type = if reader.read_bit("frame_type")? {
            FrameType::Inter
        } else {
            FrameType::Key
        };
        header.set(FH_SHOW_FRAME, reader.read_bit("show_frame")?);
        header.set(FH_ERROR_RESILIENT, reader.read_bit("error_resilient_mode")?);
        if header.frame_type.is_intra() {
            self.parse_key(&mut reader, &mut header)?;
        } else {
            self.parse_inter(&mut reader, &mut header)?;
        }
        if header.error_resilient_mode() {
            header.set(FH_FRAME_PARALLEL, true);
        } else {
            header.set(
                FH_REFRESH_CONTEXT,
                reader.read_bit("refresh_frame_context")?,
            );
            header.set(
                FH_FRAME_PARALLEL,
                reader.read_bit("frame_parallel_decoding_mode")?,
            );
        }
        header.frame_context_idx = read_small(&mut reader, 2, "frame_context_idx")?;
        if header.frame_type.is_intra() || header.intra_only() || header.error_resilient_mode() {
            self.setup_past_independence(&mut header);
        }
        self.lf.parse(&mut reader)?;
        header.filter_level = self.lf.level;
        header.sharpness = self.lf.sharpness;
        header.set(FH_LF_DELTA_ENABLED, self.lf.delta_enabled);
        header.set(FH_LF_DELTA_UPDATE, self.lf.delta_update);
        header.ref_deltas = self.lf.ref_deltas;
        header.mode_deltas = self.lf.mode_deltas;
        parse_quantization(&mut reader, &mut header)?;
        self.seg.parse(&mut reader)?;
        header.set(FH_SEG_ENABLED, self.seg.enabled());
        header.set(FH_SEG_UPDATE_MAP, self.seg.update_map());
        header.set(FH_SEG_TEMPORAL_UPDATE, self.seg.temporal_update());
        header.set(FH_SEG_UPDATE_DATA, self.seg.update_data());
        header.set(FH_SEG_ABS_DELTA, self.seg.abs_delta());
        header.tree_probs = self.seg.tree_probs;
        header.pred_probs = self.seg.pred_probs;
        header.seg_enabled = self.seg.feature_enabled;
        header.seg_value = self.seg.feature_data;
        self.parse_tile_info(&mut reader, &mut header)?;
        header.header_size_in_bytes = u16::try_from(reader.read_bits(16, "header_size_in_bytes")?)
            .map_err(|_| {
                E::from(Error::Range {
                    syntax: "header_size_in_bytes",
                })
            })?;
        header.uncompressed_size = u16::try_from(reader.position().div_ceil(8)).map_err(|_| {
            E::from(Error::Range {
                syntax: "uncompressed_header",
            })
        })?;
        self.refresh_refs(&header);
        let tiles = split_tiles(data, &header)?;
        Ok(Frame {
            header,
            bytes: data.to_vec(),
            tiles,
        })
    }

    /// Keyframe sync code, color, size, and render size.
    fn parse_key(&mut self, reader: &mut BitReader, header: &mut FrameHeader) -> Result<(), E> {
        parse_sync_code(reader)?;
        self.parse_color_config(reader, header)?;
        self.parse_frame_size(reader, header)?;
        parse_render_size(reader, header)?;
        header.refresh_frame_flags = 0xFF;
        Ok(())
    }

    /// Inter frame references, size, and prediction tools.
    fn parse_inter(&mut self, reader: &mut BitReader, header: &mut FrameHeader) -> Result<(), E> {
        if !header.show_frame() {
            header.set(FH_INTRA_ONLY, reader.read_bit("intra_only")?);
        }
        header.reset_frame_context = if header.error_resilient_mode() {
            0
        } else {
            read_small(reader, 2, "reset_frame_context")?
        };
        if header.intra_only() {
            parse_sync_code(reader)?;
            if header.profile == 0 {
                header.bit_depth = 8;
                header.color_space = COLOR_BT601;
                header.set(FH_SUBSAMPLING_X, true);
                header.set(FH_SUBSAMPLING_Y, true);
                self.cache_color(header);
            } else {
                self.parse_color_config(reader, header)?;
            }
            header.refresh_frame_flags = read_small(reader, 8, "refresh_frame_flags")?;
            self.parse_frame_size(reader, header)?;
            parse_render_size(reader, header)?;
            return Ok(());
        }
        header.bit_depth = self.bit_depth;
        header.color_space = self.color_space;
        header.color_range = self.color_range;
        header.set(FH_SUBSAMPLING_X, self.subsampling_x);
        header.set(FH_SUBSAMPLING_Y, self.subsampling_y);
        header.refresh_frame_flags = read_small(reader, 8, "refresh_frame_flags")?;
        for position in 0..REFS_PER_FRAME {
            header.ref_frame_idx[position] = read_small(reader, 3, "ref_frame_idx")?;
            header.ref_frame_sign_bias[position + 1] =
                read_small(reader, 1, "ref_frame_sign_bias")?;
        }
        self.parse_frame_size_with_refs(reader, header)?;
        header.set(
            FH_HIGH_PRECISION_MV,
            reader.read_bit("allow_high_precision_mv")?,
        );
        header.interp_filter = if reader.read_bit("is_filter_switchable")? {
            INTERP_SWITCHABLE
        } else {
            let literal = reader.read_bits(2, "interp_filter")?;
            INTERP_LITERAL[usize::try_from(literal).map_err(|_| {
                E::from(Error::Range {
                    syntax: "interp_filter",
                })
            })?]
        };
        Ok(())
    }

    /// Color config into `header` and the inter cache.
    fn parse_color_config(
        &mut self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), E> {
        header.bit_depth = if header.profile >= 2 {
            if reader.read_bit("ten_or_twelve_bit")? {
                12
            } else {
                10
            }
        } else {
            8
        };
        header.color_space = read_small(reader, 3, "color_space")?;
        if header.color_space == COLOR_SRGB {
            header.color_range = 1;
            if header.profile & 1 == 1 {
                header.set(FH_SUBSAMPLING_X, false);
                header.set(FH_SUBSAMPLING_Y, false);
                let _reserved = reader.read_bit("color_config_reserved")?;
            } else {
                header.set(FH_SUBSAMPLING_X, true);
                header.set(FH_SUBSAMPLING_Y, true);
            }
        } else {
            header.color_range = read_small(reader, 1, "color_range")?;
            if header.profile & 1 == 1 {
                header.set(FH_SUBSAMPLING_X, reader.read_bit("subsampling_x")?);
                header.set(FH_SUBSAMPLING_Y, reader.read_bit("subsampling_y")?);
                let _reserved = reader.read_bit("color_config_reserved")?;
            } else {
                header.set(FH_SUBSAMPLING_X, true);
                header.set(FH_SUBSAMPLING_Y, true);
            }
        }
        self.cache_color(header);
        Ok(())
    }

    /// Explicit frame dimensions plus superblock geometry.
    fn parse_frame_size(
        &mut self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), E> {
        header.frame_width = reader.read_bits(16, "frame_width_minus_1")? + 1;
        header.frame_height = reader.read_bits(16, "frame_height_minus_1")? + 1;
        self.size_superblocks(header);
        Ok(())
    }

    /// Inter frame size from a reference slot or explicit dimensions.
    fn parse_frame_size_with_refs(
        &mut self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), E> {
        for position in 0..REFS_PER_FRAME {
            if !reader.read_bit("found_ref")? {
                continue;
            }
            let slot = usize::from(header.ref_frame_idx[position]);
            let (width, height) = self.ref_dims[slot];
            if width == 0 || height == 0 {
                return Err(E::from(Error::NamesEmptySlot {
                    reference: "ref_frame_idx",
                }));
            }
            header.frame_width = width;
            header.frame_height = height;
            self.size_superblocks(header);
            return parse_render_size(reader, header);
        }
        self.parse_frame_size(reader, header)?;
        parse_render_size(reader, header)
    }

    /// Tile grid from superblock geometry.
    fn parse_tile_info(&self, reader: &mut BitReader, header: &mut FrameHeader) -> Result<(), E> {
        let max_cols = max_log2_tile_cols(self.sb64_cols);
        let mut cols = min_log2_tile_cols(self.sb64_cols);
        while cols < max_cols {
            if reader.read_bit("increment_tile_cols_log2")? {
                cols += 1;
            } else {
                break;
            }
        }
        if cols > MAX_TILE_COLS_LOG2 {
            return Err(E::from(Error::Range {
                syntax: "tile_cols_log2",
            }));
        }
        header.tile_cols_log2 = cols;
        header.tile_rows_log2 = read_small(reader, 1, "tile_rows_log2")?;
        if header.tile_rows_log2 > 0 {
            header.tile_rows_log2 += u8::from(reader.read_bit("increment_tile_rows_log2")?);
        }
        Ok(())
    }

    /// Superblock columns behind tile info.
    fn size_superblocks(&mut self, header: &FrameHeader) {
        let mi_cols = (header.frame_width + 7) >> 3;
        self.sb64_cols = (mi_cols + 7) >> 3;
    }

    /// Color config cached behind inter frames.
    fn cache_color(&mut self, header: &FrameHeader) {
        self.bit_depth = header.bit_depth;
        self.subsampling_x = header.subsampling_x();
        self.subsampling_y = header.subsampling_y();
        self.color_space = header.color_space;
        self.color_range = header.color_range;
    }

    /// Past state reset behind intra and error resilient frames.
    fn setup_past_independence(&mut self, header: &mut FrameHeader) {
        self.seg.reset_past();
        self.lf.reset_past();
        header.ref_frame_sign_bias = [0; 4];
    }

    /// Reference sizes refreshed from `header`.
    fn refresh_refs(&mut self, header: &FrameHeader) {
        for (slot, dims) in self.ref_dims.iter_mut().enumerate() {
            if header.refresh_frame_flags >> slot & 1 != 0 {
                *dims = (header.frame_width, header.frame_height);
            }
        }
    }
}

/// Fixed-width value below 256.
fn read_small<E: From<Error>>(
    reader: &mut BitReader,
    width: u32,
    syntax: &'static str,
) -> Result<u8, E> {
    let value = reader.read_bits(width, syntax)?;
    u8::try_from(value).map_err(|_| E::from(Error::Range { syntax }))
}

/// Signed magnitude value of `width` bits plus a sign bit.
fn read_delta<E: From<Error>>(
    reader: &mut BitReader,
    width: u32,
    syntax: &'static str,
) -> Result<i8, E> {
    let magnitude = reader.read_bits(width, syntax)?;
    let negative = reader.read_bit(syntax)?;
    let magnitude = i8::try_from(magnitude).map_err(|_| E::from(Error::Range { syntax }))?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Coded probability, 255 when absent.
fn read_prob<E: From<Error>>(reader: &mut BitReader, syntax: &'static str) -> Result<u8, E> {
    if reader.read_bit(syntax)? {
        read_small(reader, 8, syntax)
    } else {
        Ok(255)
    }
}

/// Quantizer delta, zero when absent.
fn read_delta_q<E: From<Error>>(reader: &mut BitReader, syntax: &'static str) -> Result<i8, E> {
    if reader.read_bit(syntax)? {
        read_delta(reader, 4, syntax)
    } else {
        Ok(0)
    }
}

/// Profile id with the reserved combination rejected.
fn parse_profile<E: From<Error>>(reader: &mut BitReader) -> Result<u8, E> {
    let low = reader.read_bits(1, "profile")?;
    let high = reader.read_bits(1, "profile")?;
    let profile = low | high << 1;
    if profile == 3 && reader.read_bit("profile")? {
        return Err(E::from(Error::ReservedProfile));
    }
    u8::try_from(profile).map_err(|_| E::from(Error::Range { syntax: "profile" }))
}

/// Sync code checked against the codec bytes.
fn parse_sync_code<E: From<Error>>(reader: &mut BitReader) -> Result<(), E> {
    if reader.read_bits(24, "sync_code")? != SYNC_CODE {
        return Err(E::from(Error::SyncCodeMismatch));
    }
    Ok(())
}

/// Render size, explicit or inferred from the frame size.
fn parse_render_size<E: From<Error>>(
    reader: &mut BitReader,
    header: &mut FrameHeader,
) -> Result<(), E> {
    header.set(
        FH_RENDER_DIFFERS,
        reader.read_bit("render_and_frame_size_different")?,
    );
    if header.render_differs() {
        header.render_width = reader.read_bits(16, "render_width_minus_1")? + 1;
        header.render_height = reader.read_bits(16, "render_height_minus_1")? + 1;
    } else {
        header.render_width = header.frame_width;
        header.render_height = header.frame_height;
    }
    Ok(())
}

/// Quantizer base and deltas.
fn parse_quantization<E: From<Error>>(
    reader: &mut BitReader,
    header: &mut FrameHeader,
) -> Result<(), E> {
    header.base_q_idx = read_small(reader, 8, "base_q_idx")?;
    header.delta_q_y_dc = read_delta_q(reader, "delta_q_y_dc")?;
    header.delta_q_uv_dc = read_delta_q(reader, "delta_q_uv_dc")?;
    header.delta_q_uv_ac = read_delta_q(reader, "delta_q_uv_ac")?;
    Ok(())
}

/// Smallest column shift fitting `sb64_cols` under the max tile width.
fn min_log2_tile_cols(sb64_cols: u32) -> u8 {
    let mut shift = 0u8;
    while u64::from(MAX_TILE_WIDTH_SB) << shift < u64::from(sb64_cols) {
        shift += 1;
    }
    shift
}

/// Largest column shift keeping every tile above the min width.
fn max_log2_tile_cols(sb64_cols: u32) -> u8 {
    let mut shift = 1u8;
    while sb64_cols >> shift >= MIN_TILE_WIDTH_SB {
        shift += 1;
    }
    shift - 1
}

/// Tile payloads of `frame` in tile order.
fn split_tiles<E: From<Error>>(frame: &[u8], header: &FrameHeader) -> Result<Vec<TileData>, E> {
    if header.tile_cols_log2 > MAX_TILE_COLS_LOG2 {
        return Err(E::from(Error::Range {
            syntax: "tile_cols_log2",
        }));
    }
    if header.tile_rows_log2 > MAX_TILE_ROWS_LOG2 {
        return Err(E::from(Error::Range {
            syntax: "tile_rows_log2",
        }));
    }
    let cols = 1u16 << header.tile_cols_log2;
    let rows = 1u16 << header.tile_rows_log2;
    let start = usize::from(header.uncompressed_size) + usize::from(header.header_size_in_bytes);
    let mut data = frame.get(start..).ok_or(E::from(Error::Truncated {
        syntax: "tile_data",
    }))?;
    let mut tiles = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for row in 0..rows {
        for column in 0..cols {
            let last = row + 1 == rows && column + 1 == cols;
            let (bytes, rest) = if last {
                (data, &data[data.len()..])
            } else {
                let field = data.get(..4).ok_or(E::from(Error::Truncated {
                    syntax: "tile_size",
                }))?;
                let raw = u32::from_be_bytes([field[0], field[1], field[2], field[3]]);
                let size = usize::try_from(raw).map_err(|_| {
                    E::from(Error::Range {
                        syntax: "tile_size",
                    })
                })?;
                let tail = data.get(4..).ok_or(E::from(Error::Truncated {
                    syntax: "tile_size",
                }))?;
                let bytes = tail.get(..size).ok_or(E::from(Error::Truncated {
                    syntax: "tile_data",
                }))?;
                let rest = tail.get(size..).ok_or(E::from(Error::Truncated {
                    syntax: "tile_data",
                }))?;
                (bytes, rest)
            };
            tiles.push(TileData {
                bytes: bytes.to_vec(),
                row,
                column,
            });
            data = rest;
        }
    }
    Ok(tiles)
}
