//! AV1 grammar behind the shared parser.
//!
//! Tier error mapping over the shared grammar.

use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason, UnsupportedReason};

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
    BrokenReferenceReason as BrokenCore, ParseReason as ParseCore, UnsupportedReason as StreamCore,
};

impl From<Av1Error> for D3d11Error {
    fn from(error: Av1Error) -> Self {
        match error {
            Av1Error::Parse(reason) => Self::Parse(match reason {
                Av1Parse::FrameBeforeSequenceHeader => {
                    ParseReason::Parse(ParseCore::FrameBeforeSequenceHeader)
                }
                Av1Parse::FrameWithoutTileData => {
                    ParseReason::Parse(ParseCore::FrameWithoutTileData)
                }
                Av1Parse::IncompleteTileGroup => ParseReason::Parse(ParseCore::IncompleteTileGroup),
                Av1Parse::ObuExtensionReservedBits => {
                    ParseReason::Parse(ParseCore::ObuExtensionReservedBits)
                }
                Av1Parse::ObuForbiddenBit => ParseReason::Parse(ParseCore::ObuForbiddenBit),
                Av1Parse::ObuReservedBit => ParseReason::Parse(ParseCore::ObuReservedBit),
                Av1Parse::ObuSizeExceedsPayload => {
                    ParseReason::Parse(ParseCore::ObuSizeExceedsPayload)
                }
                Av1Parse::OverflowI32 { syntax } => {
                    ParseReason::Parse(ParseCore::OverflowI32 { syntax })
                }
                Av1Parse::OverflowU32 { syntax } => {
                    ParseReason::Parse(ParseCore::OverflowU32 { syntax })
                }
                Av1Parse::Range { syntax } => ParseReason::Parse(ParseCore::Range { syntax }),
                Av1Parse::RedundantHeaderMismatch => {
                    ParseReason::Parse(ParseCore::RedundantHeaderMismatch)
                }
                Av1Parse::ReservedObuType => ParseReason::Parse(ParseCore::ReservedObuType),
                Av1Parse::TileGridCoverage => ParseReason::Parse(ParseCore::TileGridCoverage),
                Av1Parse::TileGroupOrder => ParseReason::Parse(ParseCore::TileGroupOrder),
                Av1Parse::TileGroupWithoutData => {
                    ParseReason::Parse(ParseCore::TileGroupWithoutData)
                }
                Av1Parse::TileGroupWithoutFrameHeader => {
                    ParseReason::Parse(ParseCore::TileGroupWithoutFrameHeader)
                }
                Av1Parse::TooWide { syntax } => ParseReason::Parse(ParseCore::TooWide { syntax }),
                Av1Parse::Truncated { syntax } => {
                    ParseReason::Parse(ParseCore::Truncated { syntax })
                }
            }),
            Av1Error::Unsupported(reason) => Self::Unsupported(match reason {
                Av1Unsupported::Av1BitDepthNeedsTarget { depth } => {
                    UnsupportedReason::Stream(StreamCore::Av1BitDepthNeedsTarget { depth })
                }
                Av1Unsupported::Av1ChromaOutside420 => {
                    UnsupportedReason::Stream(StreamCore::Av1ChromaOutside420)
                }
                Av1Unsupported::Av1Monochrome => {
                    UnsupportedReason::Stream(StreamCore::Av1Monochrome)
                }
                Av1Unsupported::RenderSizeDiffersFromFrameSize => {
                    UnsupportedReason::Stream(StreamCore::RenderSizeDiffersFromFrameSize)
                }
                Av1Unsupported::SpatialLayers => {
                    UnsupportedReason::Stream(StreamCore::SpatialLayers)
                }
                Av1Unsupported::SuperresUpscale => {
                    UnsupportedReason::Stream(StreamCore::SuperresUpscale)
                }
                Av1Unsupported::TileCountAbove256 => {
                    UnsupportedReason::Stream(StreamCore::TileCountAbove256)
                }
                Av1Unsupported::TileList => UnsupportedReason::Stream(StreamCore::TileList),
            }),
            Av1Error::BrokenReference(reason) => Self::BrokenReference(match reason {
                Av1Broken::NamesEmptySlot { reference } => {
                    BrokenReferenceReason::Broken(BrokenCore::NamesEmptySlot { reference })
                }
            }),
        }
    }
}
