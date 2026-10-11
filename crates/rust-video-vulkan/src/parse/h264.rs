//! H.264 Annex-B parameter sets and slice headers.
//!
//! Syntax per ITU-T H.264 section 7.3; the hardware parses slice bodies,
//! so the slice header stops after reference marking.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::bits::{BitReader, Rbsp};
use crate::parse::nal::Nalu as RawNalu;

/// H.264 NAL unit type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NalType {
    /// Coded slice of a non-IDR picture.
    Slice,
    /// Coded slice data partition A.
    SliceDpa,
    /// Coded slice data partition B.
    SliceDpb,
    /// Coded slice data partition C.
    SliceDpc,
    /// Coded slice of an IDR picture.
    Idr,
    /// Supplemental enhancement information.
    Sei,
    /// Sequence parameter set.
    Sps,
    /// Picture parameter set.
    Pps,
    /// Access unit delimiter.
    Aud,
    /// Any other type, by id.
    Other(u8),
}

impl NalType {
    /// Type from the 5-bit NAL id.
    fn from_id(id: u8) -> Self {
        match id {
            1 => Self::Slice,
            2 => Self::SliceDpa,
            3 => Self::SliceDpb,
            4 => Self::SliceDpc,
            5 => Self::Idr,
            6 => Self::Sei,
            7 => Self::Sps,
            8 => Self::Pps,
            9 => Self::Aud,
            other => Self::Other(other),
        }
    }
}

/// H.264 NAL header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct NalHeader {
    /// Reference importance of the payload.
    ref_idc: u8,
    /// Payload type.
    unit_type: NalType,
}

impl NalHeader {
    /// True for IDR slice NAL units.
    #[must_use]
    pub fn is_idr(self) -> bool {
        self.unit_type == NalType::Idr
    }

    /// Reference importance of the payload.
    #[must_use]
    pub fn ref_idc(self) -> u8 {
        self.ref_idc
    }
}

/// HRD parameters of one set.
#[derive(Clone, Debug, Default)]
pub(crate) struct HrdParams {
    /// CPB count minus 1.
    pub(crate) cpb_count_minus1: u8,
    /// Bitrate scale.
    pub(crate) bitrate_scale: u8,
    /// CPB size scale.
    pub(crate) cpb_scale: u8,
    /// Bitrate minus 1 per CPB.
    pub(crate) bitrate_minus1: Vec<u32>,
    /// CPB size minus 1 per CPB.
    pub(crate) cpb_size_minus1: Vec<u32>,
    /// CBR flag per CPB.
    pub(crate) cbr: Vec<bool>,
    /// Initial CPB removal delay length minus 1.
    pub(crate) initial_delay_minus1: u8,
    /// CPB removal delay length minus 1.
    pub(crate) removal_delay_minus1: u8,
    /// DPB output delay length minus 1.
    pub(crate) output_delay_minus1: u8,
    /// Time offset length.
    pub(crate) time_offset_length: u8,
}

/// Aspect-ratio description of a sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AspectRatio {
    /// Aspect ratio idc.
    pub(crate) idc: u8,
    /// Sample aspect width.
    pub(crate) sar_width: u16,
    /// Sample aspect height.
    pub(crate) sar_height: u16,
}

/// Overscan description of a sequence set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Overscan {
    /// Overscan appropriate for display.
    pub(crate) appropriate: bool,
}

/// Colour description of a video signal.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ColourDesc {
    /// Colour primaries.
    pub(crate) primaries: u8,
    /// Transfer characteristics.
    pub(crate) transfer: u8,
    /// Matrix coefficients.
    pub(crate) matrix: u8,
}

/// Video signal description of a sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VideoSignal {
    /// Video format.
    pub(crate) format: u8,
    /// Full range.
    pub(crate) full_range: bool,
    /// Colour description, when present.
    pub(crate) colour: Option<ColourDesc>,
}

/// Chroma sample locations of a sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChromaLoc {
    /// Top-field location.
    pub(crate) top: u32,
    /// Bottom-field location.
    pub(crate) bottom: u32,
}

/// Timing description of a sequence set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TimingInfo {
    /// Ticks per second.
    pub(crate) num_units_in_tick: u32,
    /// Time scale.
    pub(crate) time_scale: u32,
    /// Fixed frame rate.
    pub(crate) fixed_rate: bool,
}

/// VUI parameters of one sequence set.
#[derive(Clone, Debug, Default)]
pub(crate) struct VuiParams {
    /// Aspect ratio, when present.
    pub(crate) aspect: Option<AspectRatio>,
    /// Overscan, when present.
    pub(crate) overscan: Option<Overscan>,
    /// Video signal, when present.
    pub(crate) signal: Option<VideoSignal>,
    /// Chroma locations, when present.
    pub(crate) chroma_loc: Option<ChromaLoc>,
    /// Timing, when present.
    pub(crate) timing: Option<TimingInfo>,
    /// NAL HRD parameters, when present.
    pub(crate) nal_hrd: Option<HrdParams>,
    /// VCL HRD parameters, when present.
    pub(crate) vcl_hrd: Option<HrdParams>,
    /// Low delay HRD operation.
    pub(crate) low_delay_hrd: bool,
    /// Picture structure present.
    pub(crate) pic_struct_present: bool,
    /// Motion vectors over picture boundaries.
    pub(crate) mv_over_pic: bool,
    /// Maximum bytes per picture denominator.
    pub(crate) max_bytes_per_pic_denom: u32,
    /// Maximum bits per MCU denominator.
    pub(crate) max_bits_per_mcu_denom: u32,
    /// Horizontal motion vector length bits.
    pub(crate) max_mv_horizontal: u32,
    /// Vertical motion vector length bits.
    pub(crate) max_mv_vertical: u32,
    /// Reorder depth, when the restriction is signaled.
    pub(crate) max_num_reorder_frames: Option<u32>,
    /// DPB capacity, when the restriction is signaled.
    pub(crate) max_dec_frame_buffering: Option<u32>,
}

/// Chroma sampling description of a set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChromaFormat {
    /// Sampling indicator.
    pub(crate) idc: u8,
    /// Separate colour plane coding.
    pub(crate) separate_plane: bool,
}

/// Frame/field coding tools of a set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameCoding {
    /// Progressive coding only.
    pub(crate) mbs_only: bool,
    /// Macroblock-adaptive frame-field coding.
    pub(crate) adaptive_field: bool,
    /// 8x8 inference always direct.
    pub(crate) direct_8x8: bool,
}

/// Type-0/1 picture-order-count section of a sequence set.
#[derive(Clone, Debug)]
struct PocTypeParams {
    /// POC lsb width minus 4, type 0 only.
    lsb_bits: u8,
    /// Type-1 deltas always zero.
    always_zero: bool,
    /// Type-1 non-reference offset.
    non_ref: i32,
    /// Type-1 top-to-bottom offset.
    top_bottom: i32,
    /// Type-1 cycle length.
    cycle: u8,
    /// Type-1 cycle offsets.
    offsets: [i32; 256],
}

