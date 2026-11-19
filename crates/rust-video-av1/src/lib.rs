//! AV1 grammar shared by every decode tier.
//!
//! One OBU, sequence, and frame parser behind the tier error types.

mod bits;
mod error;
mod frame;
mod framing;
mod picture;
mod sequence;
mod syntax;
#[cfg(test)]
mod tests;

pub use bits::BitReader;
pub use error::{
    BrokenReferenceKind, BrokenReferenceReason, Error, ParseKind, ParseReason, UnsupportedKind,
    UnsupportedReason,
};
pub use frame::{
    parse_frame_header, refresh_refs, show_existing_refs, skip_mode_frames, FilmGrain, Frame,
    FrameHeader, FrameType, GlobalMotion, RefInfo, TileData, FG_APPLY, FG_CHROMA_FROM_LUMA,
    FG_OVERLAP, FG_RESTRICTED_RANGE, FH_ALLOW_INTRABC, FH_ALLOW_WARPED_MOTION, FH_DELTA_LF_MULTI,
    FH_DELTA_LF_PRESENT, FH_DELTA_Q_PRESENT, FH_DIFF_UV_DELTA, FH_DISABLE_CDF_UPDATE,
    FH_DISABLE_FRAME_END_UPDATE_CDF, FH_ERROR_RESILIENT, FH_FORCE_INTEGER_MV, FH_HIGH_PRECISION_MV,
    FH_LOOP_DELTA_ENABLED, FH_LOOP_DELTA_UPDATE, FH_MOTION_MODE_SWITCHABLE, FH_REDUCED_TX_SET,
    FH_REFERENCE_SELECT, FH_RENDER_DIFFERENT, FH_SCREEN_CONTENT_TOOLS, FH_SEG_ENABLED,
    FH_SEG_TEMPORAL_UPDATE, FH_SEG_UPDATE_DATA, FH_SEG_UPDATE_MAP, FH_SHORT_SIGNALING,
    FH_SHOWABLE_FRAME, FH_SHOW_EXISTING, FH_SHOW_FRAME, FH_SIZE_OVERRIDE, FH_SKIP_MODE_PRESENT,
    FH_USE_REF_MVS, FH_USING_QMATRIX, INTERP_SWITCHABLE, SUPERRES_DENOM_MIN,
};
pub use framing::{
    frame_obu_tile_offset, split_obus, split_tiles, tile_group_range, Obu, TileGroupHead,
    OBU_FRAME, OBU_FRAME_HEADER, OBU_METADATA, OBU_PADDING, OBU_REDUNDANT_FRAME_HEADER,
    OBU_SEQUENCE_HEADER, OBU_TEMPORAL_DELIMITER, OBU_TILE_GROUP, OBU_TILE_LIST,
};
pub use picture::Ingest;
pub use sequence::{
    parse_sequence_header, DecoderModelPoint, OperatingPoint, SequenceHeader, SEQ_CDEF,
    SEQ_CHOOSE_INTEGER_MV, SEQ_CHOOSE_SCREEN_CONTENT, SEQ_COLOR_DESCRIPTION, SEQ_COLOR_RANGE,
    SEQ_DECODER_MODEL_PRESENT, SEQ_DUAL_FILTER, SEQ_EQUAL_INTERVAL, SEQ_FILM_GRAIN,
    SEQ_FILTER_INTRA, SEQ_FRAME_ID_NUMBERS, SEQ_HIGH_BITDEPTH, SEQ_INITIAL_DISPLAY_PRESENT,
    SEQ_INTERINTRA, SEQ_INTRA_EDGE, SEQ_JNT_COMP, SEQ_MASKED_COMPOUND, SEQ_MONO_CHROME,
    SEQ_ORDER_HINT, SEQ_REDUCED_HEADER, SEQ_REF_MVS, SEQ_RESTORATION, SEQ_SEPARATE_UV_DELTA_Q,
    SEQ_STILL_PICTURE, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y, SEQ_SUPERBLOCK_128, SEQ_SUPERRES,
    SEQ_TIMING_PRESENT, SEQ_TWELVE_BIT, SEQ_WARPED_MOTION,
};
pub use syntax::{relative_dist, tile_log2};

/// Operating point count.
pub(crate) const MAX_OPERATING_POINTS: usize = 32;

/// Reference frame count.
pub const NUM_REF_FRAMES: usize = 8;

/// References per frame.
pub const REFS_PER_FRAME: usize = 7;

/// Segments per frame.
pub const MAX_SEGMENTS: usize = 8;

/// Segment levels per segment.
pub const SEG_LEVELS: usize = 8;

/// Reference deltas per frame.
pub const TOTAL_REFS: usize = 8;

/// Superres numerator.
pub const SUPERRES_NUM: u8 = 8;

/// Screen content tools selected per frame.
pub(crate) const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;

/// Integer motion vectors selected per frame.
pub(crate) const SELECT_INTEGER_MV: u8 = 2;

/// AV1 codec namespace.
pub struct Av1;
