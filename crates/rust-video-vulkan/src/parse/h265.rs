//! H.265 Annex-B parameter sets and slice headers.
//!
//! Syntax per ITU-T H.265 section 7.3; the hardware parses slice bodies,
//! so the slice header stops after reference lists.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::bits::Rbsp;
use crate::parse::nal::Nalu as RawNalu;

use rust_video_h265::{HevcError, HevcGate, ParseKind, UnsupportedKind};

/// H.265 NAL unit type id.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NalType(u8);

impl NalType {
    /// True for coded slice segments.
    #[must_use]
    pub fn is_slice(self) -> bool {
        self.0 <= 21
    }

    /// True for IRAP pictures.
    #[must_use]
    pub fn is_irap(self) -> bool {
        (16..=21).contains(&self.0)
    }

    /// True for IDR pictures.
    #[cfg(test)]
    #[must_use]
    pub fn is_idr(self) -> bool {
        self.0 == 19 || self.0 == 20
    }

    /// Raw type id.
    #[must_use]
    pub fn id(self) -> u8 {
        self.0
    }
}

/// H.265 NAL header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NalHeader {
    /// Payload type.
    unit_type: NalType,
    /// Temporal id.
    temporal_id: u8,
}

impl NalHeader {
    /// Payload type.
    #[must_use]
    pub fn unit_type(self) -> NalType {
        self.unit_type
    }

    /// Temporal id.
    #[must_use]
    pub fn temporal_id(self) -> u8 {
        self.temporal_id
    }
}

/// Source flags of one profile record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SourceFlags {
    /// Progressive source.
    pub(crate) progressive: bool,
}

/// Packing constraint flags of one profile record.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConstraintFlags {
    /// Non-packed constraint.
    pub(crate) non_packed: bool,
    /// Frame-only constraint.
    pub(crate) frame_only: bool,
}

/// Profile, tier and level.
#[derive(Clone, Debug)]
pub(crate) struct ProfileTierLevel {
    /// Tier flag.
    tier: bool,
    /// Profile indicator.
    profile_idc: u8,
    /// Source flags.
    source: SourceFlags,
    /// Packing constraint flags.
    constraints: ConstraintFlags,
    /// Level indicator.
    level_idc: u8,
}

impl ProfileTierLevel {
    /// Profile indicator.
    #[must_use]
    pub fn profile_idc(&self) -> u8 {
        self.profile_idc
    }

    /// Level indicator.
    #[must_use]
    pub fn level_idc(&self) -> u8 {
        self.level_idc
    }

    /// Tier flag.
    #[must_use]
    pub fn tier(&self) -> bool {
        self.tier
    }

    /// Progressive source.
    #[must_use]
    pub fn progressive(&self) -> bool {
        self.source.progressive
    }

    /// Non-packed constraint.
    #[must_use]
    pub fn non_packed(&self) -> bool {
        self.constraints.non_packed
    }

    /// Frame-only constraint.
    #[must_use]
    pub fn frame_only(&self) -> bool {
        self.constraints.frame_only
    }
}

/// Checked narrowing of a parsed value to `u8`.
fn narrow_u8(value: u32, syntax: &'static str) -> Result<u8, ParseError> {
    u8::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// Checked narrowing of a parsed value to `u16`.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
}

/// VPS timing information.
#[derive(Clone, Debug, Default)]
pub(crate) struct VpsTiming {
    /// Ticks per second.
    pub(crate) num_units_in_tick: u32,
    /// Time scale.
    pub(crate) time_scale: u32,
    /// POC proportional to timing.
    pub(crate) poc_proportional: bool,
    /// Ticks per POC step minus 1.
    pub(crate) num_ticks_poc_diff_one_minus1: u32,
    /// HRD parameter sets.
    pub(crate) hrd: Vec<HrdParams>,
}

/// Video parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Vps {
    /// Own id.
    id: u8,
    /// Highest sub-layer index.
    max_sub_layers_minus1: u8,
    /// Temporal id nesting.
    temporal_nesting: bool,
    /// Profile, tier and level.
    ptl: ProfileTierLevel,
    /// Sub-layer ordering info present.
    ordering_info_present: bool,
    /// DPB capacity per sub-layer minus 1.
    max_dec_pic_buffering: Vec<u32>,
    /// Reorder depth per sub-layer.
    max_num_reorder_pics: Vec<u32>,
    /// Maximum latency per sub-layer.
    max_latency: Vec<u32>,
    /// Timing information, when present.
    timing: Option<VpsTiming>,
}

impl Vps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Highest sub-layer index.
    #[must_use]
    pub fn max_sub_layers_minus1(&self) -> u8 {
        self.max_sub_layers_minus1
    }

    /// Temporal id nesting.
    #[must_use]
    pub fn temporal_nesting(&self) -> bool {
        self.temporal_nesting
    }

    /// Profile, tier and level.
    #[must_use]
    pub fn ptl(&self) -> &ProfileTierLevel {
        &self.ptl
    }

    /// Sub-layer ordering info present.
    #[must_use]
    pub fn ordering_info_present(&self) -> bool {
        self.ordering_info_present
    }

    /// DPB capacity per sub-layer minus 1.
    #[must_use]
    pub fn max_dec_pic_buffering(&self) -> &[u32] {
        &self.max_dec_pic_buffering
    }

    /// Reorder depth per sub-layer.
    #[must_use]
    pub fn max_num_reorder_pics(&self) -> &[u32] {
        &self.max_num_reorder_pics
    }

    /// Maximum latency per sub-layer.
    #[must_use]
    pub fn max_latency(&self) -> &[u32] {
        &self.max_latency
    }

    /// Timing information, when present.
    #[must_use]
    pub fn timing(&self) -> Option<&VpsTiming> {
        self.timing.as_ref()
    }
}

/// Scaling lists for one parameter set.
#[derive(Clone, Debug)]
pub(crate) struct ScalingLists {
    /// 4x4 lists.
    pub(crate) list_4x4: [[u8; 16]; 6],
    /// 8x8 lists.
    pub(crate) list_8x8: [[u8; 64]; 6],
    /// 16x16 lists.
    pub(crate) list_16x16: [[u8; 64]; 6],
    /// 32x32 lists.
    pub(crate) list_32x32: [[u8; 64]; 2],
    /// 16x16 DC coefficients.
    pub(crate) dc_16x16: [u8; 6],
    /// 32x32 DC coefficients.
    pub(crate) dc_32x32: [u8; 2],
}

impl Default for ScalingLists {
    fn default() -> Self {
        Self {
            list_4x4: [[16; 16]; 6],
            list_8x8: [[16; 64]; 6],
            list_16x16: [[16; 64]; 6],
            list_32x32: [[16; 64]; 2],
            dc_16x16: [16; 6],
            dc_32x32: [16; 2],
        }
    }
}

/// One short-term reference picture set with derived POC deltas.
#[derive(Clone, Debug, Default)]
pub(crate) struct RefPicSet {
    /// Negative deltas with used-by-current flags, coding order.
    s0: Vec<(i32, bool)>,
    /// Positive deltas with used-by-current flags, coding order.
    s1: Vec<(i32, bool)>,
    /// Delta pictures of the predicted-from set.
    #[cfg(test)]
    ref_pics: u8,
    /// Inter-predicted from another set.
    inter: bool,
    /// Reference set index delta minus 1, slice sets only.
    delta_idx_minus1: u32,
    /// Delta RPS sign.
    delta_rps_sign: bool,
    /// Absolute delta RPS minus 1.
    abs_delta_rps_minus1: u16,
    /// Used-by-current mask over the reference set's pictures.
    used_mask: u16,
    /// Use-delta mask over the reference set's pictures.
    use_mask: u16,
}

impl RefPicSet {
    /// Delta pictures of the predicted-from set.
    #[cfg(test)]
    #[must_use]
    pub fn ref_pics(&self) -> u8 {
        self.ref_pics
    }

    /// Negative deltas with used-by-current flags, coding order.
    #[must_use]
    pub fn s0(&self) -> &[(i32, bool)] {
        &self.s0
    }

    /// Positive deltas with used-by-current flags, coding order.
    #[must_use]
    pub fn s1(&self) -> &[(i32, bool)] {
        &self.s1
    }

    /// Inter-predicted from another set.
    #[must_use]
    pub fn inter(&self) -> bool {
        self.inter
    }

    /// Reference set index delta minus 1, slice sets only.
    #[must_use]
    pub fn delta_idx_minus1(&self) -> u32 {
        self.delta_idx_minus1
    }

    /// Delta RPS sign.
    #[must_use]
    pub fn delta_rps_sign(&self) -> bool {
        self.delta_rps_sign
    }

    /// Absolute delta RPS minus 1.
    #[must_use]
    pub fn abs_delta_rps_minus1(&self) -> u16 {
        self.abs_delta_rps_minus1
    }

    /// Used-by-current mask over the reference set's pictures.
    #[must_use]
    pub fn used_mask(&self) -> u16 {
        self.used_mask
    }

    /// Use-delta mask over the reference set's pictures.
    #[must_use]
    pub fn use_mask(&self) -> u16 {
        self.use_mask
    }
}

/// Long-term reference picture from the SPS.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LtSps {
    /// POC least significant bits.
    pub(crate) poc_lsb: u32,
    /// Used by the current picture.
    pub(crate) used: bool,
}

/// PCM parameters of one sequence set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PcmParams {
    /// Luma PCM bit depth minus 1.
    pub(crate) luma_depth_minus1: u8,
    /// Chroma PCM bit depth minus 1.
    pub(crate) chroma_depth_minus1: u8,
    /// Minimum PCM block size minus 3.
    pub(crate) min_cb_minus3: u32,
    /// PCM block size range.
    pub(crate) diff_cb: u32,
    /// Loop filter disabled on PCM.
    pub(crate) loop_filter_disabled: bool,
}