/// Sequence parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Sps {
    /// Profile indicator.
    profile_idc: u8,
    /// Constraint-set flags packed bit 5..0.
    constraints: u8,
    /// Level indicator.
    level_idc: u8,
    /// Own id.
    id: u8,
    /// Chroma sampling description.
    chroma: ChromaFormat,
    /// Luma bit depth minus 8.
    bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    bit_depth_chroma_minus8: u8,
    /// Transform bypass on lossless slices.
    qpprime_y_zero_transform_bypass: bool,
    /// Sequence scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// Frame number bit width minus 4.
    log2_max_frame_num_minus4: u8,
    /// Picture order count type.
    pic_order_cnt_type: u8,
    /// Picture-order-count section.
    poc: PocTypeParams,
    /// Reference frame window size.
    max_num_ref_frames: u8,
    /// Frame number gaps allowed.
    gaps_in_frame_num_allowed: bool,
    /// Picture width in macroblocks minus 1.
    pic_width_in_mbs_minus1: u16,
    /// Picture height in map units minus 1.
    pic_height_in_map_units_minus1: u16,
    /// Frame/field coding tools.
    frame: FrameCoding,
    /// Cropping window present.
    frame_cropping: bool,
    /// Crop offsets left, right, top, bottom.
    crop: [u32; 4],
    /// VUI parameters, when present.
    vui: Option<VuiParams>,
}

impl Sps {
    /// Own id.
    #[must_use]
    pub fn id(&self) -> u8 {
        self.id
    }

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

    /// Constraint-set-3 flag marking level 1b.
    #[must_use]
    pub fn constraint_set3(&self) -> bool {
        self.constraint_set(3)
    }

    /// Constraint-set flag `index`.
    #[must_use]
    pub fn constraint_set(&self, index: u8) -> bool {
        index < 6 && (self.constraints >> (5 - index)) & 1 != 0
    }

    /// Chroma sampling indicator.
    #[must_use]
    pub fn chroma_format_idc(&self) -> u8 {
        self.chroma.idc
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

    /// Frame number bit width.
    #[must_use]
    pub fn frame_num_bits(&self) -> u32 {
        u32::from(self.log2_max_frame_num_minus4) + 4
    }

    /// Picture order count type.
    #[must_use]
    pub fn pic_order_cnt_type(&self) -> u8 {
        self.pic_order_cnt_type
    }

    /// POC lsb bit width.
    #[must_use]
    pub fn pic_order_cnt_lsb_bits(&self) -> u32 {
        u32::from(self.poc.lsb_bits) + 4
    }

    /// Type-1 POC cycle length.
    #[must_use]
    pub fn num_ref_frames_in_poc_cycle(&self) -> u8 {
        self.poc.cycle
    }

    /// Type-1 POC cycle offsets.
    #[must_use]
    pub fn offset_for_ref_frame(&self) -> &[i32; 256] {
        &self.poc.offsets
    }

    /// Type-1 POC offset for non-reference pictures.
    #[must_use]
    pub fn offset_for_non_ref_pic(&self) -> i32 {
        self.poc.non_ref
    }

    /// Frame-number gaps allowed in the stream.
    #[must_use]
    pub fn gaps_in_frame_num_allowed(&self) -> bool {
        self.gaps_in_frame_num_allowed
    }

    /// Reference frame window size.
    #[must_use]
    pub fn max_num_ref_frames(&self) -> u8 {
        self.max_num_ref_frames
    }

    /// Progressive coding only.
    #[must_use]
    pub fn frame_mbs_only(&self) -> bool {
        self.frame.mbs_only
    }

    /// Coded picture size in luma samples, cropping ignored.
    #[must_use]
    pub fn coded_size(&self) -> (u32, u32) {
        let width = (u32::from(self.pic_width_in_mbs_minus1) + 1) * 16;
        let units = u32::from(self.pic_height_in_map_units_minus1) + 1;
        let height = units * 16 * u32::from(!self.frame.mbs_only) + units * 16;
        (width, height)
    }

    /// Display picture size in luma samples, cropping applied.
    #[must_use]
    pub fn display_size(&self) -> (u32, u32) {
        let (mut width, mut height) = self.coded_size();
        if self.frame_cropping {
            let (unit_x, unit_y) = self.crop_unit();
            width = width.saturating_sub((self.crop[0] + self.crop[1]) * unit_x);
            height = height.saturating_sub((self.crop[2] + self.crop[3]) * unit_y);
        }
        (width, height)
    }

    /// DPB slot budget: window plus the picture under decode.
    #[must_use]
    pub fn dpb_slots(&self) -> u32 {
        u32::from(self.max_num_ref_frames).saturating_add(1).min(16)
    }

    /// Reorder depth from the VUI restriction, else zero.
    #[must_use]
    pub fn max_num_reorder_frames(&self) -> u32 {
        self.vui
            .as_ref()
            .and_then(|vui| vui.max_num_reorder_frames)
            .unwrap_or(0)
            .min(16)
    }

    /// Separate colour plane coding.
    #[must_use]
    pub fn separate_colour_plane(&self) -> bool {
        self.chroma.separate_plane
    }

    /// Transform bypass on lossless slices.
    #[must_use]
    pub fn qpprime_y_zero_transform_bypass(&self) -> bool {
        self.qpprime_y_zero_transform_bypass
    }

    /// Sequence scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }

    /// Frame number bit width minus 4.
    #[must_use]
    pub fn log2_max_frame_num_minus4(&self) -> u8 {
        self.log2_max_frame_num_minus4
    }

    /// POC lsb bit width minus 4.
    #[must_use]
    pub fn log2_max_pic_order_cnt_lsb_minus4(&self) -> u8 {
        self.poc.lsb_bits
    }

    /// Type-1 POC deltas always zero.
    #[must_use]
    pub fn delta_pic_order_always_zero(&self) -> bool {
        self.poc.always_zero
    }

    /// Type-1 top-to-bottom offset.
    #[must_use]
    pub fn offset_for_top_to_bottom_field(&self) -> i32 {
        self.poc.top_bottom
    }

    /// Macroblock-adaptive frame-field coding.
    #[must_use]
    pub fn mb_adaptive_frame_field(&self) -> bool {
        self.frame.adaptive_field
    }

    /// 8x8 inference always direct.
    #[must_use]
    pub fn direct_8x8_inference(&self) -> bool {
        self.frame.direct_8x8
    }

    /// Cropping window present.
    #[must_use]
    pub fn frame_cropping(&self) -> bool {
        self.frame_cropping
    }

    /// Crop offsets left, right, top, bottom.
    #[must_use]
    pub fn crop(&self) -> [u32; 4] {
        self.crop
    }

    /// Picture width in macroblocks minus 1.
    #[must_use]
    pub fn pic_width_in_mbs_minus1(&self) -> u16 {
        self.pic_width_in_mbs_minus1
    }

    /// Picture height in map units minus 1.
    #[must_use]
    pub fn pic_height_in_map_units_minus1(&self) -> u16 {
        self.pic_height_in_map_units_minus1
    }

    /// VUI parameters, when present.
    #[must_use]
    pub fn vui(&self) -> Option<&VuiParams> {
        self.vui.as_ref()
    }

    /// Crop unit in luma samples per chroma sampling.
    fn crop_unit(&self) -> (u32, u32) {
        let unit_x = match self.chroma.idc {
            1 | 2 => 2,
            _ => 1,
        };
        let sub_height = match self.chroma.idc {
            1 => 2,
            _ => 1,
        };
        let unit_y = sub_height * (u32::from(!self.frame.mbs_only) + 1);
        (unit_x, unit_y)
    }
}

/// Picture parameter set.
#[derive(Clone, Debug)]
pub(crate) struct Pps {
    /// Own id.
    id: u8,
    /// Sequence set id.
    sps_id: u8,
    /// Slice-header presence flags.
    presence: SliceHeaderPresence,
    /// Picture-level coding tools.
    coding: PictureCoding,
    /// Default active L0 reference count minus 1.
    num_ref_idx_l0_default_minus1: u32,
    /// Default active L1 reference count minus 1.
    num_ref_idx_l1_default_minus1: u32,
    /// Weighted prediction on P slices.
    weighted_pred: bool,
    /// Weighted prediction mode on B slices.
    weighted_bipred_idc: u8,
    /// Picture QP minus 26.
    pic_init_qp_minus26: i32,
    /// Picture QS minus 26.
    pic_init_qs_minus26: i32,
    /// Chroma QP offset.
    chroma_qp_index_offset: i32,
    /// Picture scaling lists, when present.
    scaling: Option<ScalingLists>,
    /// Second chroma QP offset.
    second_chroma_qp_index_offset: i32,
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

