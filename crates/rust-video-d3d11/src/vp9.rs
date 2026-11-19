//! VP9 frame ingest: uncompressed headers plus tile assembly.
//!
//! Reference sizes behind frame-size inheritance; the DPB owns surfaces and output.

use crate::bits::BitReader;
use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason};

/// Frame marker bits opening every frame.
const FRAME_MARKER: u32 = 2;

/// Keyframe sync code.
const SYNC_CODE: u32 = 0x49_83_42;

/// Superframe index marker closing the chunk.
const SUPERFRAME_MARKER: u8 = 0x06;

/// Reference frames per inter frame.
pub(crate) const REFS_PER_FRAME: usize = 3;

/// Reference slots behind the map.
pub(crate) const NUM_REF_FRAMES: usize = 8;

/// Segments per frame.
pub(crate) const MAX_SEGMENTS: usize = 8;

/// Segment levels per segment.
pub(crate) const SEG_LEVELS: usize = 4;

/// Segment tree probabilities per frame.
const TREE_PROBS: usize = 7;

/// Segment prediction probabilities per frame.
const PRED_PROBS: usize = 3;

/// Smallest tile width in superblocks.
const MIN_TILE_WIDTH_SB: u32 = 4;

/// Largest tile width in superblocks.
const MAX_TILE_WIDTH_SB: u32 = 64;

/// Tile column shift ceiling.
const MAX_TILE_COLS_LOG2: u8 = 6;

/// Tile row shift ceiling.
const MAX_TILE_ROWS_LOG2: u8 = 2;

/// Switchable interpolation filter id.
const INTERP_SWITCHABLE: u8 = 4;

/// Literal to filter id per S6.2.
const INTERP_LITERAL: [u8; 4] = [1, 0, 2, 3];

/// sRGB color space id.
const COLOR_SRGB: u8 = 7;

/// BT.601 color space id behind profile 0 intra-only frames.
const COLOR_BT601: u8 = 1;

/// Existing-frame display flag bit.
pub(crate) const FH_SHOW_EXISTING: u32 = 1;

/// Frame display flag bit.
pub(crate) const FH_SHOW_FRAME: u32 = 1 << 1;

/// Error resilient mode flag bit.
pub(crate) const FH_ERROR_RESILIENT: u32 = 1 << 2;

/// Intra-only frame flag bit.
pub(crate) const FH_INTRA_ONLY: u32 = 1 << 3;

/// Reference context refresh flag bit.
pub(crate) const FH_REFRESH_CONTEXT: u32 = 1 << 4;

/// Parallel decode mode flag bit.
pub(crate) const FH_FRAME_PARALLEL: u32 = 1 << 5;

/// High precision motion vector flag bit.
pub(crate) const FH_HIGH_PRECISION_MV: u32 = 1 << 6;

/// Render size differs flag bit.
pub(crate) const FH_RENDER_DIFFERS: u32 = 1 << 7;

/// Horizontal subsampling flag bit.
pub(crate) const FH_SUBSAMPLING_X: u32 = 1 << 8;

/// Vertical subsampling flag bit.
pub(crate) const FH_SUBSAMPLING_Y: u32 = 1 << 9;

/// Segmentation enabled flag bit.
pub(crate) const FH_SEG_ENABLED: u32 = 1 << 10;

/// Segmentation map update flag bit.
pub(crate) const FH_SEG_UPDATE_MAP: u32 = 1 << 11;

/// Segmentation temporal update flag bit.
pub(crate) const FH_SEG_TEMPORAL_UPDATE: u32 = 1 << 12;

/// Segmentation data update flag bit.
pub(crate) const FH_SEG_UPDATE_DATA: u32 = 1 << 13;

/// Segmentation absolute delta flag bit.
pub(crate) const FH_SEG_ABS_DELTA: u32 = 1 << 14;

/// Loop filter delta enabled flag bit.
pub(crate) const FH_LF_DELTA_ENABLED: u32 = 1 << 15;

/// Loop filter delta update flag bit.
pub(crate) const FH_LF_DELTA_UPDATE: u32 = 1 << 16;

/// VP9 frame type.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum FrameType {
    /// Key frame.
    #[default]
    Key,
    /// Inter frame.
    Inter,
}

impl FrameType {
    /// True for key frames.
    #[must_use]
    pub(crate) fn is_intra(self) -> bool {
        matches!(self, Self::Key)
    }
}