/// HRD coded-picture-buffer data of one layer.
#[derive(Clone, Debug, Default)]
pub(crate) struct HrdLayer {
    /// Bitrate minus 1 per CPB.
    pub(crate) bitrate_minus1: Vec<u32>,
    /// CPB size minus 1 per CPB.
    pub(crate) cpb_size_minus1: Vec<u32>,
    /// DU CPB size minus 1 per CPB.
    pub(crate) du_cpb_size_minus1: Vec<u32>,
    /// DU bitrate minus 1 per CPB.
    pub(crate) du_bitrate_minus1: Vec<u32>,
    /// CBR flag per CPB.
    pub(crate) cbr: Vec<bool>,
}

/// HRD stream presence of one parameter set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HrdStreams {
    /// NAL HRD present.
    pub(crate) nal: bool,
    /// VCL HRD present.
    pub(crate) vcl: bool,
}

/// HRD parameters of one layer set.
#[derive(Clone, Debug, Default)]
pub(crate) struct HrdParams {
    /// Shared header coded in this set rather than inherited.
    pub(crate) common: bool,
    /// Stream presence flags.
    pub(crate) streams: HrdStreams,
    /// Sub-picture HRD.
    pub(crate) sub_pic: bool,
    /// Tick divisor minus 2.
    pub(crate) tick_divisor_minus2: u8,
    /// Bitrate scale.
    pub(crate) bitrate_scale: u8,
    /// CPB size scale.
    pub(crate) cpb_scale: u8,
    /// DU CPB size scale.
    pub(crate) du_cpb_scale: u8,
    /// Initial CPB removal delay length minus 1.
    pub(crate) initial_delay_minus1: u8,
    /// AU CPB removal delay length minus 1.
    pub(crate) au_delay_minus1: u8,
    /// DPB output delay length minus 1.
    pub(crate) dpb_delay_minus1: u8,
    /// Fixed picture rate per layer.
    pub(crate) fixed_rate: Vec<bool>,
    /// Picture duration per layer.
    pub(crate) pic_duration: Vec<bool>,
    /// Elemental duration minus 1 per layer.
    pub(crate) elemental: Vec<u16>,
    /// CPB count minus 1 per layer.
    pub(crate) cpb_count_minus1: Vec<u32>,
    /// NAL CPB data per layer.
    pub(crate) nal_layers: Vec<HrdLayer>,
    /// VCL CPB data per layer.
    pub(crate) vcl_layers: Vec<HrdLayer>,
}

/// VUI bitstream restriction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BitstreamRestriction {
    /// Tiles fixed structure.
    pub(crate) tiles_fixed: bool,
    /// Motion vectors over picture boundaries.
    pub(crate) mv_over_pic: bool,
    /// Restricted reference pictures.
    pub(crate) restricted_ref: bool,
    /// Minimum spatial segmentation idc.
    pub(crate) min_spatial_segmentation: u16,
    /// Maximum bytes per picture denominator.
    pub(crate) max_bytes_per_pic_denom: u8,
    /// Maximum bits per MCU denominator.
    pub(crate) max_bits_per_mcu_denom: u8,
    /// Horizontal motion vector length bits.
    pub(crate) max_mv_horizontal: u8,
    /// Vertical motion vector length bits.
    pub(crate) max_mv_vertical: u8,
}

/// Aspect-ratio block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AspectRatio {
    /// Aspect ratio present.
    pub(crate) present: bool,
    /// Aspect ratio idc.
    pub(crate) idc: u8,
    /// Sample aspect width.
    pub(crate) sar_width: u16,
    /// Sample aspect height.
    pub(crate) sar_height: u16,
}

/// Overscan block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Overscan {
    /// Overscan info present.
    pub(crate) present: bool,
    /// Overscan appropriate.
    pub(crate) appropriate: bool,
}

/// Video-signal block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VideoSignal {
    /// Video signal present.
    pub(crate) present: bool,
    /// Video format.
    pub(crate) format: u8,
    /// Full range.
    pub(crate) full_range: bool,
    /// Colour description present.
    pub(crate) colour_present: bool,
    /// Colour primaries.
    pub(crate) primaries: u8,
    /// Transfer characteristics.
    pub(crate) transfer: u8,
    /// Matrix coefficients.
    pub(crate) matrix: u8,
}

/// Chroma-location block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChromaLoc {
    /// Chroma location present.
    pub(crate) present: bool,
    /// Chroma top-field location.
    pub(crate) top: u32,
    /// Chroma bottom-field location.
    pub(crate) bottom: u32,
}

/// Timing block of one VUI record.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VuiTiming {
    /// Timing present.
    pub(crate) present: bool,
    /// Ticks per second.
    pub(crate) num_units_in_tick: u32,
    /// Time scale.
    pub(crate) time_scale: u32,
    /// POC proportional to timing.
    pub(crate) proportional: bool,
    /// Ticks per POC step minus 1.
    pub(crate) num_ticks_poc_diff_one_minus1: u32,
}

/// VUI parameters of one sequence set.
#[derive(Clone, Debug, Default)]
pub(crate) struct VuiParams {
    /// Aspect-ratio block.
    pub(crate) aspect: AspectRatio,
    /// Overscan block.
    pub(crate) overscan: Overscan,
    /// Video-signal block.
    pub(crate) signal: VideoSignal,
    /// Chroma-location block.
    pub(crate) chroma_loc: ChromaLoc,
    /// Neutral chroma indication.
    pub(crate) neutral_chroma: bool,
    /// Field sequence flag.
    pub(crate) field_seq: bool,
    /// Frame field info present.
    pub(crate) frame_field_info: bool,
    /// First default display window left, right, top, bottom.
    pub(crate) display_window: Option<[u32; 4]>,
    /// Timing block.
    pub(crate) timing: VuiTiming,
    /// HRD parameters, when present.
    pub(crate) hrd: Option<HrdParams>,
    /// Bitstream restriction, when present.
    pub(crate) restriction: Option<BitstreamRestriction>,
}

/// Sub-layer ordering limits of one sequence set.
#[derive(Clone, Debug, Default)]
pub(crate) struct SubLayerOrdering {
    /// Sub-layer ordering info present.
    pub(crate) info_present: bool,
    /// DPB capacity minus 1.
    pub(crate) buffering: Vec<u32>,
    /// Reorder depth per coded layer.
    pub(crate) reorder: Vec<u32>,
    /// Maximum latency per coded layer.
    pub(crate) latency: Vec<u32>,
}

/// Block coding tools of one sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockTools {
    /// Asymmetric motion partitions enabled.
    pub(crate) amp: bool,
    /// Sample-adaptive offset enabled.
    pub(crate) sao: bool,
    /// Scaling lists enabled.
    pub(crate) scaling: bool,
}

/// Prediction tools of one sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PredictionTools {
    /// Long-term pictures may appear in slices.
    pub(crate) long_term_present: bool,
    /// Temporal motion-vector prediction enabled.
    pub(crate) temporal_mvp: bool,
    /// Strong intra smoothing enabled.
    pub(crate) strong_intra_smoothing: bool,
}

/// Sequence parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Sps {
    /// Video set id.
    vps_id: u8,
    /// Highest sub-layer index.
    max_sub_layers_minus1: u8,
    /// Temporal id nesting.
    temporal_nesting: bool,
    /// Profile, tier and level.
    ptl: ProfileTierLevel,
    /// Own id.
    id: u8,
    /// Chroma sampling indicator.
    chroma_format_idc: u8,
    /// Separate colour plane coding.
    separate_colour_plane: bool,
    /// Picture width in luma samples.
    pic_width: u32,
    /// Picture height in luma samples.
    pic_height: u32,
    /// Conformance window offsets left, right, top, bottom.
    conformance: Option<[u32; 4]>,
    /// Luma bit depth minus 8.
    bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    bit_depth_chroma_minus8: u8,
    /// POC lsb bit width minus 4.
    log2_max_pic_order_cnt_lsb_minus4: u8,
    /// Sub-layer ordering limits.
    ordering: SubLayerOrdering,
    /// Minimum luma block size minus 3.
    log2_min_cb_minus3: u32,
    /// Luma block size range.
    log2_diff_cb: u32,
    /// Minimum transform size minus 2.
    log2_min_tb_minus2: u32,
    /// Transform size range.
    log2_diff_tb: u32,
    /// Maximum inter transform depth.
    max_inter_depth: u32,
    /// Maximum intra transform depth.
    max_intra_depth: u32,
    /// Block coding tools.
    block: BlockTools,
    /// Scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// PCM parameters, when enabled.
    pcm: Option<PcmParams>,
    /// Short-term reference picture sets.
    rps_sets: Vec<RefPicSet>,
    /// Long-term pictures from the SPS.
    long_term: Vec<LtSps>,
    /// Prediction tools.
    prediction: PredictionTools,
    /// VUI parameters, when present.
    vui: Option<VuiParams>,
    /// Shared set behind slice parsing.
    shared: Rc<rust_video_h265::Sps>,
}

