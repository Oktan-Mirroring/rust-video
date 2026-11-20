//! H.265 grammar: sets, slices, and picture description.
//!
//! One parser behind every tier, generic over the tier error.

mod bits;
mod error;
mod framing;
mod hrd;
mod picture;
mod pps;
mod ptl;
mod rps;
mod scaling;
mod slice;
mod sps;
#[cfg(test)]
mod tests;
mod vps;
mod vui;

pub use bits::BitReader;
pub use error::{
    H265Error, H265Gate, HevcError, HevcGate, NalAction, ParseKind, ParseReason, PermissiveGate,
    StrictGate, UnsupportedKind, UnsupportedReason,
};
pub use framing::{
    anchors_tid0, is_bla, is_cra, is_cra_bla, is_idr, is_irap, is_radl, is_rasl, is_slnr,
    nal_unit_type, rbsp, rbsp_prefix, split_nals, temporal_id, Crop, Nalu, NaluSplitter,
    HEADER_PREFIX, MAX_LT_SPS, MAX_PPS, MAX_RPS, MAX_RPS_PICS, MAX_SPS,
};
pub use hrd::{HrdLayerData, HrdParams, HrdStreams};
pub use picture::{Ingest, PendingSlice, Picture};
pub use pps::{
    parse_pps, parse_pps_ids, Pps, PPS_CABAC_INIT, PPS_CHROMA_OFFSETS, PPS_CONSTRAINED_INTRA,
    PPS_CU_QP_DELTA, PPS_DEBLOCK_DISABLED, PPS_DEBLOCK_OVERRIDE, PPS_DEPENDENT, PPS_LISTS_MOD,
    PPS_LOOP_ACROSS_SLICES, PPS_LOOP_ACROSS_TILES, PPS_OUTPUT_FLAG, PPS_SCALING, PPS_SIGN_HIDING,
    PPS_SLICE_EXT, PPS_SYNC, PPS_TILES, PPS_TRANSFORM_SKIP, PPS_TRANSQUANT_BYPASS,
    PPS_UNIFORM_SPACING, PPS_WEIGHTED_BIPRED, PPS_WEIGHTED_PRED,
};
pub use ptl::{ConstraintFlags, ProfileTierLevel, SourceFlags, SubPtl};
pub use rps::{parse_short_term_rps, ResolvedRps, ShortTermRps};
pub use scaling::ScalingLists;
pub use slice::{
    parse_slice_prefix, parse_slice_rest, parse_slice_tail, LtSpec, RpsBits, SliceHeader,
    SliceTail, SliceType, SliceWeights, WeightEntry,
};
pub use sps::{
    parse_sps, Sps, SPS_AMP, SPS_LONG_TERM, SPS_NESTING, SPS_ORDERING_INFO, SPS_PCM,
    SPS_PCM_LOOP_DISABLED, SPS_SAO, SPS_SCALING, SPS_SMOOTHING, SPS_TEMPORAL_MVP,
};
pub use vps::{parse_vps, Vps, VpsTiming};
pub use vui::{
    parse_vui, AspectRatio, BitstreamRestriction, ChromaLoc, Overscan, VideoSignal, VuiParams,
    VuiTiming,
};

/// H.265 codec namespace.
pub struct H265;