/// Parsed VP9 frame header.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FrameHeader {
    /// Packed frame flags.
    pub(crate) flags: u32,
    /// Frame type.
    pub(crate) frame_type: FrameType,
    /// Profile id.
    pub(crate) profile: u8,
    /// Bit depth.
    pub(crate) bit_depth: u8,
    /// Color space id.
    pub(crate) color_space: u8,
    /// Color range id.
    pub(crate) color_range: u8,
    /// Shown slot for existing-frame display.
    pub(crate) frame_to_show_map_idx: u8,
    /// Frame width in luma samples.
    pub(crate) frame_width: u32,
    /// Frame height in luma samples.
    pub(crate) frame_height: u32,
    /// Render width in luma samples.
    pub(crate) render_width: u32,
    /// Render height in luma samples.
    pub(crate) render_height: u32,
    /// Refresh flags per slot.
    pub(crate) refresh_frame_flags: u8,
    /// Reference slot per position.
    pub(crate) ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Reference sign bias per frame.
    pub(crate) ref_frame_sign_bias: [u8; 4],
    /// Frame context reset mode.
    pub(crate) reset_frame_context: u8,
    /// Frame context id.
    pub(crate) frame_context_idx: u8,
    /// Interpolation filter id.
    pub(crate) interp_filter: u8,
    /// Loop filter level.
    pub(crate) filter_level: u8,
    /// Loop filter sharpness.
    pub(crate) sharpness: u8,
    /// Loop filter reference deltas.
    pub(crate) ref_deltas: [i8; 4],
    /// Loop filter mode deltas.
    pub(crate) mode_deltas: [i8; 2],
    /// Base quantizer index.
    pub(crate) base_q_idx: u8,
    /// Y DC delta.
    pub(crate) delta_q_y_dc: i8,
    /// UV DC delta.
    pub(crate) delta_q_uv_dc: i8,
    /// UV AC delta.
    pub(crate) delta_q_uv_ac: i8,
    /// Segmentation tree probabilities.
    pub(crate) tree_probs: [u8; TREE_PROBS],
    /// Segmentation prediction probabilities.
    pub(crate) pred_probs: [u8; PRED_PROBS],
    /// Segmentation feature enables per segment and level.
    pub(crate) seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Segmentation feature values per segment and level.
    pub(crate) seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// Tile column shift.
    pub(crate) tile_cols_log2: u8,
    /// Tile row shift.
    pub(crate) tile_rows_log2: u8,
    /// Compressed header size in bytes.
    pub(crate) header_size_in_bytes: u16,
    /// Uncompressed header size in bytes.
    pub(crate) uncompressed_size: u16,
}

impl FrameHeader {
    /// Flag `flag` raised when `on`.
    fn set(&mut self, flag: u32, on: bool) {
        if on {
            self.flags |= flag;
        }
    }

    /// Existing-frame display.
    pub(crate) const fn show_existing_frame(&self) -> bool {
        self.flags & FH_SHOW_EXISTING != 0
    }

    /// Frame display.
    pub(crate) const fn show_frame(&self) -> bool {
        self.flags & FH_SHOW_FRAME != 0
    }

    /// Error resilient mode.
    pub(crate) const fn error_resilient_mode(&self) -> bool {
        self.flags & FH_ERROR_RESILIENT != 0
    }

    /// Intra-only frame.
    pub(crate) const fn intra_only(&self) -> bool {
        self.flags & FH_INTRA_ONLY != 0
    }

    /// Reference context refresh.
    pub(crate) const fn refresh_frame_context(&self) -> bool {
        self.flags & FH_REFRESH_CONTEXT != 0
    }

    /// Parallel decode mode.
    pub(crate) const fn frame_parallel_decoding_mode(&self) -> bool {
        self.flags & FH_FRAME_PARALLEL != 0
    }

    /// High precision motion vectors.
    pub(crate) const fn allow_high_precision_mv(&self) -> bool {
        self.flags & FH_HIGH_PRECISION_MV != 0
    }

    /// Render size against the frame size.
    pub(crate) const fn render_differs(&self) -> bool {
        self.flags & FH_RENDER_DIFFERS != 0
    }

    /// Horizontal subsampling.
    pub(crate) const fn subsampling_x(&self) -> bool {
        self.flags & FH_SUBSAMPLING_X != 0
    }

    /// Vertical subsampling.
    pub(crate) const fn subsampling_y(&self) -> bool {
        self.flags & FH_SUBSAMPLING_Y != 0
    }

