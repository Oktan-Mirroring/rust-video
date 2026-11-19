//! Parsed VP9 frame header plus flag bits.

/// Reference frames per inter frame.
pub const REFS_PER_FRAME: usize = 3;

/// Reference slots behind the map.
pub const NUM_REF_FRAMES: usize = 8;

/// Segments per frame.
pub const MAX_SEGMENTS: usize = 8;

/// Segment levels per segment.
pub const SEG_LEVELS: usize = 4;

/// Segment tree probabilities per frame.
pub(crate) const TREE_PROBS: usize = 7;

/// Segment prediction probabilities per frame.
pub(crate) const PRED_PROBS: usize = 3;

/// Switchable interpolation filter id.
pub const INTERP_SWITCHABLE: u8 = 4;

/// Existing-frame display flag bit.
pub const FH_SHOW_EXISTING: u32 = 1;

/// Frame display flag bit.
pub const FH_SHOW_FRAME: u32 = 1 << 1;

/// Error resilient mode flag bit.
pub const FH_ERROR_RESILIENT: u32 = 1 << 2;

/// Intra-only frame flag bit.
pub const FH_INTRA_ONLY: u32 = 1 << 3;

/// Reference context refresh flag bit.
pub const FH_REFRESH_CONTEXT: u32 = 1 << 4;

/// Parallel decode mode flag bit.
pub const FH_FRAME_PARALLEL: u32 = 1 << 5;

/// High precision motion vector flag bit.
pub const FH_HIGH_PRECISION_MV: u32 = 1 << 6;

/// Render size differs flag bit.
pub const FH_RENDER_DIFFERS: u32 = 1 << 7;

/// Horizontal subsampling flag bit.
pub const FH_SUBSAMPLING_X: u32 = 1 << 8;

/// Vertical subsampling flag bit.
pub const FH_SUBSAMPLING_Y: u32 = 1 << 9;

/// Segmentation enabled flag bit.
pub const FH_SEG_ENABLED: u32 = 1 << 10;

/// Segmentation map update flag bit.
pub const FH_SEG_UPDATE_MAP: u32 = 1 << 11;

/// Segmentation temporal update flag bit.
pub const FH_SEG_TEMPORAL_UPDATE: u32 = 1 << 12;

/// Segmentation data update flag bit.
pub const FH_SEG_UPDATE_DATA: u32 = 1 << 13;

/// Segmentation absolute delta flag bit.
pub const FH_SEG_ABS_DELTA: u32 = 1 << 14;

/// Loop filter delta enabled flag bit.
pub const FH_LF_DELTA_ENABLED: u32 = 1 << 15;

/// Loop filter delta update flag bit.
pub const FH_LF_DELTA_UPDATE: u32 = 1 << 16;
/// VP9 frame type.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum FrameType {
    /// Key frame.
    #[default]
    Key,
    /// Inter frame.
    Inter,
}

impl FrameType {
    /// True for key frames.
    #[must_use]
    pub fn is_intra(self) -> bool {
        matches!(self, Self::Key)
    }
}

/// Parsed VP9 frame header.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FrameHeader {
    /// Packed frame flags.
    pub flags: u32,
    /// Frame type.
    pub frame_type: FrameType,
    /// Profile id.
    pub profile: u8,
    /// Bit depth.
    pub bit_depth: u8,
    /// Color space id.
    pub color_space: u8,
    /// Color range id.
    pub color_range: u8,
    /// Shown slot for existing-frame display.
    pub frame_to_show_map_idx: u8,
    /// Frame width in luma samples.
    pub frame_width: u32,
    /// Frame height in luma samples.
    pub frame_height: u32,
    /// Render width in luma samples.
    pub render_width: u32,
    /// Render height in luma samples.
    pub render_height: u32,
    /// Refresh flags per slot.
    pub refresh_frame_flags: u8,
    /// Reference slot per position.
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    /// Reference sign bias per frame.
    pub ref_frame_sign_bias: [u8; 4],
    /// Frame context reset mode.
    pub reset_frame_context: u8,
    /// Frame context id.
    pub frame_context_idx: u8,
    /// Interpolation filter id.
    pub interp_filter: u8,
    /// Loop filter level.
    pub filter_level: u8,
    /// Loop filter sharpness.
    pub sharpness: u8,
    /// Loop filter reference deltas.
    pub ref_deltas: [i8; 4],
    /// Loop filter mode deltas.
    pub mode_deltas: [i8; 2],
    /// Base quantizer index.
    pub base_q_idx: u8,
    /// Y DC delta.
    pub delta_q_y_dc: i8,
    /// UV DC delta.
    pub delta_q_uv_dc: i8,
    /// UV AC delta.
    pub delta_q_uv_ac: i8,
    /// Segmentation tree probabilities.
    pub tree_probs: [u8; TREE_PROBS],
    /// Segmentation prediction probabilities.
    pub pred_probs: [u8; PRED_PROBS],
    /// Segmentation feature enables per segment and level.
    pub seg_enabled: [[bool; SEG_LEVELS]; MAX_SEGMENTS],
    /// Segmentation feature values per segment and level.
    pub seg_value: [[i16; SEG_LEVELS]; MAX_SEGMENTS],
    /// Tile column shift.
    pub tile_cols_log2: u8,
    /// Tile row shift.
    pub tile_rows_log2: u8,
    /// Compressed header size in bytes.
    pub header_size_in_bytes: u16,
    /// Uncompressed header size in bytes.
    pub uncompressed_size: u16,
}