    /// Bottom-field POC delta present in slices.
    #[must_use]
    pub fn bottom_field_pic_order_present(&self) -> bool {
        self.presence.bottom_field_pic_order
    }

    /// Default active L0 reference count.
    #[must_use]
    pub fn num_ref_idx_l0_default(&self) -> u32 {
        self.num_ref_idx_l0_default_minus1 + 1
    }

    /// Default active L1 reference count.
    #[must_use]
    pub fn num_ref_idx_l1_default(&self) -> u32 {
        self.num_ref_idx_l1_default_minus1 + 1
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

    /// CABAC entropy coding.
    #[must_use]
    pub fn entropy_coding_mode(&self) -> bool {
        self.coding.entropy_coding
    }

    /// Weighted prediction on P slices.
    #[must_use]
    pub fn weighted_pred(&self) -> bool {
        self.weighted_pred
    }

    /// Weighted prediction mode on B slices.
    #[must_use]
    pub fn weighted_bipred_idc(&self) -> u8 {
        self.weighted_bipred_idc
    }

    /// Picture QP minus 26.
    #[must_use]
    pub fn pic_init_qp_minus26(&self) -> i32 {
        self.pic_init_qp_minus26
    }

    /// Picture QS minus 26.
    #[must_use]
    pub fn pic_init_qs_minus26(&self) -> i32 {
        self.pic_init_qs_minus26
    }

    /// Chroma QP offset.
    #[must_use]
    pub fn chroma_qp_index_offset(&self) -> i32 {
        self.chroma_qp_index_offset
    }

    /// Second chroma QP offset.
    #[must_use]
    pub fn second_chroma_qp_index_offset(&self) -> i32 {
        self.second_chroma_qp_index_offset
    }

    /// Deblocking filter control present in slices.
    #[must_use]
    pub fn deblocking_control_present(&self) -> bool {
        self.presence.deblocking_control
    }

    /// Constrained intra prediction.
    #[must_use]
    pub fn constrained_intra_pred(&self) -> bool {
        self.coding.constrained_intra
    }

    /// Redundant pictures may appear.
    #[must_use]
    pub fn redundant_pic_cnt_present(&self) -> bool {
        self.presence.redundant_pic_cnt
    }

    /// 8x8 transform mode.
    #[must_use]
    pub fn transform_8x8_mode(&self) -> bool {
        self.coding.transform_8x8
    }

    /// Picture scaling lists, when present.
    #[must_use]
    pub fn scaling(&self) -> Option<&ScalingLists> {
        self.scaling.as_ref()
    }
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SliceType {
    /// Predictive slice.
    P,
    /// Bi-predictive slice.
    B,
    /// Intra slice.
    I,
}

impl SliceType {
    /// Type from the raw `slice_type` idc modulo 5.
    fn from_idc(idc: u32) -> Option<Self> {
        match idc % 5 {
            0 => Some(Self::P),
            1 => Some(Self::B),
            2 => Some(Self::I),
            _ => None,
        }
    }
}

/// Memory management control operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Mmco {
    /// Forget a short-term picture by pic-number gap.
    ForgetShort(u32),
    /// Forget a long-term picture by index.
    ForgetLong(u32),
    /// Move long-term to short-term with a new cap.
    LongToShort {
        /// Long-term frame index.
        index: u32,
        /// New long-term cap plus 1.
        max_plus1: u32,
    },
    /// Set the long-term cap plus 1.
    SetMaxLong(u32),
    /// Reset all reference pictures.
    Reset,
    /// Mark the current picture long-term by index.
    CurrentToLong(u32),
}

/// Decoded reference picture marking.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RefPicMarking {
    /// IDR pictures drop prior pictures from output.
    no_output_of_prior_pics: bool,
    /// IDR picture is a long-term reference.
    long_term_reference: bool,
    /// Adaptive marking mode on non-IDR slices.
    adaptive: bool,
    /// Adaptive operations in order.
    ops: Vec<Mmco>,
}

/// One reference-list modification entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RefMod {
    /// Modification idc: 0 short down, 1 short up, 2 long.
    idc: u32,
    /// Picture number gap or long-term index.
    value: u32,
}

/// Reference-list modifications per list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RefListMod {
    /// L0 modifications in order.
    l0: Vec<RefMod>,
    /// L1 modifications in order.
    l1: Vec<RefMod>,
}

/// Parsed slice header with its parameter sets.
#[derive(Clone, Debug)]
pub(crate) struct SliceHeader {
    /// First macroblock address in the slice.
    first_mb_in_slice: u32,
    /// Slice coding type.
    slice_type: SliceType,
    /// Frame number.
    frame_num: u16,
    /// IDR picture id.
    idr_pic_id: u16,
    /// Type-0 POC least significant bits.
    pic_order_cnt_lsb: u32,
    /// Type-0 bottom-field POC delta.
    delta_pic_order_cnt_bottom: i32,
    /// Type-1 POC deltas.
    delta_pic_order_cnt: [i32; 2],
    /// Active L0 reference count.
    num_ref_idx_l0_active: u32,
    /// Active L1 reference count.
    num_ref_idx_l1_active: u32,
    /// Reference-list modifications.
    ref_mods: RefListMod,
    /// Reference picture marking.
    marking: RefPicMarking,
}