    /// Segmentation enabled.
    pub(crate) const fn segmentation_enabled(&self) -> bool {
        self.flags & FH_SEG_ENABLED != 0
    }

    /// Segmentation map update.
    pub(crate) const fn segmentation_update_map(&self) -> bool {
        self.flags & FH_SEG_UPDATE_MAP != 0
    }

    /// Segmentation temporal update.
    pub(crate) const fn segmentation_temporal_update(&self) -> bool {
        self.flags & FH_SEG_TEMPORAL_UPDATE != 0
    }

    /// Segmentation absolute delta.
    pub(crate) const fn segmentation_abs_delta(&self) -> bool {
        self.flags & FH_SEG_ABS_DELTA != 0
    }

    /// Loop filter delta enabled.
    pub(crate) const fn loop_filter_delta_enabled(&self) -> bool {
        self.flags & FH_LF_DELTA_ENABLED != 0
    }

    /// Loop filter delta update.
    pub(crate) const fn loop_filter_delta_update(&self) -> bool {
        self.flags & FH_LF_DELTA_UPDATE != 0
    }
}

/// One decoded tile: payload plus grid position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TileData {
    /// Tile payload bytes without the size field.
    pub(crate) bytes: Vec<u8>,
    /// Tile row.
    pub(crate) row: u16,
    /// Tile column.
    pub(crate) column: u16,
}