impl Sps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Video set id.
    #[must_use]
    pub fn vps_id(&self) -> u8 {
        self.vps_id
    }

    /// Profile, tier and level.
    #[must_use]
    pub fn ptl(&self) -> &ProfileTierLevel {
        &self.ptl
    }

    /// Chroma sampling indicator.
    #[must_use]
    pub fn chroma_format_idc(&self) -> u8 {
        self.chroma_format_idc
    }

    /// Luma bit depth minus 8.
    #[must_use]
    pub fn bit_depth_luma_minus8(&self) -> u8 {
        self.bit_depth_luma_minus8
    }

    /// Chroma bit depth minus 8.
    #[must_use]
    pub fn bit_depth_chroma_minus8(&self) -> u8 {
        self.bit_depth_chroma_minus8
    }

    /// POC lsb bit width.
    #[must_use]
    pub fn pic_order_cnt_lsb_bits(&self) -> u32 {
        u32::from(self.log2_max_pic_order_cnt_lsb_minus4) + 4
    }

    /// DPB slot budget: capacity plus the picture under decode.
    #[must_use]
    pub fn dpb_slots(&self) -> u32 {
        self.max_dec_pic_buffering_minus1()
            .saturating_add(2)
            .min(16)
    }

    /// Reorder depth of the highest coded layer.
    #[must_use]
    pub fn max_num_reorder_pics(&self) -> u32 {
        self.ordering.reorder.last().copied().unwrap_or(0).min(16)
    }

    /// Short-term reference picture sets.
    #[must_use]
    pub fn rps_sets(&self) -> &[RefPicSet] {
        &self.rps_sets
    }

    /// Long-term pictures from the SPS.
    #[must_use]
    pub fn lt_sps(&self) -> &[LtSps] {
        &self.long_term
    }

    /// Long-term pictures may appear in slices.
    #[must_use]
    pub fn lt_present(&self) -> bool {
        self.prediction.long_term_present
    }

    /// Picture width in luma samples.
    #[must_use]
    pub fn pic_width(&self) -> u32 {
        self.pic_width
    }

    /// Picture height in luma samples.
    #[must_use]
    pub fn pic_height(&self) -> u32 {
        self.pic_height
    }

    /// POC lsb bit width minus 4.
    #[must_use]
    pub fn log2_max_pic_order_cnt_lsb_minus4(&self) -> u8 {
        self.log2_max_pic_order_cnt_lsb_minus4
    }

    /// Temporal motion-vector prediction enabled.
    #[must_use]
    pub fn temporal_mvp_enabled(&self) -> bool {
        self.prediction.temporal_mvp
    }

    /// Sample-adaptive offset enabled.
    #[must_use]
    pub fn sao_enabled(&self) -> bool {
        self.block.sao
    }

    /// Highest sub-layer index.
    #[must_use]
    pub fn max_sub_layers_minus1(&self) -> u8 {
        self.max_sub_layers_minus1
    }

    /// Temporal id nesting.
    #[must_use]
    pub fn temporal_nesting(&self) -> bool {
        self.temporal_nesting
    }

    /// Separate colour plane coding.
    #[must_use]
    pub fn separate_colour_plane(&self) -> bool {
        self.separate_colour_plane
    }

    /// Conformance window offsets left, right, top, bottom.
    #[must_use]
    pub fn conformance(&self) -> Option<[u32; 4]> {
        self.conformance
    }

    /// Crop left and top in luma samples, zero without a window.
    #[must_use]
    pub fn crop_origin(&self) -> (u32, u32) {
        let Some(window) = self.conformance else {
            return (0, 0);
        };
        let sub_width = if self.chroma_format_idc == 3 { 1 } else { 2 };
        let sub_height = if self.chroma_format_idc <= 1 { 2 } else { 1 };
        (
            window[0].saturating_mul(sub_width),
            window[2].saturating_mul(sub_height),
        )
    }

    /// Sub-layer ordering info present.
    #[must_use]
    pub fn ordering_info_present(&self) -> bool {
        self.ordering.info_present
    }

    /// DPB capacity minus 1 of the highest coded layer.
    #[must_use]
    pub fn max_dec_pic_buffering_minus1(&self) -> u32 {
        self.ordering.buffering.last().copied().unwrap_or(0)
    }

    /// DPB capacity minus 1 per coded layer.
    #[must_use]
    pub fn dec_pic_buffering(&self) -> &[u32] {
        &self.ordering.buffering
    }

    /// Reorder depth per coded layer.
    #[must_use]
    pub fn reorder_pics(&self) -> &[u32] {
        &self.ordering.reorder
    }

    /// Maximum latency per coded layer.
    #[must_use]
    pub fn max_latency(&self) -> &[u32] {
        &self.ordering.latency
    }

    /// Scaling lists enabled.
    #[must_use]
    pub fn scaling_enabled(&self) -> bool {
        self.block.scaling
    }

    /// Minimum luma block size minus 3.
    #[must_use]
    pub fn log2_min_cb_minus3(&self) -> u32 {
        self.log2_min_cb_minus3
    }

    /// Luma block size range.
    #[must_use]
    pub fn log2_diff_cb(&self) -> u32 {
        self.log2_diff_cb
    }

    /// Minimum transform size minus 2.
    #[must_use]
    pub fn log2_min_tb_minus2(&self) -> u32 {
        self.log2_min_tb_minus2
    }

    /// Transform size range.
    #[must_use]
    pub fn log2_diff_tb(&self) -> u32 {
        self.log2_diff_tb
    }

    /// Maximum inter transform depth.
    #[must_use]
    pub fn max_inter_depth(&self) -> u32 {
        self.max_inter_depth
    }

    /// Maximum intra transform depth.
    #[must_use]
    pub fn max_intra_depth(&self) -> u32 {
        self.max_intra_depth
    }

    /// Asymmetric motion partitions enabled.
    #[must_use]
    pub fn amp_enabled(&self) -> bool {
        self.block.amp
    }

    /// Scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }

    /// PCM parameters, when enabled.
    #[must_use]
    pub fn pcm(&self) -> Option<&PcmParams> {
        self.pcm.as_ref()
    }

    /// Strong intra smoothing enabled.
    #[must_use]
    pub fn strong_intra_smoothing_enabled(&self) -> bool {
        self.prediction.strong_intra_smoothing
    }

    /// VUI parameters, when present.
    #[must_use]
    pub fn vui(&self) -> Option<&VuiParams> {
        self.vui.as_ref()
    }

    /// Coded picture size in luma samples, window ignored.
    #[must_use]
    pub fn coded_size(&self) -> (u32, u32) {
        (self.pic_width, self.pic_height)
    }

    /// Display picture size in luma samples, window applied.
    #[must_use]
    pub fn display_size(&self) -> (u32, u32) {
        let Some(window) = self.conformance else {
            return (self.pic_width, self.pic_height);
        };
        let sub_width = if self.chroma_format_idc == 3 { 1 } else { 2 };
        let sub_height = if self.chroma_format_idc <= 1 { 2 } else { 1 };
        let width = self
            .pic_width
            .saturating_sub((window[0] + window[1]) * sub_width);
        let height = self
            .pic_height
            .saturating_sub((window[2] + window[3]) * sub_height);
        (width, height)
    }
}

/// Slice-segment header flags of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SliceHeaderFlags {
    /// Dependent slice segments allowed.
    pub(crate) dependent: bool,
    /// Picture output flag in slices.
    pub(crate) output_present: bool,
    /// Reference-list modification in slices.
    pub(crate) lists_modification: bool,
}

/// Intra coding tools of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IntraTools {
    /// Constrained intra prediction.
    pub(crate) constrained_pred: bool,
    /// Transform skip enabled.
    pub(crate) transform_skip: bool,
    /// Sign data hiding enabled.
    pub(crate) sign_hiding: bool,
}

/// QP delta tools of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QpTools {
    /// CU QP delta enabled.
    pub(crate) cu_delta: bool,
    /// CU QP delta depth.
    pub(crate) cu_depth: u32,
    /// Cb QP offset.
    pub(crate) cb_offset: i32,
    /// Cr QP offset.
    pub(crate) cr_offset: i32,
    /// Slice chroma QP offsets present.
    pub(crate) chroma_present: bool,
}

/// Weighted prediction flags of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WeightFlags {
    /// Weighted prediction on P slices.
    pub(crate) pred_p: bool,
    /// Weighted prediction on B slices.
    pub(crate) pred_b: bool,
}

/// Tile layout of one picture set.
#[derive(Clone, Debug, Default)]
pub(crate) struct TileLayout {
    /// Tiles enabled.
    pub(crate) enabled: bool,
    /// Entropy coding sync enabled.
    pub(crate) entropy_sync: bool,
    /// Tile column count minus 1.
    pub(crate) columns_minus1: u32,
    /// Tile row count minus 1.
    pub(crate) rows_minus1: u32,
    /// Uniform tile spacing.
    pub(crate) uniform: bool,
    /// Tile column widths minus 1.
    pub(crate) column_widths: Vec<u32>,
    /// Tile row heights minus 1.
    pub(crate) row_heights: Vec<u32>,
}

/// Loop-filter defaults of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LoopFilter {
    /// Loop filter across tiles enabled.
    pub(crate) across_tiles: bool,
    /// Loop filter across slices enabled.
    pub(crate) across_slices: bool,
}

/// Deblocking defaults of one picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeblockDefaults {
    /// Deblocking filter control present.
    pub(crate) control_present: bool,
    /// Deblocking override enabled in slices.
    pub(crate) override_enabled: bool,
    /// Deblocking disabled by default.
    pub(crate) disabled: bool,
    /// Beta offset divided by 2.
    pub(crate) beta: i32,
    /// Tc offset divided by 2.
    pub(crate) tc: i32,
}

/// Picture parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Pps {
    /// Own id.
    id: u8,
    /// Sequence set id.
    sps_id: u8,
    /// Slice-segment header flags.
    header: SliceHeaderFlags,
    /// Extra slice header bits.
    num_extra_slice_header_bits: u8,
    /// CABAC init flag in slices.
    cabac_init_present: bool,
    /// Default active L0 reference count minus 1.
    num_ref_idx_l0_default_minus1: u32,
    /// Default active L1 reference count minus 1.
    num_ref_idx_l1_default_minus1: u32,
    /// Initial QP minus 26.
    init_qp_minus26: i32,
    /// Intra coding tools.
    intra: IntraTools,
    /// QP delta tools.
    qp: QpTools,
    /// Weighted prediction flags.
    weights: WeightFlags,
    /// Transquant bypass enabled.
    transquant_bypass_enabled: bool,
    /// Tile layout.
    tiles: TileLayout,
    /// Loop-filter defaults.
    loop_filter: LoopFilter,
    /// Deblocking defaults.
    deblock: DeblockDefaults,
    /// Scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// Parallel merge level minus 2.
    log2_parallel_merge_level_minus2: u8,
    /// Slice header extension present.
    slice_header_extension_present: bool,
    /// Shared set behind slice parsing.
    shared: Rc<rust_video_h265::Pps>,
}

