//! H.264 grammar: sets, slices, and picture description.
//!
//! One parser behind every tier, generic over the tier error.

mod bits;
mod error;
mod nal;
mod picture;
mod pps;
mod scaling;
mod slice;
mod sps;
mod vui;

#[cfg(test)]
pub(crate) mod test_support;

pub use bits::BitReader;
pub use error::{
    H264Error, H264Gate, NalAction, ParseKind, PermissiveGate, StrictGate, UnsupportedKind,
};
pub use nal::{rbsp, rbsp_prefix, split_nals, Crop, HEADER_PREFIX, MAX_PPS, MAX_SPS};
pub use picture::{Ingest, PendingSlice, Picture};
pub use pps::{
    parse_pps, parse_pps_ids, Pps, PPS_CONSTRAINED_INTRA, PPS_DEBLOCKING, PPS_ENTROPY,
    PPS_PIC_ORDER, PPS_REDUNDANT, PPS_TRANSFORM_8X8, PPS_WEIGHTED_PRED,
};
pub use scaling::{ScalingMap, ScalingSets};
pub use slice::{
    parse_slice_prefix, parse_slice_rest, Marking, Mmco, RefMod, SliceHeader, SlicePoc, SliceType,
    WeightEntry, Weights,
};
pub use sps::{parse_sps, FidelityRange, PocType, Sps};
pub use vui::{
    AspectRatio, ChromaLoc, ColourDesc, HrdParams, Overscan, TimingInfo, VideoSignal, VuiParams,
};

/// H.264 codec namespace.
pub struct H264;
