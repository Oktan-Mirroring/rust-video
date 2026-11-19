//! H.264 Annex-B parameter sets and slice headers.
//!
//! Set tables and tier views over the shared grammar output.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::bits::Rbsp;
use crate::parse::nal::Nalu as RawNalu;

use rust_video_h264::{
    parse_pps_ids, parse_slice_prefix, parse_slice_rest, H264Error, ParseKind, PermissiveGate,
    UnsupportedKind,
};
use rust_video_h264::{
    BitReader as SharedBitReader, Marking as SharedMarking, Mmco as SharedMmco, Pps as SharedPps,
    RefMod as SharedRefMod, SliceHeader as SharedSliceHeader, SlicePoc as SharedPoc,
    SliceType as SharedSliceType, Sps as SharedSps,
};
use rust_video_h264::{ScalingMap as SharedMap, ScalingSets as SharedScaling};

pub(crate) use rust_video_h264::{HrdParams, VuiParams};

impl H264Error for ParseError {
    /// Parse failure of `kind`.
    fn parse(kind: ParseKind) -> Self {
        match kind {
            ParseKind::Truncated { syntax } => Self::Truncated { syntax },
            ParseKind::OverflowU32 { syntax } | ParseKind::OverflowI32 { syntax } => {
                Self::Overflow { syntax }
            }
            ParseKind::TooWide { syntax } | ParseKind::Range { syntax } => {
                Self::OutOfRange { syntax }
            }
            ParseKind::NalForbiddenBit => Self::Unsupported {
                what: "nal forbidden bit",
            },
            ParseKind::SpsWithoutBody => Self::Truncated { syntax: "sps" },
            ParseKind::PpsWithoutBody => Self::Truncated { syntax: "pps" },
            ParseKind::SliceWithoutHeader => Self::Truncated {
                syntax: "slice header",
            },
            ParseKind::SliceHeaderTooLong => Self::OutOfRange {
                syntax: "slice header",
            },
            ParseKind::PpsCitesUnknownSps => Self::Unsupported {
                what: "pps cites unknown sps",
            },
            ParseKind::SliceCitesUnknownPps => Self::Unsupported {
                what: "slice cites unknown pps",
            },
            ParseKind::SliceCitesUnknownSps => Self::Unsupported {
                what: "slice cites unknown sps",
            },
            ParseKind::PictureSlicesDisagree => Self::Unsupported {
                what: "picture slices disagree",
            },
            ParseKind::NonIntraSliceInIdr => Self::OutOfRange {
                syntax: "slice type",
            },
            ParseKind::FrameCropOutsideCodedSize => Self::OutOfRange {
                syntax: "frame crop",
            },
        }
    }

    /// Tier-declined construct of `kind`.
    fn unsupported(kind: UnsupportedKind) -> Self {
        match kind {
            UnsupportedKind::Interlaced => Self::Unsupported {
                what: "field pictures",
            },
            UnsupportedKind::ChromaOutside420
            | UnsupportedKind::LumaDepthAbove8Bit
            | UnsupportedKind::ChromaDepthAbove8Bit => Self::Unsupported {
                what: "h264 sampling",
            },
            UnsupportedKind::TransformBypass => Self::Unsupported {
                what: "transform bypass",
            },
            UnsupportedKind::FlexibleMacroblockOrder => Self::Unsupported {
                what: "slice groups",
            },
            UnsupportedKind::DataPartitions => Self::Unsupported {
                what: "data partitioning",
            },
            UnsupportedKind::Multiview => Self::Unsupported { what: "multiview" },
            UnsupportedKind::SpSiSlices => Self::Unsupported {
                what: "sp/si slices",
            },
            UnsupportedKind::RedundantPictures => Self::Unsupported {
                what: "redundant pictures",
            },
            UnsupportedKind::ArbitrarySliceOrder => Self::Unsupported {
                what: "arbitrary slice order",
            },
        }
    }
}

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
    /// Shared grammar output behind the accessors.
    shared: SharedSps,
}