impl Pps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Sequence set id.
    #[must_use]
    pub fn sps_id(&self) -> u8 {
        self.sps_id
    }

    /// Picture output flag in slices.
    #[must_use]
    pub fn output_flag_present(&self) -> bool {
        self.header.output_present
    }

    /// Extra slice header bits.
    #[must_use]
    pub fn num_extra_slice_header_bits(&self) -> u8 {
        self.num_extra_slice_header_bits
    }

    /// CABAC init flag in slices.
    #[must_use]
    pub fn cabac_init_present(&self) -> bool {
        self.cabac_init_present
    }

    /// Default active L0 count minus 1.
    #[must_use]
    pub fn num_ref_idx_l0_default_minus1(&self) -> u32 {
        self.num_ref_idx_l0_default_minus1
    }

    /// Default active L1 count minus 1.
    #[must_use]
    pub fn num_ref_idx_l1_default_minus1(&self) -> u32 {
        self.num_ref_idx_l1_default_minus1
    }

    /// Weighted prediction on P slices.
    #[must_use]
    pub fn weighted_pred(&self) -> bool {
        self.weights.pred_p
    }

    /// Weighted prediction on B slices.
    #[must_use]
    pub fn weighted_bipred(&self) -> bool {
        self.weights.pred_b
    }

    /// Tiles enabled.
    #[must_use]
    pub fn tiles_enabled(&self) -> bool {
        self.tiles.enabled
    }

    /// Entropy coding sync enabled.
    #[must_use]
    pub fn entropy_coding_sync_enabled(&self) -> bool {
        self.tiles.entropy_sync
    }

    /// Deblocking filter control present.
    #[must_use]
    pub fn deblocking_filter_control_present(&self) -> bool {
        self.deblock.control_present
    }

    /// Deblocking override enabled in slices.
    #[must_use]
    pub fn deblocking_filter_override_enabled(&self) -> bool {
        self.deblock.override_enabled
    }

    /// Reference-list modification in slices.
    #[must_use]
    pub fn lists_modification_present(&self) -> bool {
        self.header.lists_modification
    }

    /// Slice header extension present.
    #[must_use]
    pub fn slice_header_extension_present(&self) -> bool {
        self.slice_header_extension_present
    }

    /// Initial QP minus 26.
    #[must_use]
    pub fn init_qp_minus26(&self) -> i32 {
        self.init_qp_minus26
    }

    /// Constrained intra prediction.
    #[must_use]
    pub fn constrained_intra_pred(&self) -> bool {
        self.intra.constrained_pred
    }

    /// Transform skip enabled.
    #[must_use]
    pub fn transform_skip_enabled(&self) -> bool {
        self.intra.transform_skip
    }

    /// CU QP delta enabled.
    #[must_use]
    pub fn cu_qp_delta_enabled(&self) -> bool {
        self.qp.cu_delta
    }

    /// CU QP delta depth.
    #[must_use]
    pub fn diff_cu_qp_delta_depth(&self) -> u32 {
        self.qp.cu_depth
    }

    /// Cb QP offset.
    #[must_use]
    pub fn cb_qp_offset(&self) -> i32 {
        self.qp.cb_offset
    }

    /// Cr QP offset.
    #[must_use]
    pub fn cr_qp_offset(&self) -> i32 {
        self.qp.cr_offset
    }

    /// Slice chroma QP offsets present.
    #[must_use]
    pub fn slice_chroma_qp_offsets_present(&self) -> bool {
        self.qp.chroma_present
    }

    /// Transquant bypass enabled.
    #[must_use]
    pub fn transquant_bypass_enabled(&self) -> bool {
        self.transquant_bypass_enabled
    }

    /// Tile column count minus 1.
    #[must_use]
    pub fn num_tile_columns_minus1(&self) -> u32 {
        self.tiles.columns_minus1
    }

    /// Tile row count minus 1.
    #[must_use]
    pub fn num_tile_rows_minus1(&self) -> u32 {
        self.tiles.rows_minus1
    }

    /// Uniform tile spacing.
    #[must_use]
    pub fn uniform_spacing(&self) -> bool {
        self.tiles.uniform
    }

    /// Tile column widths minus 1.
    #[must_use]
    pub fn column_widths(&self) -> &[u32] {
        &self.tiles.column_widths
    }

    /// Tile row heights minus 1.
    #[must_use]
    pub fn row_heights(&self) -> &[u32] {
        &self.tiles.row_heights
    }

    /// Loop filter across tiles enabled.
    #[must_use]
    pub fn loop_filter_across_tiles(&self) -> bool {
        self.loop_filter.across_tiles
    }

    /// Loop filter across slices enabled.
    #[must_use]
    pub fn loop_filter_across_slices(&self) -> bool {
        self.loop_filter.across_slices
    }

    /// Deblocking disabled by default.
    #[must_use]
    pub fn deblocking_filter_disabled(&self) -> bool {
        self.deblock.disabled
    }

    /// Beta offset divided by 2.
    #[must_use]
    pub fn beta_offset_div2(&self) -> i32 {
        self.deblock.beta
    }

    /// Tc offset divided by 2.
    #[must_use]
    pub fn tc_offset_div2(&self) -> i32 {
        self.deblock.tc
    }

    /// Scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }

    /// Parallel merge level minus 2.
    #[must_use]
    pub fn log2_parallel_merge_level_minus2(&self) -> u8 {
        self.log2_parallel_merge_level_minus2
    }

    /// Dependent slice segments allowed.
    #[must_use]
    pub fn dependent_slices_enabled(&self) -> bool {
        self.header.dependent
    }

    /// Sign data hiding enabled.
    #[must_use]
    pub fn sign_data_hiding_enabled(&self) -> bool {
        self.intra.sign_hiding
    }
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SliceType {
    /// Bi-predictive slice.
    B,
    /// Predictive slice.
    P,
    /// Intra slice.
    I,
}

/// Long-term reference picture of one slice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LtSlice {
    /// POC least significant bits.
    poc_lsb: u32,
    /// POC MSB cycle offset.
    msb_cycle: i32,
    /// MSB cycle signaled in the slice.
    msb_cycle_present: bool,
    /// Used by the current picture.
    used: bool,
}

impl LtSlice {
    /// POC least significant bits.
    #[must_use]
    pub fn poc_lsb(&self) -> u32 {
        self.poc_lsb
    }

    /// POC MSB cycle offset.
    #[must_use]
    pub fn msb_cycle(&self) -> i32 {
        self.msb_cycle
    }

    /// MSB cycle signaled in the slice.
    #[must_use]
    pub fn msb_cycle_present(&self) -> bool {
        self.msb_cycle_present
    }

    /// Used by the current picture.
    #[must_use]
    pub fn used(&self) -> bool {
        self.used
    }
}

/// Current picture's reference set resolved from the slice.
#[derive(Clone, Debug, Default)]
pub(crate) struct SliceRps {
    /// Short-term POC deltas with used flags.
    short: Vec<(i32, bool)>,
    /// Long-term pictures.
    long: Vec<LtSlice>,
}

impl SliceRps {
    /// Short-term POC deltas with used flags.
    #[must_use]
    pub fn short(&self) -> &[(i32, bool)] {
        &self.short
    }

    /// Long-term pictures.
    #[must_use]
    pub fn long(&self) -> &[LtSlice] {
        &self.long
    }
}

/// Slice header of one H.265 slice: identity, POC and reference sets.
///
/// The hardware parses slice bodies, so the header stops after reference
/// lists; later syntax only advances the bit reader.
#[derive(Clone, Debug)]
pub(crate) struct SliceHeader {
    /// First segment of the picture.
    first_slice_in_pic: bool,
    /// IRAP pictures drop prior pictures from output.
    no_output_of_prior_pics: bool,
    /// Slice coding type.
    slice_type: SliceType,
    /// POC least significant bits, absent on IDR.
    poc_lsb: Option<u32>,
    /// Short-term set comes from the SPS.
    rps_sps_flag: bool,
    /// Delta pictures of the predicted-from set.
    rps_ref_count: u8,
    /// Syntax bit length of an explicit short-term set.
    rps_bits: u16,
    /// Current picture's reference set.
    rps: SliceRps,
}

impl SliceHeader {
    /// First segment of the picture.
    #[must_use]
    pub fn first_slice_in_pic(&self) -> bool {
        self.first_slice_in_pic
    }

    /// IRAP pictures drop prior pictures from output.
    #[must_use]
    pub fn no_output_of_prior_pics(&self) -> bool {
        self.no_output_of_prior_pics
    }

    /// Slice coding type.
    #[must_use]
    pub fn slice_type(&self) -> SliceType {
        self.slice_type
    }

    /// POC least significant bits, absent on IDR.
    #[must_use]
    pub fn poc_lsb(&self) -> Option<u32> {
        self.poc_lsb
    }

    /// Short-term set comes from the SPS.
    #[must_use]
    pub fn rps_sps_flag(&self) -> bool {
        self.rps_sps_flag
    }

    /// Delta pictures of the predicted-from set.
    #[must_use]
    pub fn rps_ref_count(&self) -> u8 {
        self.rps_ref_count
    }

    /// Syntax bit length of an explicit short-term set.
    #[must_use]
    pub fn rps_bits(&self) -> u16 {
        self.rps_bits
    }