impl FrameHeader {
    /// Flag `flag` raised when `on`.
    pub(crate) fn set(&mut self, flag: u32, on: bool) {
        if on {
            self.flags |= flag;
        }
    }

    /// Existing-frame display.
    #[must_use]
    pub const fn show_existing_frame(&self) -> bool {
        self.flags & FH_SHOW_EXISTING != 0
    }

    /// Frame display.
    #[must_use]
    pub const fn show_frame(&self) -> bool {
        self.flags & FH_SHOW_FRAME != 0
    }

    /// Error resilient mode.
    #[must_use]
    pub const fn error_resilient_mode(&self) -> bool {
        self.flags & FH_ERROR_RESILIENT != 0
    }

    /// Intra-only frame.
    #[must_use]
    pub const fn intra_only(&self) -> bool {
        self.flags & FH_INTRA_ONLY != 0
    }

    /// Reference context refresh.
    #[must_use]
    pub const fn refresh_frame_context(&self) -> bool {
        self.flags & FH_REFRESH_CONTEXT != 0
    }

    /// Parallel decode mode.
    #[must_use]
    pub const fn frame_parallel_decoding_mode(&self) -> bool {
        self.flags & FH_FRAME_PARALLEL != 0
    }

    /// High precision motion vectors.
    #[must_use]
    pub const fn allow_high_precision_mv(&self) -> bool {
        self.flags & FH_HIGH_PRECISION_MV != 0
    }

    /// Render size against the frame size.
    #[must_use]
    pub const fn render_differs(&self) -> bool {
        self.flags & FH_RENDER_DIFFERS != 0
    }

    /// Horizontal subsampling.
    #[must_use]
    pub const fn subsampling_x(&self) -> bool {
        self.flags & FH_SUBSAMPLING_X != 0
    }

    /// Vertical subsampling.
    #[must_use]
    pub const fn subsampling_y(&self) -> bool {
        self.flags & FH_SUBSAMPLING_Y != 0
    }

    /// Segmentation enabled.
    #[must_use]
    pub const fn segmentation_enabled(&self) -> bool {
        self.flags & FH_SEG_ENABLED != 0
    }

    /// Segmentation map update.
    #[must_use]
    pub const fn segmentation_update_map(&self) -> bool {
        self.flags & FH_SEG_UPDATE_MAP != 0
    }

    /// Segmentation temporal update.
    #[must_use]
    pub const fn segmentation_temporal_update(&self) -> bool {
        self.flags & FH_SEG_TEMPORAL_UPDATE != 0
    }

    /// Segmentation absolute delta.
    #[must_use]
    pub const fn segmentation_abs_delta(&self) -> bool {
        self.flags & FH_SEG_ABS_DELTA != 0
    }

    /// Loop filter delta enabled.
    #[must_use]
    pub const fn loop_filter_delta_enabled(&self) -> bool {
        self.flags & FH_LF_DELTA_ENABLED != 0
    }

    /// Loop filter delta update.
    #[must_use]
    pub const fn loop_filter_delta_update(&self) -> bool {
        self.flags & FH_LF_DELTA_UPDATE != 0
    }
}

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

/// One coded frame: header plus bytes plus tiles in tile order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Parsed frame header.
    pub header: FrameHeader,
    /// Coded frame bytes behind driver submission.
    pub bytes: Vec<u8>,
    /// Tile payloads in tile order.
    pub tiles: Vec<TileData>,
}
