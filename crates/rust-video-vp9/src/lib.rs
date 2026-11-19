//! VP9 grammar: uncompressed headers plus tile assembly behind every tier.
//!
//! Parse faults surface as [`Fault`]; tiers map it into their own errors.

mod bits;
mod error;
mod frame;
mod picture;
#[cfg(test)]
mod tests;

pub use bits::BitReader;
pub use error::Fault;
pub use frame::{
    Frame, FrameHeader, FrameType, TileData, FH_ERROR_RESILIENT, FH_FRAME_PARALLEL,
    FH_HIGH_PRECISION_MV, FH_INTRA_ONLY, FH_LF_DELTA_ENABLED, FH_LF_DELTA_UPDATE,
    FH_REFRESH_CONTEXT, FH_RENDER_DIFFERS, FH_SEG_ABS_DELTA, FH_SEG_ENABLED,
    FH_SEG_TEMPORAL_UPDATE, FH_SEG_UPDATE_DATA, FH_SEG_UPDATE_MAP, FH_SHOW_EXISTING, FH_SHOW_FRAME,
    FH_SUBSAMPLING_X, FH_SUBSAMPLING_Y, MAX_SEGMENTS, NUM_REF_FRAMES, REFS_PER_FRAME, SEG_LEVELS,
};
pub use picture::Ingest;

/// VP9 codec namespace.
pub struct Vp9;