    /// Current picture's reference set.
    #[must_use]
    pub fn rps(&self) -> &SliceRps {
        &self.rps
    }
}

/// Coded slice with its Annex-B bytes for the hardware.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    /// NAL header of the slice.
    nal: NalHeader,
    /// Parsed slice header.
    header: SliceHeader,
    /// Active video set.
    vps: Rc<Vps>,
    /// Active sequence set.
    sps: Rc<Sps>,
    /// Active picture set.
    pps: Rc<Pps>,
    /// Start code plus NAL as coded.
    bytes: Vec<u8>,
}

impl Slice {
    /// NAL header of the slice.
    #[must_use]
    pub fn nal(&self) -> NalHeader {
        self.nal
    }

    /// Parsed slice header.
    #[must_use]
    pub fn header(&self) -> &SliceHeader {
        &self.header
    }

    /// Active video set.
    #[must_use]
    pub fn vps(&self) -> &Rc<Vps> {
        &self.vps
    }

    /// Active sequence set.
    #[must_use]
    pub fn sps(&self) -> &Rc<Sps> {
        &self.sps
    }

    /// Active picture set.
    #[must_use]
    pub fn pps(&self) -> &Rc<Pps> {
        &self.pps
    }

    /// NAL header plus payload with the start code cut.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        if self.bytes.len() >= 4 && self.bytes[2] == 0 {
            &self.bytes[4..]
        } else if self.bytes.len() >= 3 {
            &self.bytes[3..]
        } else {
            &[]
        }
    }
}

/// One parsed NAL unit.
#[derive(Debug)]
pub(crate) enum ParsedNalu {
    /// Video parameter set.
    Vps(Rc<Vps>),
    /// Sequence parameter set.
    Sps(Rc<Sps>),
    /// Picture parameter set.
    Pps(Rc<Pps>),
    /// Coded slice.
    Slice(Box<Slice>),
    /// Anything the tier ignores.
    Other,
}

/// Parsed NAL unit.
#[derive(Debug)]
pub(crate) struct Nalu {
    /// Parsed payload.
    parsed: ParsedNalu,
}

impl Nalu {
    /// Unit with `parsed` payload.
    #[must_use]
    pub fn new(parsed: ParsedNalu) -> Self {
        Self { parsed }
    }

    /// Parsed payload.
    #[must_use]
    pub fn parsed(&self) -> &ParsedNalu {
        &self.parsed
    }
}

/// H.265 NAL parser with parameter-set tables.
pub(crate) struct H265Parser {
    /// Video sets by id.
    vps: HashMap<u8, Rc<Vps>>,
    /// Sequence sets by id.
    sps: HashMap<u8, Rc<Sps>>,
    /// Picture sets by id.
    pps: HashMap<u8, Rc<Pps>>,
}

impl H265Parser {
    /// Parser with empty tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            vps: HashMap::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }
}

impl Default for H265Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// Tier capability gates over shared grammar output.
struct Gate;

impl HevcError for ParseError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } | ParseKind::OverflowI32 { syntax } => {
                Self::Overflow { syntax }
            }
            ParseKind::TooWide { syntax } => Self::Overflow { syntax },
            ParseKind::Range { syntax } => Self::OutOfRange {
                syntax: remap_syntax(syntax),
            },
            ParseKind::NalForbiddenBit | ParseKind::NalTemporalId => Self::OutOfRange {
                syntax: "nal header",
            },
            ParseKind::VpsWithoutBody
            | ParseKind::SpsWithoutBody
            | ParseKind::PpsWithoutBody
            | ParseKind::SliceWithoutHeader => Self::Truncated { syntax: "nal body" },
            ParseKind::PpsCitesUnknownSps
            | ParseKind::SliceCitesUnknownPps
            | ParseKind::SliceCitesUnknownSps => Self::MissingSet { id: 0 },
            ParseKind::PictureSlicesDisagree => Self::OutOfRange {
                syntax: "picture slices",
            },
            ParseKind::NonIntraSliceInIdr => Self::OutOfRange {
                syntax: "slice type",
            },
            ParseKind::ShortTermRefPicSetSize => Self::Unsupported {
                what: "oversize rps",
            },
            ParseKind::EmptyCodedSize => Self::OutOfRange {
                syntax: "sps width",
            },
            ParseKind::ConformanceOutside => Self::OutOfRange {
                syntax: "sps conformance",
            },
            ParseKind::RpsTableOversize => Self::Unsupported {
                what: "oversize rps list",
            },
            ParseKind::LtSpsOversize | ParseKind::LtTotalOversize => Self::Unsupported {
                what: "oversize lt list",
            },
            ParseKind::EmptySpsRps => Self::Unsupported {
                what: "empty sps rps",
            },
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::DependentSlices => Self::Unsupported {
                what: "dependent slices",
            },
            UnsupportedKind::ArbitrarySliceOrder => Self::Unsupported {
                what: "arbitrary slice order",
            },
            UnsupportedKind::Multilayer => Self::Unsupported { what: "multilayer" },
        }
    }
}

/// Shared syntax label behind the tier label.
fn remap_syntax(syntax: &'static str) -> &'static str {
    match syntax {
        "slice_type" => "slice type",
        "short_term_ref_pic_set_idx" => "slice rps idx",
        "slice_segment_address" => "slice address",
        "pic_parameter_set_id" => "pps id",
        "seq_parameter_set_id" => "sps id",
        "pps_seq_parameter_set_id" => "pps sps id",
        "slice_pic_parameter_set_id" => "slice pps id",
        "chroma_format_idc" => "sps chroma",
        "bit_depth_luma_minus8" => "sps luma depth",
        "bit_depth_chroma_minus8" => "sps chroma depth",
        "log2_max_pic_order_cnt_lsb_minus4" => "sps poc bits",
        "offset_len_minus1" => "slice offset len",
        _ => syntax,
    }
}

impl HevcGate<ParseError> for Gate {
    /// Sequence set against tier caps.
    fn check_sps(sps: &rust_video_h265::Sps) -> Result<(), ParseError> {
        if sps.chroma_format > 3 {
            return Err(ParseError::OutOfRange {
                syntax: "sps chroma",
            });
        }
        if sps.max_sub_layers > 7 {
            return Err(ParseError::OutOfRange {
                syntax: "sps max sub",
            });
        }
        check_ptl(&sps.ptl)?;
        if sps.sps_extension_present {
            return Err(ParseError::Unsupported {
                what: "sps extension",
            });
        }
        Ok(())
    }

    /// Picture set against tier caps with its sequence set, absent for orphans.
    fn check_pps(
        pps: &rust_video_h265::Pps,
        sps: Option<&rust_video_h265::Sps>,
    ) -> Result<(), ParseError> {
        let _ = sps;
        if pps.num_tile_columns - 1 > 19 || pps.num_tile_rows - 1 > 21 {
            return Err(ParseError::Unsupported {
                what: "oversize tiles",
            });
        }
        if pps.pps_extension_present {
            return Err(ParseError::Unsupported {
                what: "pps extension",
            });
        }
        Ok(())
    }

    /// Video set against tier caps.
    fn check_vps(vps: &rust_video_h265::Vps) -> Result<(), ParseError> {
        if vps.max_layers_minus1 > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layers",
            });
        }
        if vps.max_sub_layers_minus1 > 6 {
            return Err(ParseError::OutOfRange {
                syntax: "vps max sub",
            });
        }
        check_ptl(&vps.ptl)?;
        if vps.num_layer_sets > 0 {
            return Err(ParseError::Unsupported {
                what: "h265 layer sets",
            });
        }
        if vps
            .timing
            .as_ref()
            .is_some_and(|timing| timing.hrd.len() > 16)
        {
            return Err(ParseError::Unsupported { what: "vps hrd" });
        }
        if vps.extension {
            return Err(ParseError::Unsupported {
                what: "vps extension",
            });
        }
        Ok(())
    }

    /// Slice header tail counts against tier caps.
    fn check_slice_extra(header: &rust_video_h265::SliceHeader) -> Result<(), ParseError> {
        if header.lt.len() > 16 {
            return Err(ParseError::Unsupported {
                what: "oversize lt list",
            });
        }
        if header.entry_count > 256 {
            return Err(ParseError::Unsupported {
                what: "oversize entry points",
            });
        }
        if header.extension_length > 256 {
            return Err(ParseError::Unsupported {
                what: "oversize slice extension",
            });
        }
        Ok(())
    }
}

/// Profile, tier, and level against tier caps.
fn check_ptl(ptl: &rust_video_h265::ProfileTierLevel) -> Result<(), ParseError> {
    check_profile(ptl.profile_idc, ptl.compat)?;
    if ptl.source.interlaced {
        return Err(ParseError::Unsupported {
            what: "interlaced source",
        });
    }
    for layer in &ptl.sub_layers {
        if !layer.present {
            continue;
        }
        check_profile(layer.profile_idc, layer.compat)?;
    }
    Ok(())
}

/// Profile indicator with compatibility against tier caps.
fn check_profile(profile_idc: u8, compat: u32) -> Result<(), ParseError> {
    if !matches!(profile_idc, 1..=3) && compat & 0x0E00_0000 == 0 {
        return Err(ParseError::Unsupported {
            what: "h265 profile",
        });
    }
    Ok(())
}

/// Tier profile record from the shared record.
fn convert_ptl(shared: &rust_video_h265::ProfileTierLevel) -> ProfileTierLevel {
    ProfileTierLevel {
        tier: shared.tier,
        profile_idc: shared.profile_idc,
        source: SourceFlags {
            progressive: shared.source.progressive,
        },
        constraints: ConstraintFlags {
            non_packed: shared.constraints.non_packed,
            frame_only: shared.constraints.frame_only,
        },
        level_idc: shared.level_idc,
    }
}