/// Coded slice with its Annex-B bytes for the hardware.
#[derive(Clone, Debug)]
pub(crate) struct Slice {
    /// NAL header of the slice.
    nal: NalHeader,
    /// Parsed slice header.
    header: SliceHeader,
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
    /// Sequence parameter set.
    Sps(Rc<Sps>),
    /// Picture parameter set.
    Pps(Rc<Pps>),
    /// Coded slice.
    Slice(Slice),
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

impl SliceHeader {
    /// First macroblock address in the slice.
    #[must_use]
    pub fn first_mb_in_slice(&self) -> u32 {
        self.first_mb_in_slice
    }

    /// Slice coding type.
    #[must_use]
    pub fn slice_type(&self) -> SliceType {
        self.slice_type
    }

    /// Frame number.
    #[must_use]
    pub fn frame_num(&self) -> u16 {
        self.frame_num
    }

    /// IDR picture id.
    #[must_use]
    pub fn idr_pic_id(&self) -> u16 {
        self.idr_pic_id
    }

    /// Type-0 POC least significant bits.
    #[must_use]
    pub fn pic_order_cnt_lsb(&self) -> u32 {
        self.pic_order_cnt_lsb
    }

    /// Type-0 bottom-field POC delta.
    #[must_use]
    pub fn delta_pic_order_cnt_bottom(&self) -> i32 {
        self.delta_pic_order_cnt_bottom
    }

    /// Type-1 POC deltas.
    #[must_use]
    pub fn delta_pic_order_cnt(&self) -> [i32; 2] {
        self.delta_pic_order_cnt
    }

    /// Active L0 reference count.
    #[must_use]
    pub fn num_ref_idx_l0_active(&self) -> u32 {
        self.num_ref_idx_l0_active
    }

    /// Active L1 reference count.
    #[must_use]
    pub fn num_ref_idx_l1_active(&self) -> u32 {
        self.num_ref_idx_l1_active
    }

    /// Reference-list modifications.
    #[must_use]
    pub fn ref_mods(&self) -> &RefListMod {
        &self.ref_mods
    }

    /// Reference picture marking.
    #[must_use]
    pub fn marking(&self) -> &RefPicMarking {
        &self.marking
    }
}

impl RefPicMarking {
    /// IDR pictures drop prior pictures from output.
    #[must_use]
    pub fn no_output_of_prior_pics(&self) -> bool {
        self.no_output_of_prior_pics
    }

    /// IDR picture is a long-term reference.
    #[must_use]
    pub fn long_term_reference(&self) -> bool {
        self.long_term_reference
    }

    /// Adaptive marking mode on non-IDR slices.
    #[must_use]
    pub fn adaptive(&self) -> bool {
        self.adaptive
    }

    /// Adaptive operations in order.
    #[must_use]
    pub fn ops(&self) -> &[Mmco] {
        &self.ops
    }
}

impl RefListMod {
    /// L0 modifications in order.
    #[must_use]
    pub fn l0(&self) -> &[RefMod] {
        &self.l0
    }

    /// L1 modifications in order.
    #[must_use]
    pub fn l1(&self) -> &[RefMod] {
        &self.l1
    }
}

impl RefMod {
    /// Modification idc: 0 short down, 1 short up, 2 long.
    #[must_use]
    pub fn idc(self) -> u32 {
        self.idc
    }

    /// Picture number gap or long-term index.
    #[must_use]
    pub fn value(self) -> u32 {
        self.value
    }
}

/// H.264 NAL parser with parameter-set tables.
pub(crate) struct H264Parser {
    /// Sequence sets by id.
    sps: HashMap<u8, Rc<Sps>>,
    /// Picture sets by id.
    pps: HashMap<u8, Rc<Pps>>,
}

impl H264Parser {
    /// Parser with empty tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sps: HashMap::new(),
            pps: HashMap::new(),
        }
    }

    /// Parse one Annex-B NAL unit.
    ///
    /// # Errors
    ///
    /// Returns `Err` on truncated or unsupported syntax.
    pub fn parse(&mut self, nalu: &RawNalu) -> Result<ParsedNalu, ParseError> {
        let payload = nalu.payload();
        if payload.is_empty() {
            return Err(ParseError::Truncated {
                syntax: "nal header",
            });
        }
        let header = NalHeader {
            ref_idc: (payload[0] >> 5) & 3,
            unit_type: NalType::from_id(payload[0] & 0x1F),
        };
        let rbsp = Rbsp::strip(&payload[1..]);
        let mut reader = BitReader::new(rbsp.bytes());
        match header.unit_type {
            NalType::Sps => {
                let sps = Rc::new(Sps::parse(&mut reader)?);
                self.sps.insert(sps.id(), Rc::clone(&sps));
                Ok(ParsedNalu::Sps(sps))
            }
            NalType::Pps => {
                let pps = Rc::new(Pps::parse(&mut reader)?);
                self.pps.insert(pps.id(), Rc::clone(&pps));
                Ok(ParsedNalu::Pps(pps))
            }
            NalType::Slice | NalType::Idr => {
                let slice = self.parse_slice(&mut reader, header, nalu.bytes())?;
                Ok(ParsedNalu::Slice(slice))
            }
            NalType::SliceDpa | NalType::SliceDpb | NalType::SliceDpc => {
                Err(ParseError::Unsupported {
                    what: "data partitioning",
                })
            }
            NalType::Sei | NalType::Aud | NalType::Other(_) => Ok(ParsedNalu::Other),
        }
    }

    /// Parse a slice header against the stored tables.
    fn parse_slice(
        &self,
        reader: &mut BitReader<'_>,
        nal: NalHeader,
        bytes: &[u8],
    ) -> Result<Slice, ParseError> {
        let first_mb_in_slice = reader.ue("slice first_mb")?;
        let raw_type = reader.ue("slice type")?;
        let slice_type = SliceType::from_idc(raw_type).ok_or(ParseError::Unsupported {
            what: "sp/si slices",
        })?;
        let pps_id = narrow_u8(reader.ue("slice pps id")?, "slice pps id")?;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps_id),
            })?;
        let sps = self
            .sps
            .get(&pps.sps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps.sps_id()),
            })?;
        let frame_num = narrow_u16(
            reader.bits("slice frame_num", sps.frame_num_bits())?,
            "slice frame_num",
        )?;
        if !sps.frame_mbs_only() {
            return Err(ParseError::Unsupported {
                what: "field pictures",
            });
        }
        let mut idr_pic_id = 0;
        if nal.is_idr() {
            idr_pic_id = narrow_u16(reader.ue("slice idr_pic_id")?, "slice idr_pic_id")?;
        }
        let (pic_order_cnt_lsb, delta_pic_order_cnt_bottom, delta_pic_order_cnt) =
            parse_poc(reader, &sps, &pps)?;
        if pps.redundant_pic_cnt_present() {
            reader.ue("slice redundant cnt")?;
        }
        if slice_type == SliceType::B {
            reader.bit("slice direct spatial")?;
        }
        let (num_ref_idx_l0_active, num_ref_idx_l1_active, ref_mods) =
            parse_ref_lists(reader, &sps, &pps, slice_type)?;
        let marking = parse_marking(reader, nal)?;
        Ok(Slice {
            nal,
            header: SliceHeader {
                first_mb_in_slice,
                slice_type,
                frame_num,
                idr_pic_id,
                pic_order_cnt_lsb,
                delta_pic_order_cnt_bottom,
                delta_pic_order_cnt,
                num_ref_idx_l0_active,
                num_ref_idx_l1_active,
                ref_mods,
                marking,
            },
            sps,
            pps,
            bytes: bytes.to_vec(),
        })
    }
}

