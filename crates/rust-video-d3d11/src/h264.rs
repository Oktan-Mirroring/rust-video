//! H.264 Annex B ingest over the shared grammar.

use rust_video_h264::{H264Error, ParseKind, StrictGate, UnsupportedKind};

use crate::error::{D3d11Error, ParseReason, UnsupportedReason};

pub(crate) use rust_video_h264::{
    Crop, Marking, Mmco, Picture, PocType, Pps, RefMod, SliceHeader, SlicePoc, SliceType, Sps,
    Weights,
};

#[cfg(test)]
pub(crate) use rust_video_h264::{FidelityRange, PendingSlice, ScalingSets, PPS_DEBLOCKING};

/// Annex B ingest over the tier gate.
pub(crate) type Ingest = rust_video_h264::Ingest<D3d11Error, StrictGate>;

impl H264Error for D3d11Error {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Parse(ParseReason::Truncated { syntax }),
            ParseKind::OverflowU32 { syntax } => Self::Parse(ParseReason::OverflowU32 { syntax }),
            ParseKind::OverflowI32 { syntax } => Self::Parse(ParseReason::OverflowI32 { syntax }),
            ParseKind::TooWide { syntax } => Self::Parse(ParseReason::TooWide { syntax }),
            ParseKind::Range { syntax } => Self::Parse(ParseReason::Range { syntax }),
            ParseKind::NalForbiddenBit => Self::Parse(ParseReason::NalForbiddenBit),
            ParseKind::SpsWithoutBody => Self::Parse(ParseReason::SpsWithoutBody),
            ParseKind::PpsWithoutBody => Self::Parse(ParseReason::PpsWithoutBody),
            ParseKind::SliceWithoutHeader => Self::Parse(ParseReason::SliceWithoutHeader),
            ParseKind::SliceHeaderTooLong => Self::Parse(ParseReason::SliceHeaderTooLong),
            ParseKind::PpsCitesUnknownSps => Self::Parse(ParseReason::PpsCitesUnknownSps),
            ParseKind::SliceCitesUnknownPps => Self::Parse(ParseReason::SliceCitesUnknownPps),
            ParseKind::SliceCitesUnknownSps => Self::Parse(ParseReason::SliceCitesUnknownSps),
            ParseKind::PictureSlicesDisagree => Self::Parse(ParseReason::PictureSlicesDisagree),
            ParseKind::NonIntraSliceInIdr => Self::Parse(ParseReason::NonIntraSliceInIdr),
            ParseKind::FrameCropOutsideCodedSize => {
                Self::Parse(ParseReason::FrameCropOutsideCodedSize)
            }
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::Interlaced => Self::Unsupported(UnsupportedReason::Interlaced),
            UnsupportedKind::ChromaOutside420 => {
                Self::Unsupported(UnsupportedReason::ChromaOutside420)
            }
            UnsupportedKind::LumaDepthAbove8Bit => {
                Self::Unsupported(UnsupportedReason::LumaDepthAbove8Bit)
            }
            UnsupportedKind::ChromaDepthAbove8Bit => {
                Self::Unsupported(UnsupportedReason::ChromaDepthAbove8Bit)
            }
            UnsupportedKind::TransformBypass => {
                Self::Unsupported(UnsupportedReason::TransformBypass)
            }
            UnsupportedKind::FlexibleMacroblockOrder => {
                Self::Unsupported(UnsupportedReason::FlexibleMacroblockOrder)
            }
            UnsupportedKind::DataPartitions => Self::Unsupported(UnsupportedReason::DataPartitions),
            UnsupportedKind::Multiview => Self::Unsupported(UnsupportedReason::Multiview),
            UnsupportedKind::SpSiSlices => Self::Unsupported(UnsupportedReason::SpSiSlices),
            UnsupportedKind::RedundantPictures => {
                Self::Unsupported(UnsupportedReason::RedundantPictures)
            }
            UnsupportedKind::ArbitrarySliceOrder => {
                Self::Unsupported(UnsupportedReason::ArbitrarySliceOrder)
            }
        }
    }
}
