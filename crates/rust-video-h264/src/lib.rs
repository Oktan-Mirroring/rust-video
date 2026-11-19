//! H.264 grammar: sets, slices, and picture description.
//!
//! One parser behind every tier, generic over the tier error.

mod bits;
mod derive;
mod error;
mod nal;
mod picture;
mod pps;
mod scaling;
mod slice;
mod sps;
#[cfg(test)]
mod tests;
mod vui;

pub use bits::BitReader;
pub use derive::{
    abs_frame_num, apply_ref_mods, frame_num_wrap, initial_ref_lists, mmco_short_target, poc_msb,
    poc_type2, RefCandidate,
};
pub use error::{
    H264Error, H264Gate, NalAction, ParseKind, PermissiveGate, StrictGate, UnsupportedKind,
};
pub use nal::{
    is_idr, nal_ref_idc, nal_unit_type, rbsp, rbsp_prefix, split_nals, Crop, Nalu, NaluSplitter,
    HEADER_PREFIX, MAX_PPS, MAX_SPS,
};
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