impl Default for H264Parser {
    fn default() -> Self {
        Self::new()
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

/// Active reference count from a minus-one value, capped at the list width.
fn checked_ref_count(minus1: u32) -> Result<u32, ParseError> {
    if minus1 >= 32 {
        return Err(ParseError::Unsupported {
            what: "oversize ref count",
        });
    }
    Ok(minus1 + 1)
}

/// Slice POC fields for the set's count type.
fn parse_poc(
    reader: &mut BitReader<'_>,
    sps: &Sps,
    pps: &Pps,
) -> Result<(u32, i32, [i32; 2]), ParseError> {
    let mut lsb = 0;
    let mut delta_bottom = 0;
    let mut delta = [0; 2];
    match sps.pic_order_cnt_type() {
        0 => {
            lsb = reader.bits("slice poc lsb", sps.pic_order_cnt_lsb_bits())?;
            if pps.bottom_field_pic_order_present() {
                delta_bottom = reader.se("slice poc delta")?;
            }
        }
        1 => {
            if !sps.delta_pic_order_always_zero() {
                delta[0] = reader.se("slice poc delta 0")?;
            }
            if pps.bottom_field_pic_order_present() {
                delta[1] = reader.se("slice poc delta 1")?;
            }
        }
        _ => {}
    }
    Ok((lsb, delta_bottom, delta))
}

/// Active counts plus list modifications and weight skips of the slice.
fn parse_ref_lists(
    reader: &mut BitReader<'_>,
    sps: &Sps,
    pps: &Pps,
    slice_type: SliceType,
) -> Result<(u32, u32, RefListMod), ParseError> {
    let mut l0 = pps.num_ref_idx_l0_default();
    let mut l1 = pps.num_ref_idx_l1_default();
    let mut mods = RefListMod::default();
    if slice_type != SliceType::I {
        if reader.bit("slice ref override")? {
            l0 = checked_ref_count(reader.ue("slice l0 active")?)?;
            if slice_type == SliceType::B {
                l1 = checked_ref_count(reader.ue("slice l1 active")?)?;
            }
        }
        mods = RefListMod::parse(reader, slice_type)?;
        let weighted = (pps.weighted_pred() && slice_type == SliceType::P)
            || (pps.weighted_bipred_idc() == 1 && slice_type == SliceType::B);
        if weighted {
            skip_weights(reader, slice_type, l0, l1, sps.chroma_format_idc() != 0)?;
        }
    }
    Ok((l0, l1, mods))
}

impl Sps {
    /// Sequence set from RBSP bytes.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let profile_idc = narrow_u8(reader.bits("sps profile", 8)?, "sps profile")?;
        let constraints = narrow_u8(reader.bits("sps constraints", 8)?, "sps constraints")? >> 2;
        let level_idc = narrow_u8(reader.bits("sps level", 8)?, "sps level")?;
        let sps_id = narrow_u8(reader.ue("sps id")?, "sps id")?;
        let fidelity = parse_fidelity(reader, profile_idc)?;
        let log2_max_frame_num_minus4 = narrow_u8(reader.ue("sps frame bits")?, "sps frame bits")?;
        if log2_max_frame_num_minus4 > 12 {
            return Err(ParseError::OutOfRange {
                syntax: "sps frame bits",
            });
        }
        let pic_order_cnt_type = narrow_u8(reader.ue("sps poc type")?, "sps poc type")?;
        if pic_order_cnt_type > 2 {
            return Err(ParseError::Unsupported {
                what: "pic_order_cnt_type",
            });
        }
        let poc = parse_poc_section(reader, pic_order_cnt_type)?;
        let max_num_ref_frames = narrow_u8(reader.ue("sps max refs")?, "sps max refs")?;
        let gaps_in_frame_num_allowed = reader.bit("sps gaps allowed")?;
        let pic_width_in_mbs_minus1 = narrow_u16(reader.ue("sps width")?, "sps width")?;
        let pic_height_in_map_units_minus1 = narrow_u16(reader.ue("sps height")?, "sps height")?;
        let frame_mbs_only = reader.bit("sps frame only")?;
        if !frame_mbs_only {
            return Err(ParseError::Unsupported {
                what: "field pictures",
            });
        }
        let mb_adaptive_frame_field = false;
        let direct_8x8_inference = reader.bit("sps direct 8x8")?;
        let frame_cropping = reader.bit("sps cropping")?;
        let mut crop = [0; 4];
        if frame_cropping {
            crop[0] = reader.ue("sps crop left")?;
            crop[1] = reader.ue("sps crop right")?;
            crop[2] = reader.ue("sps crop top")?;
            crop[3] = reader.ue("sps crop bottom")?;
        }
        let mut vui = None;
        if reader.bit("sps vui present")? {
            vui = Some(parse_vui(reader)?);
        }
        Ok(Self {
            profile_idc,
            constraints,
            level_idc,
            id: sps_id,
            chroma: fidelity.chroma,
            bit_depth_luma_minus8: fidelity.bit_depth_luma_minus8,
            bit_depth_chroma_minus8: fidelity.bit_depth_chroma_minus8,
            qpprime_y_zero_transform_bypass: fidelity.qpprime_y_zero_transform_bypass,
            scaling: fidelity.scaling,
            log2_max_frame_num_minus4,
            pic_order_cnt_type,
            poc,
            max_num_ref_frames,
            gaps_in_frame_num_allowed,
            pic_width_in_mbs_minus1,
            pic_height_in_map_units_minus1,
            frame: FrameCoding {
                mbs_only: frame_mbs_only,
                adaptive_field: mb_adaptive_frame_field,
                direct_8x8: direct_8x8_inference,
            },
            frame_cropping,
            crop,
            vui,
        })
    }
}

/// Fidelity-range extensions of one sequence set.
struct FidelityRange {
    /// Chroma sampling description.
    chroma: ChromaFormat,
    /// Luma bit depth minus 8.
    bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    bit_depth_chroma_minus8: u8,
    /// Transform bypass on lossless slices.
    qpprime_y_zero_transform_bypass: bool,
    /// Sequence scaling lists, when present.
    scaling: Option<ScalingLists>,
}

/// Fidelity-range extensions for `profile_idc`, defaults on base profiles.
fn parse_fidelity(
    reader: &mut BitReader<'_>,
    profile_idc: u8,
) -> Result<FidelityRange, ParseError> {
    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    let mut bit_depth_luma_minus8 = 0;
    let mut bit_depth_chroma_minus8 = 0;
    let mut qpprime_y_zero_transform_bypass = false;
    let mut scaling = None;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128
    ) {
        chroma_format_idc = narrow_u8(reader.ue("sps chroma")?, "sps chroma")?;
        if chroma_format_idc > 3 {
            return Err(ParseError::OutOfRange {
                syntax: "sps chroma",
            });
        }
        if chroma_format_idc == 3 {
            separate_colour_plane = reader.bit("sps separate plane")?;
        }
        bit_depth_luma_minus8 = narrow_u8(reader.ue("sps luma depth")?, "sps luma depth")?;
        bit_depth_chroma_minus8 = narrow_u8(reader.ue("sps chroma depth")?, "sps chroma depth")?;
        qpprime_y_zero_transform_bypass = reader.bit("sps transform bypass")?;
        if reader.bit("sps scaling present")? {
            let count = if chroma_format_idc == 3 { 12 } else { 8 };
            scaling = Some(parse_scaling(reader, count)?);
        }
    }
    Ok(FidelityRange {
        chroma: ChromaFormat {
            idc: chroma_format_idc,
            separate_plane: separate_colour_plane,
        },
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        qpprime_y_zero_transform_bypass,
        scaling,
    })
}