/// Tier HRD layer from the shared layer.
fn convert_hrd_layer(shared: &rust_video_h265::HrdLayerData) -> HrdLayer {
    HrdLayer {
        bitrate_minus1: shared.bitrate_minus1.clone(),
        cpb_size_minus1: shared.cpb_size_minus1.clone(),
        du_cpb_size_minus1: shared.du_cpb_size_minus1.clone(),
        du_bitrate_minus1: shared.du_bitrate_minus1.clone(),
        cbr: shared.cbr.clone(),
    }
}

/// Tier HRD parameters from the shared parameters.
fn convert_hrd(shared: &rust_video_h265::HrdParams) -> Result<HrdParams, ParseError> {
    let mut elemental = Vec::with_capacity(shared.elemental.len());
    for duration in &shared.elemental {
        elemental.push(match duration {
            Some(value) => narrow_u16(*value, "hrd elemental")?,
            None => 0,
        });
    }
    Ok(HrdParams {
        common: shared.common,
        streams: HrdStreams {
            nal: shared.streams.nal,
            vcl: shared.streams.vcl,
        },
        sub_pic: shared.sub_pic,
        tick_divisor_minus2: shared.tick_divisor_minus2,
        bitrate_scale: shared.bitrate_scale,
        cpb_scale: shared.cpb_scale,
        du_cpb_scale: shared.du_cpb_scale,
        initial_delay_minus1: shared.initial_delay_minus1,
        au_delay_minus1: shared.au_delay_minus1,
        dpb_delay_minus1: shared.dpb_delay_minus1,
        fixed_rate: shared.fixed_rate.clone(),
        pic_duration: shared.pic_duration.clone(),
        elemental,
        cpb_count_minus1: shared.cpb_count_minus1.clone(),
        nal_layers: shared.nal_layers.iter().map(convert_hrd_layer).collect(),
        vcl_layers: shared.vcl_layers.iter().map(convert_hrd_layer).collect(),
    })
}

