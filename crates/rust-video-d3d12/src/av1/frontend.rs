//! AV1 grammar behind the shared parser.
//!
//! Tier error mapping over the shared grammar.

use crate::error::{BrokenReferenceReason, D3d12Error, ParseReason, UnsupportedReason};

pub(crate) use rust_video_av1::{
    Frame, FrameHeader, FrameType, Ingest, SequenceHeader, MAX_SEGMENTS, NUM_REF_FRAMES,
    REFS_PER_FRAME, SEG_LEVELS,
};

use rust_video_av1::{
    BrokenReferenceReason as Av1Broken, Error as Av1Error, ParseReason as Av1Parse,
    UnsupportedReason as Av1Unsupported,
};
/// Shared grain and flag vocabulary behind submit tests.
#[cfg(test)]
pub(crate) use rust_video_av1::{
    FilmGrain, GlobalMotion, TileData, FG_APPLY, FG_OVERLAP, FH_SHOW_EXISTING, FH_SHOW_FRAME,
    SEQ_CDEF, SEQ_ORDER_HINT, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y,
};

impl From<Av1Error> for D3d12Error {
    fn from(error: Av1Error) -> Self {
        match error {
            Av1Error::Parse(reason) => Self::Parse(match reason {
                Av1Parse::FrameBeforeSequenceHeader => ParseReason::FrameBeforeSequenceHeader,
                Av1Parse::FrameWithoutTileData => ParseReason::FrameWithoutTileData,
                Av1Parse::IncompleteTileGroup => ParseReason::IncompleteTileGroup,
                Av1Parse::ObuExtensionReservedBits => ParseReason::ObuExtensionReservedBits,
                Av1Parse::ObuForbiddenBit => ParseReason::ObuForbiddenBit,
                Av1Parse::ObuReservedBit => ParseReason::ObuReservedBit,
                Av1Parse::ObuSizeExceedsPayload => ParseReason::ObuSizeExceedsPayload,
                Av1Parse::OverflowI32 { syntax } => ParseReason::OverflowI32 { syntax },
                Av1Parse::OverflowU32 { syntax } => ParseReason::OverflowU32 { syntax },
                Av1Parse::Range { syntax } => ParseReason::Range { syntax },
                Av1Parse::RedundantHeaderMismatch => ParseReason::RedundantHeaderMismatch,
                Av1Parse::ReservedObuType => ParseReason::ReservedObuType,
                Av1Parse::TileGridCoverage => ParseReason::TileGridCoverage,
                Av1Parse::TileGroupOrder => ParseReason::TileGroupOrder,
                Av1Parse::TileGroupWithoutData => ParseReason::TileGroupWithoutData,
                Av1Parse::TileGroupWithoutFrameHeader => ParseReason::TileGroupWithoutFrameHeader,
                Av1Parse::TooWide { syntax } => ParseReason::TooWide { syntax },
                Av1Parse::Truncated { syntax } => ParseReason::Truncated { syntax },
            }),
            Av1Error::Unsupported(reason) => Self::Unsupported(match reason {
                Av1Unsupported::Av1BitDepthNeedsTarget { depth } => {
                    UnsupportedReason::Av1BitDepthNeedsTarget { depth }
                }
                Av1Unsupported::Av1ChromaOutside420 => UnsupportedReason::Av1ChromaOutside420,
                Av1Unsupported::Av1Monochrome => UnsupportedReason::Av1Monochrome,
                Av1Unsupported::RenderSizeDiffersFromFrameSize => {
                    UnsupportedReason::RenderSizeDiffersFromFrameSize
                }
                Av1Unsupported::SpatialLayers => UnsupportedReason::SpatialLayers,
                Av1Unsupported::SuperresUpscale => UnsupportedReason::SuperresUpscale,
                Av1Unsupported::TileCountAbove256 => UnsupportedReason::TileCountAbove256,
                Av1Unsupported::TileList => UnsupportedReason::TileList,
            }),
            Av1Error::BrokenReference(reason) => Self::BrokenReference(match reason {
                Av1Broken::NamesEmptySlot { reference } => {
                    BrokenReferenceReason::NamesEmptySlot { reference }
                }
            }),
        }
    }
}