/// Type-0/1 picture-order-count section for `pic_order_cnt_type`.
fn parse_poc_section(
    reader: &mut BitReader<'_>,
    pic_order_cnt_type: u8,
) -> Result<PocTypeParams, ParseError> {
    let mut params = PocTypeParams {
        lsb_bits: 0,
        always_zero: false,
        non_ref: 0,
        top_bottom: 0,
        cycle: 0,
        offsets: [0; 256],
    };
    match pic_order_cnt_type {
        0 => {
            params.lsb_bits = narrow_u8(reader.ue("sps poc bits")?, "sps poc bits")?;
            if params.lsb_bits > 12 {
                return Err(ParseError::OutOfRange {
                    syntax: "sps poc bits",
                });
            }
        }
        1 => {
            params.always_zero = reader.bit("sps poc always zero")?;
            params.non_ref = reader.se("sps poc non-ref")?;
            params.top_bottom = reader.se("sps poc top-bottom")?;
            params.cycle = narrow_u8(reader.ue("sps poc cycle")?, "sps poc cycle")?;
            for offset in params.offsets.iter_mut().take(params.cycle as usize) {
                *offset = reader.se("sps poc cycle offset")?;
            }
        }
        _ => {}
    }
    Ok(params)
}

/// Scaling lists of a set, flat 16 when a list is absent.
#[derive(Clone, Debug)]
pub(crate) struct ScalingLists {
    /// Lists present per index.
    pub(crate) present_mask: u16,
    /// Lists using the default matrix per index.
    pub(crate) default_mask: u16,
    /// 4x4 lists.
    pub(crate) list_4x4: [[u8; 16]; 6],
    /// 8x8 lists.
    pub(crate) list_8x8: [[u8; 64]; 6],
}

/// Slice-header presence flags of a picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SliceHeaderPresence {
    /// Bottom-field POC delta present in slices.
    pub(crate) bottom_field_pic_order: bool,
    /// Deblocking filter control present in slices.
    pub(crate) deblocking_control: bool,
    /// Redundant pictures may appear.
    pub(crate) redundant_pic_cnt: bool,
}

/// Picture-level coding tools of a picture set.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PictureCoding {
    /// CABAC entropy coding.
    pub(crate) entropy_coding: bool,
    /// 8x8 transform mode.
    pub(crate) transform_8x8: bool,
    /// Constrained intra prediction.
    pub(crate) constrained_intra: bool,
}

/// Scaling lists over `count` coded indexes, flat 16 when absent.
fn parse_scaling(reader: &mut BitReader<'_>, count: usize) -> Result<ScalingLists, ParseError> {
    let mut lists = ScalingLists {
        present_mask: 0,
        default_mask: 0,
        list_4x4: [[16_u8; 16]; 6],
        list_8x8: [[16_u8; 64]; 6],
    };
    for i in 0..count {
        if !reader.bit("sps scaling list")? {
            continue;
        }
        lists.present_mask |= 1_u16 << i;
        let use_default = if i < 6 {
            parse_scaling_list(reader, &mut lists.list_4x4[i])?
        } else {
            parse_scaling_list(reader, &mut lists.list_8x8[i - 6])?
        };
        if use_default {
            lists.default_mask |= 1_u16 << i;
        }
    }
    Ok(lists)
}

/// One scaling list from delta scales; reports the default-matrix escape.
fn parse_scaling_list(reader: &mut BitReader<'_>, list: &mut [u8]) -> Result<bool, ParseError> {
    let mut last_scale = 8_i32;
    let mut next_scale = 8_i32;
    for (j, slot) in list.iter_mut().enumerate() {
        if next_scale != 0 {
            let delta = reader.se("scaling delta")?;
            next_scale = (last_scale + delta + 256) % 256;
            if j == 0 && next_scale == 0 {
                list.fill(16);
                return Ok(true);
            }
        }
        last_scale = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
        *slot = u8::try_from(last_scale).map_err(|_| ParseError::OutOfRange {
            syntax: "scaling delta",
        })?;
    }
    Ok(false)
}