impl Sps {
    /// Sequence set from the shared grammar output.
    fn from_shared(shared: SharedSps) -> Self {
        let mut offsets = [0; 256];
        offsets[..255].copy_from_slice(&shared.cycle_offsets);
        Self {
            profile_idc: shared.profile_idc,
            constraints: shared.constraints >> 2,
            level_idc: shared.level_idc,
            id: shared.seq_parameter_set_id,
            chroma: ChromaFormat {
                idc: shared.fidelity.chroma_format_idc,
                separate_plane: shared.fidelity.separate_plane,
            },
            bit_depth_luma_minus8: shared.fidelity.bit_depth_luma_minus8,
            bit_depth_chroma_minus8: shared.fidelity.bit_depth_chroma_minus8,
            qpprime_y_zero_transform_bypass: shared.fidelity.transform_bypass,
            scaling: convert_scaling(&shared.scaling, shared.scaling_map),
            log2_max_frame_num_minus4: shared.log2_max_frame_num_minus4,
            pic_order_cnt_type: shared.poc_type.id(),
            poc: PocTypeParams {
                lsb_bits: shared.log2_max_poc_lsb_minus4,
                always_zero: shared.delta_zero,
                non_ref: shared.offset_non_ref,
                top_bottom: shared.offset_top_bottom,
                cycle: shared.cycle,
                offsets,
            },
            max_num_ref_frames: shared.max_num_ref_frames,
            gaps_in_frame_num_allowed: shared.gaps_allowed,
            pic_width_in_mbs_minus1: shared.width_in_mbs_minus1,
            pic_height_in_map_units_minus1: shared.height_in_mbs_minus1,
            frame: FrameCoding {
                mbs_only: true,
                adaptive_field: false,
                direct_8x8: shared.direct_8x8,
            },
            frame_cropping: shared.crop_offsets.is_some(),
            crop: shared.crop_offsets.unwrap_or([0; 4]),
            vui: shared.vui.clone(),
            shared,
        }
    }

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

