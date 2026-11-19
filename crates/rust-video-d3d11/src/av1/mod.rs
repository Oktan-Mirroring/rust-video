//! AV1 decode over the D3D11 video engine.

mod decoder;
mod dpb;
mod frontend;

pub use decoder::Av1Decoder;
pub(crate) use dpb::SubmittedAv1;
#[cfg(test)]
pub(crate) use frontend::{
    FilmGrain, GlobalMotion, TileData, FG_APPLY, FG_OVERLAP, FH_SHOW_EXISTING, FH_SHOW_FRAME,
    SEQ_CDEF, SEQ_ORDER_HINT, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y,
};
pub(crate) use frontend::{
    Frame, FrameHeader, FrameType, Ingest, SequenceHeader, MAX_SEGMENTS, NUM_REF_FRAMES,
    REFS_PER_FRAME, SEG_LEVELS,
};