/// VUI parameters.
fn parse_vui(reader: &mut BitReader<'_>) -> Result<VuiParams, ParseError> {
    let mut vui = VuiParams::default();
    if reader.bit("vui aspect present")? {
        let idc = narrow_u8(reader.bits("vui aspect idc", 8)?, "vui aspect idc")?;
        let (sar_width, sar_height) = if idc == 255 {
            (
                narrow_u16(reader.bits("vui sar width", 16)?, "vui sar width")?,
                narrow_u16(reader.bits("vui sar height", 16)?, "vui sar height")?,
            )
        } else {
            (0, 0)
        };
        vui.aspect = Some(AspectRatio {
            idc,
            sar_width,
            sar_height,
        });
    }
    if reader.bit("vui overscan present")? {
        vui.overscan = Some(Overscan {
            appropriate: reader.bit("vui overscan")?,
        });
    }
    if reader.bit("vui signal present")? {
        let format = narrow_u8(reader.bits("vui format", 3)?, "vui format")?;
        let full_range = reader.bit("vui full range")?;
        let colour = if reader.bit("vui colour present")? {
            Some(ColourDesc {
                primaries: narrow_u8(reader.bits("vui primaries", 8)?, "vui primaries")?,
                transfer: narrow_u8(reader.bits("vui transfer", 8)?, "vui transfer")?,
                matrix: narrow_u8(reader.bits("vui matrix", 8)?, "vui matrix")?,
            })
        } else {
            None
        };
        vui.signal = Some(VideoSignal {
            format,
            full_range,
            colour,
        });
    }
    if reader.bit("vui chroma loc present")? {
        vui.chroma_loc = Some(ChromaLoc {
            top: reader.ue("vui chroma top")?,
            bottom: reader.ue("vui chroma bottom")?,
        });
    }
    if reader.bit("vui timing present")? {
        vui.timing = Some(TimingInfo {
            num_units_in_tick: reader.bits("vui tick", 32)?,
            time_scale: reader.bits("vui scale", 32)?,
            fixed_rate: reader.bit("vui fixed rate")?,
        });
    }
    if reader.bit("vui nal hrd")? {
        vui.nal_hrd = Some(parse_hrd(reader)?);
    }
    if reader.bit("vui vcl hrd")? {
        vui.vcl_hrd = Some(parse_hrd(reader)?);
    }
    if vui.nal_hrd.is_some() || vui.vcl_hrd.is_some() {
        vui.low_delay_hrd = reader.bit("vui low delay")?;
    }
    vui.pic_struct_present = reader.bit("vui pic struct")?;
    if reader.bit("vui restriction")? {
        vui.mv_over_pic = reader.bit("vui mv over boundary")?;
        vui.max_bytes_per_pic_denom = reader.ue("vui max bytes denom")?;
        vui.max_bits_per_mcu_denom = reader.ue("vui max bits denom")?;
        vui.max_mv_horizontal = reader.ue("vui max mv horizontal")?;
        vui.max_mv_vertical = reader.ue("vui max mv vertical")?;
        vui.max_num_reorder_frames = Some(reader.ue("vui reorder frames")?);
        vui.max_dec_frame_buffering = Some(reader.ue("vui max buffering")?);
    }
    Ok(vui)
}

/// HRD parameters.
fn parse_hrd(reader: &mut BitReader<'_>) -> Result<HrdParams, ParseError> {
    let mut hrd = HrdParams::default();
    let cpb_cnt = reader.ue("hrd cpb count")?;
    if cpb_cnt > 31 {
        return Err(ParseError::Unsupported {
            what: "oversize hrd",
        });
    }
    hrd.cpb_count_minus1 = narrow_u8(cpb_cnt, "hrd cpb count")?;
    hrd.bitrate_scale = narrow_u8(reader.bits("hrd bitrate scale", 4)?, "hrd bitrate scale")?;
    hrd.cpb_scale = narrow_u8(reader.bits("hrd cpb scale", 4)?, "hrd cpb scale")?;
    for _ in 0..=cpb_cnt {
        hrd.bitrate_minus1.push(reader.ue("hrd bitrate")?);
        hrd.cpb_size_minus1.push(reader.ue("hrd cpb size")?);
        hrd.cbr.push(reader.bit("hrd cbr")?);
    }
    hrd.initial_delay_minus1 =
        narrow_u8(reader.bits("hrd initial delay", 5)?, "hrd initial delay")?;
    hrd.removal_delay_minus1 =
        narrow_u8(reader.bits("hrd removal delay", 5)?, "hrd removal delay")?;
    hrd.output_delay_minus1 = narrow_u8(reader.bits("hrd output delay", 5)?, "hrd output delay")?;
    hrd.time_offset_length = narrow_u8(reader.bits("hrd time offset", 5)?, "hrd time offset")?;
    Ok(hrd)
}

impl Pps {
    /// Picture set from RBSP bytes.
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ParseError> {
        let pps_id = narrow_u8(reader.ue("pps id")?, "pps id")?;
        let sps_id = narrow_u8(reader.ue("pps sps id")?, "pps sps id")?;
        let entropy_coding_mode = reader.bit("pps cabac")?;
        let bottom_field_pic_order_present = reader.bit("pps bottom poc")?;
        let groups = reader.ue("pps slice groups")?;
        if groups > 0 {
            return Err(ParseError::Unsupported {
                what: "slice groups",
            });
        }
        let num_ref_idx_l0_default_minus1 = reader.ue("pps l0 default")?;
        let num_ref_idx_l1_default_minus1 = reader.ue("pps l1 default")?;
        if num_ref_idx_l0_default_minus1 >= 32 || num_ref_idx_l1_default_minus1 >= 32 {
            return Err(ParseError::Unsupported {
                what: "oversize ref count",
            });
        }
        let weighted_pred = reader.bit("pps weighted pred")?;
        let weighted_bipred_idc = narrow_u8(
            reader.bits("pps weighted bipred", 2)?,
            "pps weighted bipred",
        )?;
        let pic_qp = reader.se("pps init qp")?;
        let pic_qs_delta = reader.se("pps init qs")?;
        let chroma_qp_index_offset = reader.se("pps chroma qp")?;
        let deblocking_control = reader.bit("pps deblocking")?;
        let constrained_intra = reader.bit("pps constrained intra")?;
        let redundant_pic_cnt = reader.bit("pps redundant")?;
        let presence = SliceHeaderPresence {
            bottom_field_pic_order: bottom_field_pic_order_present,
            deblocking_control,
            redundant_pic_cnt,
        };
        let mut coding = PictureCoding {
            entropy_coding: entropy_coding_mode,
            transform_8x8: false,
            constrained_intra,
        };
        let mut scaling = None;
        let mut second_chroma_qp_index_offset = 0;
        if reader.more_rbsp() {
            coding.transform_8x8 = reader.bit("pps transform 8x8")?;
            if reader.bit("pps scaling present")? {
                scaling = Some(parse_scaling(reader, 8)?);
            }
            second_chroma_qp_index_offset = reader.se("pps chroma qp 2")?;
        }
        Ok(Self {
            id: pps_id,
            sps_id,
            presence,
            coding,
            num_ref_idx_l0_default_minus1,
            num_ref_idx_l1_default_minus1,
            weighted_pred,
            weighted_bipred_idc,
            pic_init_qp_minus26: pic_qp,
            pic_init_qs_minus26: pic_qs_delta,
            chroma_qp_index_offset,
            scaling,
            second_chroma_qp_index_offset,
        })
    }
}

impl RefListMod {
    /// Reference-list modifications for one slice.
    fn parse(reader: &mut BitReader<'_>, slice_type: SliceType) -> Result<Self, ParseError> {
        let mut mods = Self::default();
        if reader.bit("slice l0 mod")? {
            mods.l0 = parse_mod_list(reader)?;
        }
        if slice_type == SliceType::B && reader.bit("slice l1 mod")? {
            mods.l1 = parse_mod_list(reader)?;
        }
        Ok(mods)
    }
}