/// Tier video set from the shared set.
fn convert_vps(shared: &rust_video_h265::Vps) -> Result<Vps, ParseError> {
    let timing = shared
        .timing
        .as_ref()
        .map(|timing| {
            Ok::<_, ParseError>(VpsTiming {
                num_units_in_tick: timing.num_units_in_tick,
                time_scale: timing.time_scale,
                poc_proportional: timing.poc_proportional,
                num_ticks_poc_diff_one_minus1: timing.num_ticks_poc_diff_one_minus1,
                hrd: timing
                    .hrd
                    .iter()
                    .map(convert_hrd)
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .transpose()?;
    Ok(Vps {
        id: shared.id,
        max_sub_layers_minus1: shared.max_sub_layers_minus1,
        temporal_nesting: shared.temporal_nesting,
        ptl: convert_ptl(&shared.ptl),
        ordering_info_present: shared.ordering_info_present,
        max_dec_pic_buffering: shared.max_dec_pic_buffering.clone(),
        max_num_reorder_pics: shared.max_num_reorder_pics.clone(),
        max_latency: shared.max_latency.clone(),
        timing,
    })
}

/// Tier scaling lists from the shared lists.
fn convert_scaling(shared: &rust_video_h265::ScalingLists) -> ScalingLists {
    ScalingLists {
        list_4x4: shared.size0,
        list_8x8: shared.size1,
        list_16x16: shared.size2,
        list_32x32: shared.size3,
        dc_16x16: shared.dc2,
        dc_32x32: shared.dc3,
    }
}

/// Tier reference set from the shared set against converted `table`.
///
/// Explicit sets map directly; inter sets re-derive over the tier table,
/// which keeps coding order while the shared arrays keep resolve order.
fn convert_rps(
    shared: &rust_video_h265::ShortTermRps,
    table: &[RefPicSet],
) -> Result<RefPicSet, ParseError> {
    if !shared.predicted {
        return Ok(RefPicSet {
            s0: pairs(
                &shared.negative,
                &shared.negative_used,
                shared.negative_count,
            ),
            s1: pairs(
                &shared.positive,
                &shared.positive_used,
                shared.positive_count,
            ),
            #[cfg(test)]
            ref_pics: 0,
            inter: false,
            delta_idx_minus1: 0,
            delta_rps_sign: false,
            abs_delta_rps_minus1: 0,
            used_mask: 0,
            use_mask: 0,
        });
    }
    let reference = table.get(shared.ref_idx).ok_or(ParseError::OutOfRange {
        syntax: "rps delta idx",
    })?;
    let (s0, s1) = derive_inter(shared, reference);
    Ok(RefPicSet {
        s0,
        s1,
        #[cfg(test)]
        ref_pics: narrow_u8(
            u32::try_from(reference.s0.len() + reference.s1.len()).map_err(|_| {
                ParseError::OutOfRange {
                    syntax: "rps ref pics",
                }
            })?,
            "rps ref pics",
        )?,
        inter: true,
        delta_idx_minus1: shared.delta_idx_minus1,
        delta_rps_sign: shared.delta_sign,
        abs_delta_rps_minus1: narrow_u16(shared.abs_delta_minus1, "rps abs delta")?,
        used_mask: shared.used_mask,
        use_mask: shared.use_mask,
    })
}

/// Delta and used pairs from parallel arrays.
fn pairs(deltas: &[i32], used: &[bool], count: u8) -> Vec<(i32, bool)> {
    deltas
        .iter()
        .zip(used.iter())
        .take(usize::from(count))
        .map(|(delta, used)| (*delta, *used))
        .collect()
}

/// Negative then positive POC deltas with used flags.
type DeltaLists = (Vec<(i32, bool)>, Vec<(i32, bool)>);

/// Inter-predicted deltas over the tier reference set.
fn derive_inter(shared: &rust_video_h265::ShortTermRps, reference: &RefPicSet) -> DeltaLists {
    let magnitude = shared.abs_delta_minus1.saturating_add(1);
    let delta = i32::try_from(magnitude).unwrap_or(i32::MAX);
    let delta = if shared.delta_sign { -delta } else { delta };
    let mut s0 = Vec::new();
    let mut s1 = Vec::new();
    for ((poc, _), flag) in reference
        .s0
        .iter()
        .chain(reference.s1.iter())
        .zip(shared.predict_flags.iter())
    {
        let (used, keep) = *flag;
        if !keep {
            continue;
        }
        let derived = poc.saturating_add(delta);
        match derived.cmp(&0) {
            std::cmp::Ordering::Less => s0.push((derived, used)),
            std::cmp::Ordering::Greater => s1.push((derived, used)),
            std::cmp::Ordering::Equal => {}
        }
    }
    (s0, s1)
}

/// Tier reference set table from the shared table.
fn convert_rps_table(
    table: &[rust_video_h265::ShortTermRps],
) -> Result<Vec<RefPicSet>, ParseError> {
    let mut out = Vec::with_capacity(table.len());
    for set in table {
        let converted = convert_rps(set, &out)?;
        out.push(converted);
    }
    Ok(out)
}

/// Tier VUI parameters from the shared parameters.
fn convert_vui(shared: &rust_video_h265::VuiParams) -> Result<VuiParams, ParseError> {
    let aspect = AspectRatio {
        present: shared.aspect.present,
        idc: shared.aspect.idc,
        sar_width: narrow_u16(shared.aspect.sar_width, "vui sar width")?,
        sar_height: narrow_u16(shared.aspect.sar_height, "vui sar height")?,
    };
    let overscan = Overscan {
        present: shared.overscan.present,
        appropriate: shared.overscan.appropriate,
    };
    let signal = VideoSignal {
        present: shared.signal.present,
        format: shared.signal.format,
        full_range: shared.signal.full_range,
        colour_present: shared.signal.colour_present,
        primaries: shared.signal.primaries,
        transfer: shared.signal.transfer,
        matrix: shared.signal.matrix,
    };
    let chroma_loc = ChromaLoc {
        present: shared.chroma_loc.present,
        top: shared.chroma_loc.top,
        bottom: shared.chroma_loc.bottom,
    };
    let timing = VuiTiming {
        present: shared.timing.present,
        num_units_in_tick: shared.timing.num_units_in_tick,
        time_scale: shared.timing.time_scale,
        proportional: shared.timing.proportional,
        num_ticks_poc_diff_one_minus1: shared.timing.num_ticks_poc_diff_one_minus1,
    };
    let hrd = shared.hrd.as_ref().map(convert_hrd).transpose()?;
    let restriction = shared
        .restriction
        .as_ref()
        .map(|restriction| {
            Ok::<_, ParseError>(BitstreamRestriction {
                tiles_fixed: restriction.tiles_fixed,
                mv_over_pic: restriction.mv_over_pic,
                restricted_ref: restriction.restricted_ref,
                min_spatial_segmentation: narrow_u16(
                    restriction.min_spatial_segmentation,
                    "vui min segmentation",
                )?,
                max_bytes_per_pic_denom: narrow_u8(
                    restriction.max_bytes_per_pic_denom,
                    "vui max bytes",
                )?,
                max_bits_per_mcu_denom: narrow_u8(
                    restriction.max_bits_per_mcu_denom,
                    "vui max bits",
                )?,
                max_mv_horizontal: narrow_u8(restriction.max_mv_horizontal, "vui max mv h")?,
                max_mv_vertical: narrow_u8(restriction.max_mv_vertical, "vui max mv v")?,
            })
        })
        .transpose()?;
    Ok(VuiParams {
        aspect,
        overscan,
        signal,
        chroma_loc,
        neutral_chroma: shared.neutral_chroma,
        field_seq: shared.field_seq,
        frame_field_info: shared.frame_field_info,
        display_window: shared.display_window,
        timing,
        hrd,
        restriction,
    })
}

/// Tier sequence set from the shared set.
fn convert_sps(shared: &Rc<rust_video_h265::Sps>) -> Result<Sps, ParseError> {
    let scaling = if shared.scaling_enabled() {
        Some(convert_scaling(&shared.scaling))
    } else {
        None
    };
    let pcm = if shared.pcm() {
        Some(PcmParams {
            luma_depth_minus1: shared.pcm_depth_luma,
            chroma_depth_minus1: shared.pcm_depth_chroma,
            min_cb_minus3: u32::from(shared.pcm_log2_min_minus3),
            diff_cb: u32::from(shared.pcm_log2_diff),
            loop_filter_disabled: shared.pcm_loop_disabled(),
        })
    } else {
        None
    };
    let mut long_term = Vec::with_capacity(shared.lt_poc_lsb.len());
    for (poc_lsb, used) in shared.lt_poc_lsb.iter().zip(shared.lt_used_by_curr.iter()) {
        long_term.push(LtSps {
            poc_lsb: *poc_lsb,
            used: *used,
        });
    }
    Ok(Sps {
        vps_id: shared.vps_id,
        max_sub_layers_minus1: shared.max_sub_layers.saturating_sub(1),
        temporal_nesting: shared.flags & rust_video_h265::SPS_NESTING != 0,
        ptl: convert_ptl(&shared.ptl),
        id: shared.id,
        chroma_format_idc: shared.chroma_format,
        separate_colour_plane: shared.separate_colour_plane,
        pic_width: shared.width,
        pic_height: shared.height,
        conformance: shared
            .crop
            .map(|crop| [crop.left / 2, crop.right / 2, crop.top / 2, crop.bottom / 2]),
        bit_depth_luma_minus8: shared.bit_depth_luma,
        bit_depth_chroma_minus8: shared.bit_depth_chroma,
        log2_max_pic_order_cnt_lsb_minus4: shared.log2_max_poc_lsb_minus4,
        ordering: SubLayerOrdering {
            info_present: shared.flags & rust_video_h265::SPS_ORDERING_INFO != 0,
            buffering: shared
                .ordering_buffering
                .iter()
                .map(|v| u32::from(*v))
                .collect(),
            reorder: shared
                .ordering_reorder
                .iter()
                .map(|v| u32::from(*v))
                .collect(),
            latency: shared.ordering_latency.clone(),
        },
        log2_min_cb_minus3: u32::from(shared.log2_min_cb_size_minus3),
        log2_diff_cb: u32::from(shared.log2_diff_max_min_cb_size),
        log2_min_tb_minus2: u32::from(shared.log2_min_tb_size_minus2),
        log2_diff_tb: u32::from(shared.log2_diff_max_min_tb_size),
        max_inter_depth: u32::from(shared.max_transform_depth_inter),
        max_intra_depth: u32::from(shared.max_transform_depth_intra),
        block: BlockTools {
            amp: shared.amp(),
            sao: shared.sao(),
            scaling: shared.scaling_enabled(),
        },
        scaling,
        pcm,
        rps_sets: convert_rps_table(&shared.rps)?,
        long_term,
        prediction: PredictionTools {
            long_term_present: shared.long_term_present(),
            temporal_mvp: shared.temporal_mvp(),
            strong_intra_smoothing: shared.smoothing(),
        },
        vui: shared.vui.as_ref().map(convert_vui).transpose()?,
        shared: Rc::clone(shared),
    })
}

/// Tier picture set from the shared set.
fn convert_pps(shared: &Rc<rust_video_h265::Pps>) -> Pps {
    let tiles = TileLayout {
        enabled: shared.tiles_enabled(),
        entropy_sync: shared.entropy_sync(),
        columns_minus1: u32::from(shared.num_tile_columns.saturating_sub(1)),
        rows_minus1: u32::from(shared.num_tile_rows.saturating_sub(1)),
        uniform: shared.uniform_spacing(),
        column_widths: shared
            .column_width
            .iter()
            .map(|width| u32::from(*width) - 1)
            .collect(),
        row_heights: shared
            .row_height
            .iter()
            .map(|height| u32::from(*height) - 1)
            .collect(),
    };
    let scaling = if shared.flags & rust_video_h265::PPS_SCALING != 0 {
        Some(convert_scaling(&shared.scaling))
    } else {
        None
    };
    Pps {
        id: shared.id,
        sps_id: shared.sps_id,
        header: SliceHeaderFlags {
            dependent: shared.dependent_slices(),
            output_present: shared.output_flag_present(),
            lists_modification: shared.lists_modification_present(),
        },
        num_extra_slice_header_bits: shared.num_extra_slice_header_bits,
        cabac_init_present: shared.cabac_init_present(),
        num_ref_idx_l0_default_minus1: u32::from(shared.num_ref_l0),
        num_ref_idx_l1_default_minus1: u32::from(shared.num_ref_l1),
        init_qp_minus26: i32::from(shared.pic_init_qp_minus26),
        intra: IntraTools {
            constrained_pred: shared.constrained_intra(),
            transform_skip: shared.transform_skip(),
            sign_hiding: shared.sign_hiding(),
        },
        qp: QpTools {
            cu_delta: shared.cu_qp_delta(),
            cu_depth: u32::from(shared.diff_cu_qp_delta_depth),
            cb_offset: i32::from(shared.cb_qp_offset),
            cr_offset: i32::from(shared.cr_qp_offset),
            chroma_present: shared.chroma_offsets_present(),
        },
        weights: WeightFlags {
            pred_p: shared.weighted_pred(),
            pred_b: shared.weighted_bipred(),
        },
        transquant_bypass_enabled: shared.transquant_bypass(),
        tiles,
        loop_filter: LoopFilter {
            across_tiles: shared.loop_across_tiles(),
            across_slices: shared.loop_across_slices(),
        },
        deblock: DeblockDefaults {
            control_present: shared.deblock_control_present,
            override_enabled: shared.deblocking_override(),
            disabled: shared.deblocking_disabled(),
            beta: i32::from(shared.beta_offset_div2),
            tc: i32::from(shared.tc_offset_div2),
        },
        scaling,
        log2_parallel_merge_level_minus2: shared.log2_parallel_merge_minus2,
        slice_header_extension_present: shared.slice_ext_present(),
        shared: Rc::clone(shared),
    }
}

/// Tier slice header from the shared header against its sequence set.
fn convert_slice(
    shared: &rust_video_h265::SliceHeader,
    sps: &Sps,
) -> Result<SliceHeader, ParseError> {
    let mut short = Vec::new();
    let mut sps_flag = false;
    let mut ref_count = 0;
    let mut bits = 0;
    if let (Some(rps), Some(provenance)) = (shared.rps.as_ref(), shared.rps_bits.as_ref()) {
        sps_flag = !provenance.explicit;
        ref_count = provenance.ref_num_delta_pocs;
        bits = provenance.bits;
        if provenance.explicit {
            if rps.predicted {
                let (s0, s1) = derive_inter_set(rps, sps.rps_sets())?;
                short.extend(s0);
                short.extend(s1);
            } else {
                short.extend(pairs(&rps.negative, &rps.negative_used, rps.negative_count));
                short.extend(pairs(&rps.positive, &rps.positive_used, rps.positive_count));
            }
        } else {
            let index = usize::from(provenance.table_idx.ok_or(ParseError::OutOfRange {
                syntax: "slice rps idx",
            })?);
            let set = sps.rps_sets().get(index).ok_or(ParseError::OutOfRange {
                syntax: "slice rps idx",
            })?;
            short.extend(set.s0().iter().copied());
            short.extend(set.s1().iter().copied());
        }
    }
    let mut long = Vec::with_capacity(shared.lt.len());
    for entry in &shared.lt {
        long.push(LtSlice {
            poc_lsb: entry.poc_lsb,
            msb_cycle: i32::try_from(entry.msb_cycle).map_err(|_| ParseError::OutOfRange {
                syntax: "slice lt msb",
            })?,
            msb_cycle_present: entry.msb_present,
            used: entry.used,
        });
    }
    Ok(SliceHeader {
        first_slice_in_pic: shared.first,
        no_output_of_prior_pics: shared.no_output_prior,
        slice_type: match shared.slice_type {
            rust_video_h265::SliceType::B => SliceType::B,
            rust_video_h265::SliceType::P => SliceType::P,
            rust_video_h265::SliceType::I => SliceType::I,
        },
        poc_lsb: shared.poc_lsb,
        rps_sps_flag: sps_flag,
        rps_ref_count: ref_count,
        rps_bits: bits,
        rps: SliceRps { short, long },
    })
}

/// Tier deltas behind a shared inter set against the tier table.
fn derive_inter_set(
    shared: &rust_video_h265::ShortTermRps,
    table: &[RefPicSet],
) -> Result<DeltaLists, ParseError> {
    let reference = table.get(shared.ref_idx).ok_or(ParseError::OutOfRange {
        syntax: "rps delta idx",
    })?;
    Ok(derive_inter(shared, reference))
}

impl H265Parser {
    /// Parse one Annex-B NAL unit.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncated or unsupported syntax.
    pub fn parse(&mut self, nalu: &RawNalu) -> Result<ParsedNalu, ParseError> {
        let payload = nalu.payload();
        if payload.len() < 2 {
            return Err(ParseError::Truncated {
                syntax: "nal header",
            });
        }
        let header = NalHeader {
            unit_type: NalType((payload[0] >> 1) & 0x3F),
            temporal_id: (payload[1] & 0x07).saturating_sub(1),
        };
        if header.unit_type.id() == 32 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let shared = rust_video_h265::parse_vps::<ParseError>(rbsp.bytes())?;
            Gate::check_vps(&shared)?;
            let vps = Rc::new(convert_vps(&shared)?);
            self.vps.insert(vps.id(), Rc::clone(&vps));
            return Ok(ParsedNalu::Vps(vps));
        }
        if header.unit_type.id() == 33 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let shared = Rc::new(rust_video_h265::parse_sps::<ParseError, Gate>(
                rbsp.bytes(),
            )?);
            let sps = Rc::new(convert_sps(&shared)?);
            self.sps.insert(sps.id(), Rc::clone(&sps));
            return Ok(ParsedNalu::Sps(sps));
        }
        if header.unit_type.id() == 34 {
            let rbsp = Rbsp::strip(&payload[2..]);
            let body = rbsp.bytes();
            let shared = Rc::new(rust_video_h265::parse_pps::<ParseError, Gate>(body, None)?);
            let pps = Rc::new(convert_pps(&shared));
            self.pps.insert(pps.id(), Rc::clone(&pps));
            return Ok(ParsedNalu::Pps(pps));
        }
        if header.unit_type.is_slice() {
            let rbsp = Rbsp::strip(&payload[2..]);
            let slice = self.parse_slice(rbsp.bytes(), header, nalu.bytes())?;
            return Ok(ParsedNalu::Slice(Box::new(slice)));
        }
        Ok(ParsedNalu::Other)
    }

    /// Parse a slice header against the stored tables.
    fn parse_slice(&self, body: &[u8], nal: NalHeader, bytes: &[u8]) -> Result<Slice, ParseError> {
        let mut reader = rust_video_h265::BitReader::<ParseError>::new(body);
        let prefix = rust_video_h265::parse_slice_prefix(&mut reader, nal.unit_type().id())?;
        let pps_id = prefix.2;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps_id),
            })?;
        if pps.sps_id() > 15 {
            return Err(ParseError::OutOfRange {
                syntax: "slice sps id",
            });
        }
        let sps = self
            .sps
            .get(&pps.sps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps.sps_id()),
            })?;
        let vps = self
            .vps
            .get(&sps.vps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(sps.vps_id()),
            })?;
        let shared = rust_video_h265::parse_slice_rest(
            &mut reader,
            prefix,
            nal.unit_type().id(),
            &sps.shared,
            &pps.shared,
        )?;
        Gate::check_slice_extra(&shared)?;
        let header = convert_slice(&shared, &sps)?;
        Ok(Slice {
            nal,
            header,
            vps,
            sps,
            pps,
            bytes: bytes.to_vec(),
        })
    }
}

