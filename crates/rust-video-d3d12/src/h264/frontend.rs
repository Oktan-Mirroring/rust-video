//! H.264 Annex B ingest over the shared grammar.

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