/// Skip one prediction weight table, validating denominators.
///
/// The software decode path never applies weights; the Vulkan driver parses
/// slice headers itself, so the table is consumed for bit position only.
fn skip_weights(
    reader: &mut BitReader<'_>,
    slice_type: SliceType,
    l0: u32,
    l1: u32,
    chroma: bool,
) -> Result<(), ParseError> {
    let luma_denom = reader.ue("slice luma denom")?;
    if luma_denom > 7 {
        return Err(ParseError::OutOfRange {
            syntax: "slice luma denom",
        });
    }
    if chroma {
        let chroma_denom = reader.ue("slice chroma denom")?;
        if chroma_denom > 7 {
            return Err(ParseError::OutOfRange {
                syntax: "slice chroma denom",
            });
        }
    }
    skip_weight_list(reader, l0, chroma)?;
    if slice_type == SliceType::B {
        skip_weight_list(reader, l1, chroma)?;
    }
    Ok(())
}

/// Skip one weight list, consuming its flag and delta bits.
fn skip_weight_list(
    reader: &mut BitReader<'_>,
    count: u32,
    chroma: bool,
) -> Result<(), ParseError> {
    for _ in 0..count {
        if reader.bit("slice weight luma flag")? {
            reader.se("slice weight luma")?;
            reader.se("slice offset luma")?;
        }
        if chroma && reader.bit("slice weight chroma flag")? {
            for _ in 0..2 {
                reader.se("slice weight chroma")?;
                reader.se("slice offset chroma")?;
            }
        }
    }
    Ok(())
}

/// One modification list up to its terminator.
fn parse_mod_list(reader: &mut BitReader<'_>) -> Result<Vec<RefMod>, ParseError> {
    let mut out = Vec::new();
    loop {
        let idc = reader.ue("slice mod idc")?;
        if idc == 3 {
            return Ok(out);
        }
        if idc > 3 || out.len() >= 32 {
            return Err(ParseError::Unsupported {
                what: "oversize ref list",
            });
        }
        let value = reader.ue("slice mod value")?;
        out.push(RefMod { idc, value });
    }
}

/// Reference picture marking for one slice.
fn parse_marking(reader: &mut BitReader<'_>, nal: NalHeader) -> Result<RefPicMarking, ParseError> {
    let mut marking = RefPicMarking::default();
    if nal.ref_idc() == 0 {
        return Ok(marking);
    }
    if nal.is_idr() {
        marking.no_output_of_prior_pics = reader.bit("slice no output prior")?;
        marking.long_term_reference = reader.bit("slice long term idr")?;
        return Ok(marking);
    }
    marking.adaptive = reader.bit("slice adaptive marking")?;
    if !marking.adaptive {
        return Ok(marking);
    }
    loop {
        let opcode = reader.ue("slice mmco")?;
        if opcode == 0 {
            return Ok(marking);
        }
        if marking.ops.len() >= 32 {
            return Err(ParseError::Unsupported {
                what: "oversize marking",
            });
        }
        let op = match opcode {
            1 => Mmco::ForgetShort(reader.ue("slice mmco gap")?),
            2 => Mmco::ForgetLong(reader.ue("slice mmco index")?),
            3 => Mmco::LongToShort {
                index: reader.ue("slice mmco index")?,
                max_plus1: reader.ue("slice mmco max")?,
            },
            4 => Mmco::SetMaxLong(reader.ue("slice mmco max")?),
            5 => Mmco::Reset,
            6 => Mmco::CurrentToLong(reader.ue("slice mmco index")?),
            _ => {
                return Err(ParseError::Unsupported {
                    what: "mmco opcode",
                });
            }
        };
        marking.ops.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::{H264Parser, ParsedNalu, SliceType};
    use crate::parse::test_support::{hex, split};

    /// First two NAL units of the fork `demo.h264` vector.
    const DEMO_SPS: &str = "6764000dacb20283f60220000003002000000641e2854900";
    /// First PPS of the fork `demo.h264` vector.
    const DEMO_PPS: &str = "68ebc3cb22c0";
    /// Head of the first slice of the fork `demo.h264` vector.
    const DEMO_SLICE: &str =
        "6588840cfffef6ecbe053614c85094a04007bb443688f75792a97d0e881b266f395d1803e3eea5efbae04cafddfd1d8f5aa3b94b6acca893a915d63dcd";

    #[test]
    fn demo_sps_matches_ffprobe() {
        let units = split(&[hex(DEMO_SPS)]);
        let mut parser = H264Parser::new();
        let ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        assert_eq!(sps.profile_idc(), 100);
        assert_eq!(sps.level_idc(), 13);
        assert_eq!(sps.coded_size(), (320, 240));
        assert_eq!(sps.display_size(), (320, 240));
        assert_eq!(sps.chroma_format_idc(), 1);
    }

    #[test]
    fn demo_pps_resolves_its_sps() {
        let units = split(&[hex(DEMO_SPS), hex(DEMO_PPS)]);
        let mut parser = H264Parser::default();
        parser.parse(&units[0]).expect("sps");
        let ParsedNalu::Pps(pps) = parser.parse(&units[1]).expect("pps") else {
            panic!("want pps");
        };
        assert_eq!(pps.sps_id(), 0);
        assert!(matches!(
            parser.parse(&units[0]).expect("sps again"),
            ParsedNalu::Sps(_)
        ));
    }

    #[test]
    fn demo_slice_parses_against_tables() {
        let units = split(&[hex(DEMO_SPS), hex(DEMO_PPS), hex(DEMO_SLICE)]);
        let mut parser = H264Parser::new();
        parser.parse(&units[0]).expect("sps");
        parser.parse(&units[1]).expect("pps");
        let ParsedNalu::Slice(slice) = parser.parse(&units[2]).expect("slice") else {
            panic!("want slice");
        };
        assert!(slice.nal().is_idr());
        assert_eq!(slice.header().slice_type(), SliceType::I);
        assert_eq!(slice.header().first_mb_in_slice(), 0);
        assert_eq!(slice.payload()[0], 0x65);
    }

    #[test]
    fn truncated_sps_errors() {
        let units = split(&[vec![0x67]]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::Truncated { .. }
        ));
    }

    #[test]
    fn slice_before_tables_errors() {
        let units = split(&[hex(DEMO_SLICE)]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).unwrap_err(),
            crate::error::ParseError::MissingSet { .. }
        ));
    }

    #[test]
    fn sei_parses_as_other() {
        let units = split(&[vec![0x06, 0x05, 0xFF]]);
        let mut parser = H264Parser::new();
        assert!(matches!(
            parser.parse(&units[0]).expect("sei"),
            ParsedNalu::Other
        ));
    }
}
