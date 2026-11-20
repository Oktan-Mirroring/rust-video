//! H.264 Annex B ingest over the shared grammar.

use rust_video_caps::reason::{ParseReason as SharedParse, UnsupportedReason as SharedUnsupported};
use rust_video_h264::{H264Error, ParseKind, StrictGate, UnsupportedKind};

use crate::error::{D3d12Error, ParseReason, UnsupportedReason};

pub(crate) use rust_video_h264::{Crop, Marking, Mmco, Picture, PocType, Pps, SlicePoc, Sps};

#[cfg(test)]
pub(crate) use rust_video_h264::{
    rbsp, split_nals, FidelityRange, PendingSlice, ScalingSets, SliceHeader, SliceType, VuiParams,
    Weights, PPS_DEBLOCKING,
};

/// Annex B ingest over the tier gate.
pub(crate) type Ingest = rust_video_h264::Ingest<D3d12Error, StrictGate>;

/// Sequence parameter set from RBSP `rbsp`.
#[cfg(test)]
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d12Error> {
    rust_video_h264::parse_sps::<D3d12Error, StrictGate>(rbsp)
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
#[cfg(test)]
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d12Error> {
    rust_video_h264::parse_pps::<D3d12Error, StrictGate>(rbsp, Some(sps))
}

impl H264Error for D3d12Error {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        Self::Parse(ParseReason::Parse(match kind {
            ParseKind::Truncated { syntax } => SharedParse::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } => SharedParse::OverflowU32 { syntax },
            ParseKind::OverflowI32 { syntax } => SharedParse::OverflowI32 { syntax },
            ParseKind::TooWide { syntax } => SharedParse::TooWide { syntax },
            ParseKind::Range { syntax } => SharedParse::Range { syntax },
            ParseKind::NalForbiddenBit => SharedParse::NalForbiddenBit,
            ParseKind::SpsWithoutBody => SharedParse::SpsWithoutBody,
            ParseKind::PpsWithoutBody => SharedParse::PpsWithoutBody,
            ParseKind::SliceWithoutHeader => SharedParse::SliceWithoutHeader,
            ParseKind::SliceHeaderTooLong => SharedParse::SliceHeaderTooLong,
            ParseKind::PpsCitesUnknownSps => SharedParse::PpsCitesUnknownSps,
            ParseKind::SliceCitesUnknownPps => SharedParse::SliceCitesUnknownPps,
            ParseKind::SliceCitesUnknownSps => SharedParse::SliceCitesUnknownSps,
            ParseKind::PictureSlicesDisagree => SharedParse::PictureSlicesDisagree,
            ParseKind::NonIntraSliceInIdr => SharedParse::NonIntraSliceInIdr,
            ParseKind::FrameCropOutsideCodedSize => SharedParse::FrameCropOutsideCodedSize,
        }))
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        Self::Unsupported(UnsupportedReason::Stream(match kind {
            UnsupportedKind::Interlaced => SharedUnsupported::Interlaced,
            UnsupportedKind::ChromaOutside420 => SharedUnsupported::ChromaOutside420,
            UnsupportedKind::LumaDepthAbove8Bit => SharedUnsupported::LumaDepthAbove8Bit,
            UnsupportedKind::ChromaDepthAbove8Bit => SharedUnsupported::ChromaDepthAbove8Bit,
            UnsupportedKind::TransformBypass => SharedUnsupported::TransformBypass,
            UnsupportedKind::FlexibleMacroblockOrder => SharedUnsupported::FlexibleMacroblockOrder,
            UnsupportedKind::DataPartitions => SharedUnsupported::DataPartitions,
            UnsupportedKind::Multiview => SharedUnsupported::Multiview,
            UnsupportedKind::SpSiSlices => SharedUnsupported::SpSiSlices,
            UnsupportedKind::RedundantPictures => SharedUnsupported::RedundantPictures,
            UnsupportedKind::ArbitrarySliceOrder => SharedUnsupported::ArbitrarySliceOrder,
        }))
    }
}