#[cfg(test)]
impl RefPicSet {
    /// Set from `st_ref_pic_set` syntax at index `idx`.
    ///
    /// `inline` marks a slice-header set, the only place `delta_idx_minus1` is coded.
    fn parse(body: &[u8], idx: usize, sets: &[Self], inline: bool) -> Result<Self, ParseError> {
        let table = sets
            .iter()
            .map(shared_rps_input)
            .collect::<Result<Vec<_>, _>>()?;
        let mut reader = rust_video_h265::BitReader::<ParseError>::new(body);
        let num_sps = if inline { idx } else { idx + 1 };
        let resolved = rust_video_h265::parse_short_term_rps(&mut reader, idx, num_sps, &table)?;
        convert_rps(&resolved.set, sets)
    }
}

/// Shared reference set input behind tier values.
#[cfg(test)]
fn shared_rps_input(set: &RefPicSet) -> Result<rust_video_h265::ShortTermRps, ParseError> {
    if set.s0().len() > 16 {
        return Err(ParseError::OutOfRange {
            syntax: "rps delta s0",
        });
    }
    if set.s1().len() > 16 {
        return Err(ParseError::OutOfRange {
            syntax: "rps delta s1",
        });
    }
    let mut out = rust_video_h265::ShortTermRps::default();
    for (index, (delta, used)) in set.s0().iter().enumerate() {
        out.negative[index] = *delta;
        out.negative_used[index] = *used;
    }
    out.negative_count = u8::try_from(set.s0().len()).unwrap_or(u8::MAX);
    for (index, (delta, used)) in set.s1().iter().enumerate() {
        out.positive[index] = *delta;
        out.positive_used[index] = *used;
    }
    out.positive_count = u8::try_from(set.s1().len()).unwrap_or(u8::MAX);
    out.predicted = set.inter();
    out.delta_idx_minus1 = set.delta_idx_minus1();
    out.delta_sign = set.delta_rps_sign();
    out.abs_delta_minus1 = u32::from(set.abs_delta_rps_minus1());
    out.used_mask = set.used_mask();
    out.use_mask = set.use_mask();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{H265Parser, ParsedNalu, SliceType};
    use crate::parse::test_support::{hex, split};

    /// First NAL units of the fork `demo.h265` vector.
    const DEMO_VPS: &str = "40010c01ffff01600000030090000003000003003c92809000";
    /// First SPS of the fork `demo.h265` vector.
    const DEMO_SPS: &str =
        "42010101600000030090000003000003003ca00a080f16592a4932bc05a0200000030020000003032100";
    /// First PPS of the fork `demo.h265` vector.
    const DEMO_PPS: &str = "4401c172b4624000";
    /// Head of the first slice of the fork `demo.h265` vector.
    const DEMO_SLICE: &str =
        "2801af1d20a965478be0af3f4ae63151b2fffd273e7fa20b405b9ec15d0337a20427f6e32f9f583c57fee84d42429d33ffe3ff4aff62991873852f99fb8bf1a358a444ab906f2a97e66aa67a1c";

    #[test]
    fn demo_sps_matches_ffprobe() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        assert_eq!(sps.ptl().profile_idc(), 1);
        assert_eq!(sps.coded_size(), (320, 240));
        assert_eq!(sps.display_size(), (320, 240));
        assert_eq!(sps.chroma_format_idc(), 1);
    }

    #[test]
    fn demo_pps_resolves_its_sps() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS)]);
        let mut parser = H265Parser::default();
        parser.parse(&units[0]).expect("vps");
        let ParsedNalu::Sps(sps) = parser.parse(&units[1]).expect("sps") else {
            panic!("want sps");
        };
        let ParsedNalu::Pps(pps) = parser.parse(&units[2]).expect("pps") else {
            panic!("want pps");
        };
        assert_eq!(pps.sps_id(), sps.id());
    }

    #[test]
    fn demo_slice_parses_against_tables() {
        let units = split(&[hex(DEMO_VPS), hex(DEMO_SPS), hex(DEMO_PPS), hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        parser.parse(&units[0]).expect("vps");
        parser.parse(&units[1]).expect("sps");
        parser.parse(&units[2]).expect("pps");
        let ParsedNalu::Slice(slice) = parser.parse(&units[3]).expect("slice") else {
            panic!("want slice");
        };
        assert!(slice.nal().unit_type().is_idr());
        assert_eq!(slice.header().slice_type(), SliceType::I);
        assert!(slice.header().first_slice_in_pic());
        assert!(slice.header().poc_lsb().is_none());
        assert_eq!(slice.nal().temporal_id(), 0);
    }

    #[test]
    fn truncated_vps_errors() {
        let units = split(&[vec![0x40, 0x01]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::Truncated { .. }
        ));
    }

    #[test]
    fn slice_before_tables_errors() {
        let units = split(&[hex(DEMO_SLICE)]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::MissingSet { .. }
        ));
    }

    #[test]
    fn sei_parses_as_other() {
        let units = split(&[vec![0x4E, 0x01, 0x05, 0xFF]]);
        let mut parser = H265Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).expect("sei"),
            ParsedNalu::Other
        ));
    }

    #[test]
    fn explicit_rps_round_trips_through_std() {
        use super::RefPicSet;
        // negative=2 (deltas 0,1; used 1,0), positive=1 (delta 2, used 1).
        let bytes = [0x6B, 0x47];
        let set = RefPicSet::parse(&bytes, 0, &[], false).expect("rps");
        assert_eq!(set.s0(), &[(-1, true), (-3, false)]);
        assert_eq!(set.s1(), &[(3, true)]);
        let std = crate::session::std_h265::fill_rps(&set).expect("fill");
        assert_eq!(std.flags.inter_ref_pic_set_prediction_flag(), 0);
        assert_eq!(std.num_negative_pics, 2);
        assert_eq!(std.num_positive_pics, 1);
        assert_eq!(&std.delta_poc_s0_minus1[..2], &[0, 1]);
        assert_eq!(&std.delta_poc_s1_minus1[..1], &[2]);
        assert_eq!(std.used_by_curr_pic_s0_flag, 0b01);
        assert_eq!(std.used_by_curr_pic_s1_flag, 0b1);
        assert_eq!(std.used_by_curr_pic_flag, 0b101);
        assert_eq!(std.use_delta_flag, 0b111);
    }

    #[test]
    fn inter_rps_keeps_raw_prediction_syntax() {
        use super::RefPicSet;
        let bytes = [0x6B, 0x47];
        let first = RefPicSet::parse(&bytes, 0, &[], false).expect("rps");
        // Inter, sign 0, abs 0, used 1/0/1 with the middle kept.
        let bytes = [0xB7];
        let sets = [first];
        let set = RefPicSet::parse(&bytes, 1, &sets, false).expect("inter");
        assert!(set.inter());
        assert_eq!(set.ref_pics(), 3);
        assert_eq!(set.used_mask(), 0b101);
        assert_eq!(set.use_mask(), 0b111);
        assert_eq!(set.s0(), &[(-2, false)]);
        assert_eq!(set.s1(), &[(4, true)]);
        let std = crate::session::std_h265::fill_rps(&set).expect("fill");
        assert_eq!(std.flags.inter_ref_pic_set_prediction_flag(), 1);
        assert_eq!(std.flags.delta_rps_sign(), 0);
        assert_eq!(std.abs_delta_rps_minus1, 0);
        assert_eq!(std.used_by_curr_pic_flag, 0b101);
        assert_eq!(std.use_delta_flag, 0b111);
        assert_eq!(&std.delta_poc_s0_minus1[..1], &[1]);
        assert_eq!(&std.delta_poc_s1_minus1[..1], &[3]);
    }
}