/// One coded frame: header plus bytes plus tiles in tile order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Frame {
    /// Parsed frame header.
    pub(crate) header: FrameHeader,
    /// Coded frame bytes behind driver submission.
    pub(crate) bytes: Vec<u8>,
    /// Tile payloads in tile order.
    pub(crate) tiles: Vec<TileData>,
}

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
    fn parse(&mut self, reader: &mut BitReader) -> Result<(), D3d11Error> {
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
    fn parse(&mut self, reader: &mut BitReader) -> Result<(), D3d11Error> {
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
                    D3d11Error::Parse(ParseReason::Range {
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
pub(crate) struct Ingest {
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
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
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
        }
    }

    /// Frames completed by `data`; every frame completes inline.
    pub(crate) fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Frame>, D3d11Error> {
        let mut frames = Vec::new();
        for coded in split_superframe(data)? {
            frames.push(self.parse_frame(coded)?);
        }
        Ok(frames)
    }

    /// Open frame, never any behind inline completion.
    // Receiver and result mirror the sibling ingest finish behind decoder flush.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(crate) fn finish(&mut self) -> Result<Option<Frame>, D3d11Error> {
        Ok(None)
    }

    /// Reference state reset after a mid-stream failure.
    pub(crate) fn reset_refs(&mut self) {
        *self = Self::new();
    }

    /// Frame parsed from `data` with tiles split.
    fn parse_frame(&mut self, data: &[u8]) -> Result<Frame, D3d11Error> {
        let mut reader = BitReader::new(data);
        let mut header = FrameHeader::default();
        if reader.read_bits(2, "frame_marker")? != FRAME_MARKER {
            return Err(D3d11Error::Parse(ParseReason::FrameMarkerMismatch));
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
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "header_size_in_bytes",
                })
            })?;
        header.uncompressed_size = u16::try_from(reader.position().div_ceil(8)).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
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
    fn parse_key(
        &mut self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), D3d11Error> {
        parse_sync_code(reader)?;
        self.parse_color_config(reader, header)?;
        self.parse_frame_size(reader, header)?;
        parse_render_size(reader, header)?;
        header.refresh_frame_flags = 0xFF;
        Ok(())
    }

    /// Inter frame references, size, and prediction tools.
    fn parse_inter(
        &mut self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), D3d11Error> {
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
                D3d11Error::Parse(ParseReason::Range {
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
    ) -> Result<(), D3d11Error> {
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
    ) -> Result<(), D3d11Error> {
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
    ) -> Result<(), D3d11Error> {
        for position in 0..REFS_PER_FRAME {
            if !reader.read_bit("found_ref")? {
                continue;
            }
            let slot = usize::from(header.ref_frame_idx[position]);
            let (width, height) = self.ref_dims[slot];
            if width == 0 || height == 0 {
                return Err(D3d11Error::BrokenReference(
                    BrokenReferenceReason::NamesEmptySlot {
                        reference: "ref_frame_idx",
                    },
                ));
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
    fn parse_tile_info(
        &self,
        reader: &mut BitReader,
        header: &mut FrameHeader,
    ) -> Result<(), D3d11Error> {
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
            return Err(D3d11Error::Parse(ParseReason::Range {
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
fn read_small(reader: &mut BitReader, width: u32, syntax: &'static str) -> Result<u8, D3d11Error> {
    let value = reader.read_bits(width, syntax)?;
    u8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Signed magnitude value of `width` bits plus a sign bit.
fn read_delta(reader: &mut BitReader, width: u32, syntax: &'static str) -> Result<i8, D3d11Error> {
    let magnitude = reader.read_bits(width, syntax)?;
    let negative = reader.read_bit(syntax)?;
    let magnitude =
        i8::try_from(magnitude).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Coded probability, 255 when absent.
fn read_prob(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d11Error> {
    if reader.read_bit(syntax)? {
        read_small(reader, 8, syntax)
    } else {
        Ok(255)
    }
}

/// Quantizer delta, zero when absent.
fn read_delta_q(reader: &mut BitReader, syntax: &'static str) -> Result<i8, D3d11Error> {
    if reader.read_bit(syntax)? {
        read_delta(reader, 4, syntax)
    } else {
        Ok(0)
    }
}

/// Profile id with the reserved combination rejected.
fn parse_profile(reader: &mut BitReader) -> Result<u8, D3d11Error> {
    let low = reader.read_bits(1, "profile")?;
    let high = reader.read_bits(1, "profile")?;
    let profile = low | high << 1;
    if profile == 3 && reader.read_bit("profile")? {
        return Err(D3d11Error::Parse(ParseReason::ReservedProfile));
    }
    u8::try_from(profile).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "profile" }))
}

/// Sync code checked against the codec bytes.
fn parse_sync_code(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if reader.read_bits(24, "sync_code")? != SYNC_CODE {
        return Err(D3d11Error::Parse(ParseReason::SyncCodeMismatch));
    }
    Ok(())
}

/// Render size, explicit or inferred from the frame size.
fn parse_render_size(reader: &mut BitReader, header: &mut FrameHeader) -> Result<(), D3d11Error> {
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
fn parse_quantization(reader: &mut BitReader, header: &mut FrameHeader) -> Result<(), D3d11Error> {
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

/// Chunk frames: superframe members or the chunk alone.
fn split_superframe(data: &[u8]) -> Result<Vec<&[u8]>, D3d11Error> {
    let Some(last) = data.last().copied() else {
        return Ok(Vec::new());
    };
    if last >> 5 != SUPERFRAME_MARKER {
        return Ok(vec![data]);
    }
    let bytes_per_size = usize::from((last >> 3) & 3) + 1;
    let count = usize::from(last & 7) + 1;
    let index = 2 + count * bytes_per_size;
    let Some(coded) = data.len().checked_sub(index) else {
        return Ok(vec![data]);
    };
    if data[coded] != last {
        return Ok(vec![data]);
    }
    let mut frames = Vec::with_capacity(count);
    let mut offset = 0usize;
    for position in 0..count {
        let mut size = 0usize;
        for byte in 0..bytes_per_size {
            size |= usize::from(data[coded + 1 + position * bytes_per_size + byte]) << (byte * 8);
        }
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= coded)
            .ok_or(D3d11Error::Parse(ParseReason::Range {
                syntax: "superframe_index",
            }))?;
        frames.push(&data[offset..end]);
        offset = end;
    }
    if offset != coded {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "superframe_index",
        }));
    }
    Ok(frames)
}

/// Tile payloads of `frame` in tile order.
fn split_tiles(frame: &[u8], header: &FrameHeader) -> Result<Vec<TileData>, D3d11Error> {
    if header.tile_cols_log2 > MAX_TILE_COLS_LOG2 {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "tile_cols_log2",
        }));
    }
    if header.tile_rows_log2 > MAX_TILE_ROWS_LOG2 {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "tile_rows_log2",
        }));
    }
    let cols = 1u16 << header.tile_cols_log2;
    let rows = 1u16 << header.tile_rows_log2;
    let start = usize::from(header.uncompressed_size) + usize::from(header.header_size_in_bytes);
    let mut data = frame
        .get(start..)
        .ok_or(D3d11Error::Parse(ParseReason::Truncated {
            syntax: "tile_data",
        }))?;
    let mut tiles = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for row in 0..rows {
        for column in 0..cols {
            let last = row + 1 == rows && column + 1 == cols;
            let (bytes, rest) = if last {
                (data, &data[data.len()..])
            } else {
                let field = data
                    .get(..4)
                    .ok_or(D3d11Error::Parse(ParseReason::Truncated {
                        syntax: "tile_size",
                    }))?;
                let raw = u32::from_be_bytes([field[0], field[1], field[2], field[3]]);
                let size = usize::try_from(raw).map_err(|_| {
                    D3d11Error::Parse(ParseReason::Range {
                        syntax: "tile_size",
                    })
                })?;
                let tail = data
                    .get(4..)
                    .ok_or(D3d11Error::Parse(ParseReason::Truncated {
                        syntax: "tile_size",
                    }))?;
                let bytes = tail
                    .get(..size)
                    .ok_or(D3d11Error::Parse(ParseReason::Truncated {
                        syntax: "tile_data",
                    }))?;
                let rest = tail
                    .get(size..)
                    .ok_or(D3d11Error::Parse(ParseReason::Truncated {
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

#[cfg(test)]
mod tests {
    use super::Ingest;
    use super::FH_SEG_UPDATE_DATA;
    use crate::error::{D3d11Error, ParseReason};
    use rust_video_harness::decode_dir;

    /// Bit writer behind synthetic headers.
    struct Writer {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte.
        open: u8,
        /// Bits filled in the open byte.
        filled: u32,
    }

    impl Writer {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                open: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, on: bool) {
            self.open = (self.open << 1) | u8::from(on);
            self.filled += 1;
            if self.filled == 8 {
                self.bytes.push(self.open);
                self.open = 0;
                self.filled = 0;
            }
        }

        /// Low `count` bits of `value`, high to low.
        fn bits(&mut self, value: u32, count: u32) {
            for shift in (0..count).rev() {
                self.bit(value >> shift & 1 != 0);
            }
        }

        /// Finished bytes with zero padding.
        fn bytes(mut self) -> Vec<u8> {
            if self.filled > 0 {
                self.bytes.push(self.open << (8 - self.filled));
            }
            self.bytes
        }
    }

    /// Profile 0 keyframe head through segmentation at `width` x `height`.
    fn key_head(writer: &mut Writer, width: u32, height: u32) {
        writer.bits(2, 2);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0x49_83_42, 24);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bits(width - 1, 16);
        writer.bits(height - 1, 16);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bits(0, 2);
        writer.bits(10, 6);
        writer.bits(2, 3);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bits(5, 6);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(100, 8);
        writer.bit(true);
        writer.bits(3, 4);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        for _ in 0..7 {
            writer.bit(true);
            writer.bits(128, 8);
        }
        writer.bit(true);
        for _ in 0..3 {
            writer.bit(true);
            writer.bits(200, 8);
        }
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bits(5, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bits(2, 2);
        writer.bit(true);
        for _ in 1..8 {
            writer.bits(0, 4);
        }
    }

    /// Synthetic profile 0 keyframe, 16x16 with segmentation and one tile.
    fn key_16x16() -> Vec<u8> {
        let mut writer = Writer::new();
        key_head(&mut writer, 16, 16);
        writer.bit(false);
        writer.bits(4, 16);
        let mut bytes = writer.bytes();
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        bytes.extend_from_slice(&[0x11, 0x22, 0x33]);
        bytes
    }

    /// Synthetic inter frame inheriting the key size.
    fn inter_16x16() -> Vec<u8> {
        let mut writer = Writer::new();
        writer.bits(2, 2);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 2);
        writer.bits(0x02, 8);
        for _ in 0..3 {
            writer.bits(0, 3);
            writer.bit(false);
        }
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(1, 2);
        writer.bit(true);
        writer.bit(false);
        writer.bits(1, 2);
        writer.bits(5, 6);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bits(90, 8);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(2, 16);
        let mut bytes = writer.bytes();
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        bytes.extend_from_slice(&[0x44, 0x55, 0x66, 0x77, 0x88]);
        bytes
    }

    /// Synthetic keyframe, 4096x16 with 4x2 tiles.
    fn tiled_4096x16() -> Vec<u8> {
        let mut writer = Writer::new();
        key_head(&mut writer, 4096, 16);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bits(0, 16);
        let mut bytes = writer.bytes();
        for tile in 0..7u8 {
            let size = u32::from(tile) + 1;
            bytes.extend_from_slice(&size.to_be_bytes());
            let len = usize::try_from(size).expect("tile fits");
            bytes.extend(std::iter::repeat_n(0xA0 + tile, len));
        }
        bytes.extend_from_slice(&[0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7, 0xA7]);
        bytes
    }

    #[test]
    fn keyframe_header_parses_every_field() {
        let bytes = key_16x16();
        let frames = Ingest::new().ingest_packet(&bytes).expect("keyframe");
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        let header = &frame.header;
        assert!(header.frame_type.is_intra());
        assert_eq!(header.profile, 0);
        assert_eq!(header.bit_depth, 8);
        assert_eq!(header.color_space, 0);
        assert_eq!(header.color_range, 0);
        assert!(!header.show_existing_frame());
        assert!(header.show_frame());
        assert!(!header.error_resilient_mode());
        assert!(!header.intra_only());
        assert_eq!((header.frame_width, header.frame_height), (16, 16));
        assert_eq!((header.render_width, header.render_height), (16, 16));
        assert!(!header.render_differs());
        assert_eq!(header.refresh_frame_flags, 0xFF);
        assert_eq!(header.ref_frame_idx, [0; 3]);
        assert_eq!(header.ref_frame_sign_bias, [0; 4]);
        assert_eq!(header.reset_frame_context, 0);
        assert!(header.refresh_frame_context());
        assert!(header.frame_parallel_decoding_mode());
        assert_eq!(header.frame_context_idx, 0);
        assert_eq!(header.filter_level, 10);
        assert_eq!(header.sharpness, 2);
        assert!(header.loop_filter_delta_enabled());
        assert!(header.loop_filter_delta_update());
        assert_eq!(header.ref_deltas, [5, 0, -1, -1]);
        assert_eq!(header.mode_deltas, [0; 2]);
        assert_eq!(header.base_q_idx, 100);
        assert_eq!(header.delta_q_y_dc, 3);
        assert_eq!(header.delta_q_uv_dc, 0);
        assert_eq!(header.delta_q_uv_ac, 0);
        assert!(header.segmentation_enabled());
        assert!(header.segmentation_update_map());
        assert!(header.segmentation_temporal_update());
        assert_ne!(header.flags & FH_SEG_UPDATE_DATA, 0);
        assert!(header.segmentation_abs_delta());
        assert_eq!(header.tree_probs, [128; 7]);
        assert_eq!(header.pred_probs, [200; 3]);
        assert_eq!(
            header.seg_enabled[0],
            [true, false, true, true],
            "segment 0 features"
        );
        assert_eq!(header.seg_value[0], [5, 0, 2, 0], "segment 0 values");
        for segment in 1..8 {
            assert_eq!(header.seg_enabled[segment], [false; 4]);
            assert_eq!(header.seg_value[segment], [0; 4]);
        }
        assert_eq!((header.tile_cols_log2, header.tile_rows_log2), (0, 0));
        assert_eq!(header.header_size_in_bytes, 4);
        assert_eq!(
            usize::from(header.uncompressed_size) + 4 + 3,
            bytes.len(),
            "header plus compressed plus tile fills the frame"
        );
        assert_eq!(frame.bytes, bytes);
        assert_eq!(frame.tiles.len(), 1);
        assert_eq!((frame.tiles[0].row, frame.tiles[0].column), (0, 0));
        assert_eq!(frame.tiles[0].bytes, vec![0x11, 0x22, 0x33]);
        assert!(header.subsampling_x() && header.subsampling_y());
    }

    #[test]
    fn inter_frame_inherits_size_and_state() {
        let mut ingest = Ingest::new();
        ingest.ingest_packet(&key_16x16()).expect("keyframe");
        let frames = ingest.ingest_packet(&inter_16x16()).expect("inter");
        assert_eq!(frames.len(), 1);
        let header = &frames[0].header;
        assert!(!header.frame_type.is_intra());
        assert_eq!((header.frame_width, header.frame_height), (16, 16));
        assert!(header.show_frame());
        assert!(!header.intra_only());
        assert_eq!(header.reset_frame_context, 0);
        assert_eq!(header.refresh_frame_flags, 0x02);
        assert_eq!(header.ref_frame_idx, [0; 3]);
        assert_eq!(header.ref_frame_sign_bias, [0; 4]);
        assert_eq!(header.bit_depth, 8);
        assert_eq!(header.color_space, 0);
        assert_eq!(header.color_range, 0);
        assert!(header.subsampling_x() && header.subsampling_y());
        assert!(!header.allow_high_precision_mv());
        assert_eq!(header.interp_filter, 0);
        assert!(header.refresh_frame_context());
        assert!(!header.frame_parallel_decoding_mode());
        assert_eq!(header.frame_context_idx, 1);
        assert_eq!(header.filter_level, 5);
        assert!(!header.loop_filter_delta_enabled());
        assert!(!header.loop_filter_delta_update());
        assert_eq!(header.ref_deltas, [5, 0, -1, -1], "deltas persist");
        assert_eq!(header.base_q_idx, 90);
        assert!(!header.segmentation_enabled());
        assert_eq!(header.tree_probs, [128; 7], "probabilities persist");
        assert_eq!(header.seg_value[0], [5, 0, 2, 0], "features persist");
        assert_eq!(frames[0].tiles[0].bytes, vec![0x44, 0x55, 0x66, 0x77, 0x88]);
    }

    #[test]
    fn inter_before_key_breaks_reference() {
        let error = Ingest::new().ingest_packet(&inter_16x16()).unwrap_err();
        assert!(matches!(error, D3d11Error::BrokenReference(_)));
        assert_eq!(
            error.to_string(),
            "broken reference list: ref_frame_idx names empty slot"
        );
    }

    #[test]
    fn reset_refs_recovers_after_failure() {
        let mut ingest = Ingest::new();
        ingest
            .ingest_packet(&inter_16x16())
            .expect_err("inter before key");
        ingest.reset_refs();
        let frames = ingest.ingest_packet(&key_16x16()).expect("keyframe");
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn bad_marker_rejects() {
        let error = Ingest::new().ingest_packet(&[0x00]).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::FrameMarkerMismatch)
        ));
        assert_eq!(error.to_string(), "parse failed: frame marker mismatch");
    }

    #[test]
    fn bad_sync_code_rejects() {
        let error = Ingest::new()
            .ingest_packet(&[0x82, 0x00, 0x00, 0x00])
            .unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::SyncCodeMismatch)
        ));
        assert_eq!(error.to_string(), "parse failed: sync code mismatch");
    }

    #[test]
    fn reserved_profile_rejects() {
        let error = Ingest::new().ingest_packet(&[0xBC]).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::ReservedProfile)
        ));
        assert_eq!(error.to_string(), "parse failed: reserved profile");
    }

    #[test]
    fn truncated_header_rejects() {
        let error = Ingest::new().ingest_packet(&[0x82]).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::Truncated { .. })
        ));
    }

    #[test]
    fn empty_packet_yields_no_frames() {
        let frames = Ingest::new().ingest_packet(&[]).expect("empty packet");
        assert!(frames.is_empty(), "empty packet yields no frames");
        assert!(Ingest::new().finish().expect("finish").is_none());
    }

    #[test]
    fn superframe_splits_members() {
        let key = key_16x16();
        let mut chunk = vec![0x88];
        chunk.extend_from_slice(&key);
        chunk.push(0xC1);
        chunk.push(1);
        chunk.push(u8::try_from(key.len()).expect("key fits"));
        chunk.push(0xC1);
        let frames = Ingest::new().ingest_packet(&chunk).expect("superframe");
        assert_eq!(frames.len(), 2);
        assert!(frames[0].header.show_existing_frame());
        assert_eq!(frames[0].header.frame_to_show_map_idx, 0);
        assert!(frames[0].tiles.is_empty(), "display carries no tiles");
        assert_eq!(frames[0].bytes, vec![0x88]);
        assert!(frames[1].header.frame_type.is_intra());
        assert_eq!(frames[1].bytes, key);
    }

    #[test]
    fn corrupt_superframe_index_rejects() {
        let error = Ingest::new()
            .ingest_packet(&[0x88, 0xC1, 0x05, 0x00, 0xC1])
            .unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::Range { .. })
        ));
    }

    #[test]
    fn mismatched_superframe_bookends_parse_single() {
        let frames = Ingest::new()
            .ingest_packet(&[0x88, 0xAA, 0xBB, 0x01, 0xC1])
            .expect("plain frame");
        assert_eq!(frames.len(), 1);
        assert!(frames[0].header.show_existing_frame());
    }

    #[test]
    fn tiles_split_big_endian_sizes() {
        let bytes = tiled_4096x16();
        let frames = Ingest::new().ingest_packet(&bytes).expect("tiled");
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        assert_eq!(
            (frame.header.tile_cols_log2, frame.header.tile_rows_log2),
            (2, 1)
        );
        assert_eq!(frame.tiles.len(), 8);
        for (index, tile) in frame.tiles.iter().enumerate() {
            let position = u16::try_from(index).expect("tile fits");
            assert_eq!(tile.row, position / 4, "tile {index} row");
            assert_eq!(tile.column, position % 4, "tile {index} column");
            let fill = 0xA0 + u8::try_from(index).expect("tile fits");
            let len = if index < 7 { index + 1 } else { 8 };
            assert_eq!(tile.bytes, vec![fill; len], "tile {index} bytes");
        }
    }

    #[test]
    fn dpb_slices_land_on_tile_payloads() {
        let bytes = tiled_4096x16();
        let frames = Ingest::new().ingest_packet(&bytes).expect("tiled");
        assert_eq!(frames.len(), 1);
        let mut dpb = crate::vp9_dpb::Vp9Dpb::new();
        let decoded = dpb.decode(&frames[0]).expect("dpb accepts key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slices.len(), frames[0].tiles.len());
        for (slice, tile) in submission.slices.iter().zip(frames[0].tiles.iter()) {
            let start = usize::try_from(slice.location).expect("offset fits");
            let len = usize::try_from(slice.bytes).expect("size fits");
            assert_eq!(
                &submission.bitstream[start..start + len],
                tile.bytes.as_slice()
            );
        }
    }

    #[test]
    fn truncated_tile_size_rejects() {
        let bytes = tiled_4096x16();
        let tile_section: usize = (1..=7).sum::<usize>() + 7 * 4 + 8;
        let cut = bytes.len() - tile_section + 2;
        let error = Ingest::new().ingest_packet(&bytes[..cut]).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::Truncated { .. })
        ));
    }

    #[test]
    fn oversized_tile_rejects() {
        let mut bytes = tiled_4096x16();
        let tile_section: usize = (1..=7).sum::<usize>() + 7 * 4 + 8;
        let start = bytes.len() - tile_section;
        bytes[start..start + 4].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        let error = Ingest::new().ingest_packet(&bytes).unwrap_err();
        assert!(matches!(
            error,
            D3d11Error::Parse(ParseReason::Truncated { .. })
        ));
    }

    /// IVF payloads of the committed vector in file order.
    fn vector_payloads() -> Vec<Vec<u8>> {
        let path = decode_dir().join("valid_320x240.ivf");
        let data = std::fs::read(&path).expect("vector readable");
        assert!(data.len() >= 32 && &data[..4] == b"DKIF", "vector is IVF");
        let head = usize::from(u16::from_le_bytes([data[6], data[7]]).max(32));
        let mut payloads = Vec::new();
        let mut offset = head;
        while let Some(entry) = data.get(offset..offset + 12) {
            let size = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
            let size = usize::try_from(size).expect("payload fits");
            let Some(payload) = data.get(offset + 12..offset + 12 + size) else {
                break;
            };
            payloads.push(payload.to_vec());
            offset += 12 + size;
        }
        payloads
    }

    #[test]
    fn committed_vector_parses_fifteen_frames() {
        let payloads = vector_payloads();
        assert_eq!(payloads.len(), 15, "vector holds fifteen packets");
        let mut ingest = Ingest::new();
        let mut frames = Vec::new();
        for payload in &payloads {
            let parsed = ingest.ingest_packet(payload).expect("packet parses");
            assert_eq!(parsed.len(), 1, "packet holds one frame");
            frames.extend(parsed);
        }
        assert!(frames[0].header.frame_type.is_intra(), "first frame keys");
        for (index, frame) in frames.iter().enumerate() {
            let header = &frame.header;
            assert_eq!(
                (header.frame_width, header.frame_height),
                (320, 240),
                "frame {index} size"
            );
            assert!(header.show_frame(), "frame {index} shows");
            assert_eq!(header.profile, 0, "frame {index} profile");
            assert_eq!(header.bit_depth, 8, "frame {index} depth");
            assert!(
                header.subsampling_x() && header.subsampling_y(),
                "frame {index} 4:2:0"
            );
            assert_eq!(
                (header.tile_cols_log2, header.tile_rows_log2),
                (0, 0),
                "frame {index} single tile"
            );
            assert_eq!(frame.tiles.len(), 1, "frame {index} tile split");
            assert!(!frame.tiles[0].bytes.is_empty(), "frame {index} tile bytes");
            if index == 0 {
                assert_eq!(header.refresh_frame_flags, 0xFF, "key refreshes all");
            } else {
                assert!(!header.frame_type.is_intra(), "frame {index} inters");
            }
        }
    }
}
