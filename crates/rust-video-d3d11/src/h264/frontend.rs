//! H.264 Annex B ingest over the shared grammar.

use rust_video_caps::reason::{ParseReason as ParseCore, UnsupportedReason as StreamCore};
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
            ParseKind::Truncated { syntax } => {
                Self::Parse(ParseReason::Parse(ParseCore::Truncated { syntax }))
            }
            ParseKind::OverflowU32 { syntax } => {
                Self::Parse(ParseReason::Parse(ParseCore::OverflowU32 { syntax }))
            }
            ParseKind::OverflowI32 { syntax } => {
                Self::Parse(ParseReason::Parse(ParseCore::OverflowI32 { syntax }))
            }
            ParseKind::TooWide { syntax } => {
                Self::Parse(ParseReason::Parse(ParseCore::TooWide { syntax }))
            }
            ParseKind::Range { syntax } => {
                Self::Parse(ParseReason::Parse(ParseCore::Range { syntax }))
            }
            ParseKind::NalForbiddenBit => {
                Self::Parse(ParseReason::Parse(ParseCore::NalForbiddenBit))
            }
            ParseKind::SpsWithoutBody => Self::Parse(ParseReason::Parse(ParseCore::SpsWithoutBody)),
            ParseKind::PpsWithoutBody => Self::Parse(ParseReason::Parse(ParseCore::PpsWithoutBody)),
            ParseKind::SliceWithoutHeader => {
                Self::Parse(ParseReason::Parse(ParseCore::SliceWithoutHeader))
            }
            ParseKind::SliceHeaderTooLong => {
                Self::Parse(ParseReason::Parse(ParseCore::SliceHeaderTooLong))
            }
            ParseKind::PpsCitesUnknownSps => {
                Self::Parse(ParseReason::Parse(ParseCore::PpsCitesUnknownSps))
            }
            ParseKind::SliceCitesUnknownPps => {
                Self::Parse(ParseReason::Parse(ParseCore::SliceCitesUnknownPps))
            }
            ParseKind::SliceCitesUnknownSps => {
                Self::Parse(ParseReason::Parse(ParseCore::SliceCitesUnknownSps))
            }
            ParseKind::PictureSlicesDisagree => {
                Self::Parse(ParseReason::Parse(ParseCore::PictureSlicesDisagree))
            }
            ParseKind::NonIntraSliceInIdr => {
                Self::Parse(ParseReason::Parse(ParseCore::NonIntraSliceInIdr))
            }
            ParseKind::FrameCropOutsideCodedSize => {
                Self::Parse(ParseReason::Parse(ParseCore::FrameCropOutsideCodedSize))
            }
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::Interlaced => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::Interlaced))
            }
            UnsupportedKind::ChromaOutside420 => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::ChromaOutside420))
            }
            UnsupportedKind::LumaDepthAbove8Bit => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::LumaDepthAbove8Bit))
            }
            UnsupportedKind::ChromaDepthAbove8Bit => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::ChromaDepthAbove8Bit))
            }
            UnsupportedKind::TransformBypass => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::TransformBypass))
            }
            UnsupportedKind::FlexibleMacroblockOrder => Self::Unsupported(
                UnsupportedReason::Stream(StreamCore::FlexibleMacroblockOrder),
            ),
            UnsupportedKind::DataPartitions => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::DataPartitions))
            }
            UnsupportedKind::Multiview => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::Multiview))
            }
            UnsupportedKind::SpSiSlices => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::SpSiSlices))
            }
            UnsupportedKind::RedundantPictures => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::RedundantPictures))
            }
            UnsupportedKind::ArbitrarySliceOrder => {
                Self::Unsupported(UnsupportedReason::Stream(StreamCore::ArbitrarySliceOrder))
            }
        }
    }
}