    /// Crop left and top in luma samples, zero without a window.
    #[must_use]
    pub fn crop_origin(&self) -> (u32, u32) {
        if !self.frame_cropping {
            return (0, 0);
        }
        let (unit_x, unit_y) = self.crop_unit();
        (
            self.crop[0].saturating_mul(unit_x),
            self.crop[2].saturating_mul(unit_y),
        )
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
    /// Shared grammar output behind the accessors.
    shared: SharedPps,
}

impl Pps {
    /// Picture set from the shared grammar output.
    fn from_shared(shared: SharedPps) -> Self {
        Self {
            id: shared.pic_parameter_set_id,
            sps_id: shared.seq_parameter_set_id,
            presence: SliceHeaderPresence {
                bottom_field_pic_order: shared.pic_order_present(),
                deblocking_control: shared.deblocking_present(),
                redundant_pic_cnt: shared.redundant_present(),
            },
            coding: PictureCoding {
                entropy_coding: shared.entropy(),
                transform_8x8: shared.transform_8x8(),
                constrained_intra: shared.constrained_intra(),
            },
            num_ref_idx_l0_default_minus1: u32::from(shared.num_ref_l0),
            num_ref_idx_l1_default_minus1: u32::from(shared.num_ref_l1),
            weighted_pred: shared.weighted_pred(),
            weighted_bipred_idc: shared.weighted_bipred_idc,
            pic_init_qp_minus26: i32::from(shared.pic_init_qp),
            pic_init_qs_minus26: i32::from(shared.pic_init_qs),
            chroma_qp_index_offset: i32::from(shared.chroma_qp),
            scaling: convert_scaling(&shared.scaling, shared.scaling_map),
            second_chroma_qp_index_offset: i32::from(shared.second_chroma_qp),
            shared,
        }
    }

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

/// Scaling lists from resolved values with their presence map.
fn convert_scaling(scaling: &SharedScaling, map: Option<SharedMap>) -> Option<ScalingLists> {
    let map = map?;
    let mut lists = ScalingLists {
        present_mask: map.present_mask,
        default_mask: map.default_mask,
        list_4x4: [[16_u8; 16]; 6],
        list_8x8: [[16_u8; 64]; 6],
    };
    for (index, list) in lists.list_4x4.iter_mut().enumerate() {
        if uses_parsed(map, index) {
            *list = scaling.four[index];
        }
    }
    for (index, list) in lists.list_8x8.iter_mut().enumerate() {
        if uses_parsed(map, 6 + index) {
            *list = scaling.eight[index];
        }
    }
    Some(lists)
}

/// True for a present list without the default-matrix escape.
fn uses_parsed(map: SharedMap, bit: usize) -> bool {
    map.present_mask & (1u16 << bit) != 0 && map.default_mask & (1u16 << bit) == 0
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

impl From<SharedSliceType> for SliceType {
    /// Slice type from the shared grammar output.
    fn from(value: SharedSliceType) -> Self {
        match value {
            SharedSliceType::P => Self::P,
            SharedSliceType::B => Self::B,
            SharedSliceType::I => Self::I,
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

impl From<SharedMmco> for Mmco {
    /// Operation from the shared grammar output.
    fn from(value: SharedMmco) -> Self {
        match value {
            SharedMmco::ForgetShort(gap) => Self::ForgetShort(gap),
            SharedMmco::ForgetLong(index) => Self::ForgetLong(index),
            SharedMmco::ShortToLong(diff, index) => Self::LongToShort {
                index: diff,
                max_plus1: index,
            },
            SharedMmco::SetMaxLong(max_plus1) => Self::SetMaxLong(max_plus1),
            SharedMmco::MarkAllUnused => Self::Reset,
            SharedMmco::CurrentToLong(index) => Self::CurrentToLong(index),
        }
    }
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

impl From<&SharedMarking> for RefPicMarking {
    /// Marking from the shared grammar output.
    fn from(value: &SharedMarking) -> Self {
        match value {
            SharedMarking::Idr {
                no_output,
                long_term,
            } => Self {
                no_output_of_prior_pics: *no_output,
                long_term_reference: *long_term,
                adaptive: false,
                ops: Vec::new(),
            },
            SharedMarking::Adaptive(operations) => Self {
                no_output_of_prior_pics: false,
                long_term_reference: false,
                adaptive: true,
                ops: operations.iter().copied().map(Mmco::from).collect(),
            },
            SharedMarking::Silent => Self::default(),
        }
    }
}

/// One reference-list modification entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RefMod {
    /// Modification idc: 0 short down, 1 short up, 2 long.
    idc: u32,
    /// Picture number gap or long-term index.
    value: u32,
}

impl From<SharedRefMod> for RefMod {
    /// Entry from the shared grammar output.
    fn from(value: SharedRefMod) -> Self {
        match value {
            SharedRefMod::ShortBelow(diff) => Self {
                idc: 0,
                value: diff,
            },
            SharedRefMod::ShortAbove(diff) => Self {
                idc: 1,
                value: diff,
            },
            SharedRefMod::Long(index) => Self {
                idc: 2,
                value: index,
            },
        }
    }
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

impl SliceHeader {
    /// Slice header from the shared grammar output.
    fn from_shared(header: &SharedSliceHeader, pps: &SharedPps) -> Result<Self, ParseError> {
        let (pic_order_cnt_lsb, delta_pic_order_cnt_bottom, delta_pic_order_cnt) =
            convert_poc(&header.poc);
        let (num_ref_idx_l0_active, num_ref_idx_l1_active) = active_counts(header, pps);
        Ok(Self {
            first_mb_in_slice: header.first_mb,
            slice_type: SliceType::from(header.slice_type),
            frame_num: narrow_u16(header.frame_num, "slice frame_num")?,
            idr_pic_id: header.idr_pic_id.unwrap_or(0),
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            num_ref_idx_l0_active,
            num_ref_idx_l1_active,
            ref_mods: RefListMod {
                l0: header
                    .ref_mod_l0
                    .iter()
                    .copied()
                    .map(RefMod::from)
                    .collect(),
                l1: header
                    .ref_mod_l1
                    .iter()
                    .copied()
                    .map(RefMod::from)
                    .collect(),
            },
            marking: RefPicMarking::from(&header.marking),
        })
    }
}

/// Slice POC fields from the shared grammar output.
fn convert_poc(poc: &SharedPoc) -> (u32, i32, [i32; 2]) {
    match *poc {
        SharedPoc::Lsb(lsb, bottom) => (lsb, bottom, [0; 2]),
        SharedPoc::Deltas(first, second) => (0, 0, [first, second.unwrap_or(0)]),
        SharedPoc::None => (0, 0, [0; 2]),
    }
}

/// Active reference counts from the override or the set defaults.
fn active_counts(header: &SharedSliceHeader, pps: &SharedPps) -> (u32, u32) {
    let l0_default = u32::from(pps.num_ref_l0) + 1;
    let l1_default = u32::from(pps.num_ref_l1) + 1;
    match header.slice_type {
        SharedSliceType::I => (l0_default, l1_default),
        SharedSliceType::P => (u32::from(header.num_ref_l0(pps)) + 1, l1_default),
        SharedSliceType::B => (
            u32::from(header.num_ref_l0(pps)) + 1,
            u32::from(header.num_ref_l1(pps)) + 1,
        ),
    }
}

/// Checked narrowing of a parsed value to `u16`.
fn narrow_u16(value: u32, syntax: &'static str) -> Result<u16, ParseError> {
    u16::try_from(value).map_err(|_| ParseError::OutOfRange { syntax })
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
        match header.unit_type {
            NalType::Sps => {
                let shared =
                    rust_video_h264::parse_sps::<ParseError, PermissiveGate>(rbsp.bytes())?;
                let sps = Rc::new(Sps::from_shared(shared));
                self.sps.insert(sps.id(), Rc::clone(&sps));
                Ok(ParsedNalu::Sps(sps))
            }
            NalType::Pps => {
                let body = rbsp.bytes();
                let (_, sps_id) = parse_pps_ids::<ParseError>(body)?;
                let sps = self.sps.get(&sps_id).map(|sps| &sps.shared);
                let shared = rust_video_h264::parse_pps::<ParseError, PermissiveGate>(body, sps)?;
                let pps = Rc::new(Pps::from_shared(shared));
                self.pps.insert(pps.id(), Rc::clone(&pps));
                Ok(ParsedNalu::Pps(pps))
            }
            NalType::Slice | NalType::Idr => {
                let slice = self.parse_slice(rbsp.bytes(), header, nalu.bytes())?;
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
    fn parse_slice(&self, body: &[u8], nal: NalHeader, bytes: &[u8]) -> Result<Slice, ParseError> {
        let mut reader = SharedBitReader::new(body);
        let prefix = parse_slice_prefix::<ParseError>(&mut reader)?;
        let pps = self
            .pps
            .get(&prefix.3)
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(prefix.3),
            })?;
        let sps = self
            .sps
            .get(&pps.sps_id())
            .cloned()
            .ok_or(ParseError::MissingSet {
                id: u32::from(pps.sps_id()),
            })?;
        let shared = parse_slice_rest::<ParseError, PermissiveGate>(
            &mut reader,
            prefix,
            nal.is_idr(),
            nal.ref_idc(),
            &sps.shared,
            &pps.shared,
        )?;
        Ok(Slice {
            nal,
            header: SliceHeader::from_shared(&shared, &pps.shared)?,
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
