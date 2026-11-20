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
use rust_video_caps::reason::{
    BrokenReferenceReason as SharedBroken, ParseReason as SharedParse,
    UnsupportedReason as SharedUnsupported,
};

impl From<Av1Error> for D3d12Error {
    fn from(error: Av1Error) -> Self {
        match error {
            Av1Error::Parse(reason) => Self::Parse(ParseReason::Parse(match reason {
                Av1Parse::FrameBeforeSequenceHeader => SharedParse::FrameBeforeSequenceHeader,
                Av1Parse::FrameWithoutTileData => SharedParse::FrameWithoutTileData,
                Av1Parse::IncompleteTileGroup => SharedParse::IncompleteTileGroup,
                Av1Parse::ObuExtensionReservedBits => SharedParse::ObuExtensionReservedBits,
                Av1Parse::ObuForbiddenBit => SharedParse::ObuForbiddenBit,
                Av1Parse::ObuReservedBit => SharedParse::ObuReservedBit,
                Av1Parse::ObuSizeExceedsPayload => SharedParse::ObuSizeExceedsPayload,
                Av1Parse::OverflowI32 { syntax } => SharedParse::OverflowI32 { syntax },
                Av1Parse::OverflowU32 { syntax } => SharedParse::OverflowU32 { syntax },
                Av1Parse::Range { syntax } => SharedParse::Range { syntax },
                Av1Parse::RedundantHeaderMismatch => SharedParse::RedundantHeaderMismatch,
                Av1Parse::ReservedObuType => SharedParse::ReservedObuType,
                Av1Parse::TileGridCoverage => SharedParse::TileGridCoverage,
                Av1Parse::TileGroupOrder => SharedParse::TileGroupOrder,
                Av1Parse::TileGroupWithoutData => SharedParse::TileGroupWithoutData,
                Av1Parse::TileGroupWithoutFrameHeader => SharedParse::TileGroupWithoutFrameHeader,
                Av1Parse::TooWide { syntax } => SharedParse::TooWide { syntax },
                Av1Parse::Truncated { syntax } => SharedParse::Truncated { syntax },
            })),
            Av1Error::Unsupported(reason) => {
                Self::Unsupported(UnsupportedReason::Stream(match reason {
                    Av1Unsupported::Av1BitDepthNeedsTarget { depth } => {
                        SharedUnsupported::Av1BitDepthNeedsTarget { depth }
                    }
                    Av1Unsupported::Av1ChromaOutside420 => SharedUnsupported::Av1ChromaOutside420,
                    Av1Unsupported::Av1Monochrome => SharedUnsupported::Av1Monochrome,
                    Av1Unsupported::RenderSizeDiffersFromFrameSize => {
                        SharedUnsupported::RenderSizeDiffersFromFrameSize
                    }
                    Av1Unsupported::SpatialLayers => SharedUnsupported::SpatialLayers,
                    Av1Unsupported::SuperresUpscale => SharedUnsupported::SuperresUpscale,
                    Av1Unsupported::TileCountAbove256 => SharedUnsupported::TileCountAbove256,
                    Av1Unsupported::TileList => SharedUnsupported::TileList,
                }))
            }
            Av1Error::BrokenReference(reason) => {
                Self::BrokenReference(BrokenReferenceReason::Broken(match reason {
                    Av1Broken::NamesEmptySlot { reference } => {
                        SharedBroken::NamesEmptySlot { reference }
                    }
                }))
            }
        }
    }
}
