//! H.265 grammar: sets, slices, and picture description.
//!
//! One parser behind every tier, generic over the tier error.

mod bits;
mod error;
mod hrd;
mod nal;
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
pub use error::{HevcError, HevcGate, NalAction, ParseKind, PermissiveGate, UnsupportedKind};
pub use hrd::{HrdLayerData, HrdParams, HrdStreams};
pub use nal::{
    is_cra_bla, is_idr, is_irap, is_rasl, rbsp, rbsp_prefix, split_nals, Crop, HEADER_PREFIX,
    MAX_LT_SPS, MAX_PPS, MAX_RPS, MAX_RPS_PICS, MAX_SPS,
};
pub use picture::{Ingest, PendingSlice, Picture};
pub use pps::{
    parse_pps, Pps, PPS_CABAC_INIT, PPS_CHROMA_OFFSETS, PPS_CONSTRAINED_INTRA, PPS_CU_QP_DELTA,
    PPS_DEBLOCK_DISABLED, PPS_DEBLOCK_OVERRIDE, PPS_DEPENDENT, PPS_LISTS_MOD,
    PPS_LOOP_ACROSS_SLICES, PPS_LOOP_ACROSS_TILES, PPS_OUTPUT_FLAG, PPS_SCALING, PPS_SIGN_HIDING,
    PPS_SLICE_EXT, PPS_SYNC, PPS_TILES, PPS_TRANSFORM_SKIP, PPS_TRANSQUANT_BYPASS,
    PPS_UNIFORM_SPACING, PPS_WEIGHTED_BIPRED, PPS_WEIGHTED_PRED,
};
pub use ptl::{ConstraintFlags, ProfileTierLevel, SourceFlags, SubPtl};
pub use rps::{parse_short_term_rps, ResolvedRps, ShortTermRps};
pub use scaling::ScalingLists;
pub use slice::{parse_slice_prefix, parse_slice_rest, LtSpec, RpsBits, SliceHeader, SliceType};
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
