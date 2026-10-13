// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Can't reasonably expect client code to consume everything that has been parsed.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::rc::Rc;

use crate::bitstream::BitReader;
use crate::h264::nalu;
use crate::h264::nalu::Header;
use crate::h264::picture::Field;

pub type Nalu<'a> = nalu::Nalu<'a, NaluHeader>;

pub(super) const DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42,
];

pub(super) const DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34,
];

pub(super) const DEFAULT_8X8_INTRA: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
];

pub(super) const DEFAULT_8X8_INTER: [u8; 64] = [
    9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22,
    22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
    27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
];

const MAX_PPS_COUNT: u16 = 256;
const MAX_SPS_COUNT: u8 = 32;

/// The maximum number of pictures in the DPB, as per A.3.1, clause h)
const DPB_MAX_SIZE: u32 = 16;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rect<T> {
    pub min: Point<T>,
    pub max: Point<T>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum NaluType {
    Unknown = 0,
    Slice = 1,
    SliceDpa = 2,
    SliceDpb = 3,
    SliceDpc = 4,
    SliceIdr = 5,
    Sei = 6,
    Sps = 7,
    Pps = 8,
    AuDelimiter = 9,
    SeqEnd = 10,
    StreamEnd = 11,
    FillerData = 12,
    SpsExt = 13,
    PrefixUnit = 14,
    SubsetSps = 15,
    DepthSps = 16,
    SliceAux = 19,
    SliceExt = 20,
    SliceDepth = 21,
}

impl TryFrom<u8> for NaluType {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(NaluType::Unknown),
            1 => Ok(NaluType::Slice),
            2 => Ok(NaluType::SliceDpa),
            3 => Ok(NaluType::SliceDpb),
            4 => Ok(NaluType::SliceDpc),
            5 => Ok(NaluType::SliceIdr),
            6 => Ok(NaluType::Sei),
            7 => Ok(NaluType::Sps),
            8 => Ok(NaluType::Pps),
            9 => Ok(NaluType::AuDelimiter),
            10 => Ok(NaluType::SeqEnd),
            11 => Ok(NaluType::StreamEnd),
            12 => Ok(NaluType::FillerData),
            13 => Ok(NaluType::SpsExt),
            14 => Ok(NaluType::PrefixUnit),
            15 => Ok(NaluType::SubsetSps),
            16 => Ok(NaluType::DepthSps),
            19 => Ok(NaluType::SliceAux),
            20 => Ok(NaluType::SliceExt),
            21 => Ok(NaluType::SliceDepth),
            _ => Err(format!("Invalid NaluType {value}")),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefPicListModification {
    pub modification_of_pic_nums_idc: u8,
    /* if modification_of_pic_nums_idc == 0 || 1 */
    pub abs_diff_pic_num_minus1: u32,
    /* if modification_of_pic_nums_idc == 2 */
    pub long_term_pic_num: u32,
    /* if modification_of_pic_nums_idc == 4 || 5 */
    pub abs_diff_view_idx_minus1: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PredWeightTable {
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,

    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i8; 32],

    /* if seq->ChromaArrayType != 0 */
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i8; 2]; 32],

    /* if slice->slice_type % 5 == 1 */
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],

    /* and if seq->ChromaArrayType != 0 */
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i8; 2]; 32],
}

/// Representation of `MaxLongTermFrameIdx`.
///
/// `MaxLongTermFrameIdx` is derived from `max_long_term_frame_idx_plus1`, an unsigned integer with
/// a special value indicating "no long-term frame indices". This type allows easy conversion
/// between the actual and "plus1" representation, while ensuring that the special value is always
/// handled by the code.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxLongTermFrameIdx {
    #[default]
    NoLongTermFrameIndices,
    Idx(u32),
}

impl MaxLongTermFrameIdx {
    /// Create a value from `max_long_term_frame_idx_plus1`.
    pub fn from_value_plus1(max_long_term_frame_idx_plus1: u32) -> Self {
        match max_long_term_frame_idx_plus1 {
            0 => Self::NoLongTermFrameIndices,
            i @ 1.. => Self::Idx(i - 1),
        }
    }

    /// Convert this value to the representation used by `max_long_term_frame_idx_plus1`.
    pub fn to_value_plus1(self) -> u32 {
        match self {
            Self::NoLongTermFrameIndices => 0,
            Self::Idx(i) => i + 1,
        }
    }
}

impl PartialEq<u32> for MaxLongTermFrameIdx {
    fn eq(&self, other: &u32) -> bool {
        match self {
            MaxLongTermFrameIdx::NoLongTermFrameIndices => false,
            MaxLongTermFrameIdx::Idx(idx) => idx.eq(other),
        }
    }
}

impl PartialOrd<u32> for MaxLongTermFrameIdx {
    fn partial_cmp(&self, other: &u32) -> Option<std::cmp::Ordering> {
        match self {
            MaxLongTermFrameIdx::NoLongTermFrameIndices => Some(std::cmp::Ordering::Less),
            MaxLongTermFrameIdx::Idx(idx) => Some(idx.cmp(other)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefPicMarkingInner {
    /// Specifies a control operation to be applied to affect the reference
    /// picture marking. The `memory_management_control_operation` syntax element
    /// is followed by data necessary for the operation specified by the value
    /// of `memory_management_control_operation`. The values and control
    /// operations associated with `memory_management_control_operation` are
    /// specified in Table 7-9
    pub memory_management_control_operation: u8,

    /// Used (with `memory_management_control_operation` equal to 3 or 1) to
    /// assign a long-term frame index to a short-term reference picture or to
    /// mark a short-term reference picture as "unused for reference".
    pub difference_of_pic_nums_minus1: u32,

    /// Used (with `memory_management_control_operation` equal to 2) to mark a
    /// long-term reference picture as "unused for reference".
    pub long_term_pic_num: u32,

    /// Used (with `memory_management_control_operation` equal to 3 or 6) to
    /// assign a long-term frame index to a picture.
    pub long_term_frame_idx: u32,

    /// Specifies the maximum value of long-term frame index allowed for
    /// long-term reference pictures (until receipt of another value of
    /// `max_long_term_frame_idx_plus1`).
    pub max_long_term_frame_idx: MaxLongTermFrameIdx,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefPicMarking {
    /// Specifies how the previously-decoded pictures in the decoded picture
    /// buffer are treated after decoding of an IDR picture. See Annex C.
    pub no_output_of_prior_pics_flag: bool,

    /// If unset, specifies that the `MaxLongTermFrameIdx` variable is set equal
    /// to "no long-term frame indices" and that the IDR picture is marked as
    /// "used for short-term reference". If set, specifies that the
    /// `MaxLongTermFrameIdx` variable is set equal to 0 and that the current IDR
    /// picture is marked "used for long-term reference" and is assigned
    /// `LongTermFrameIdx` equal to 0.
    pub long_term_reference_flag: bool,

    /// Selects the reference picture marking mode of the currently decoded
    /// picture as specified in Table 7-8.
    pub adaptive_ref_pic_marking_mode_flag: bool,

    /// An Vec with additional data used in the marking process.
    pub inner: Vec<RefPicMarkingInner>,
}

/// Presence of reference-list modification data for each list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListModificationFlags {
    /// Modification data present for list 0.
    pub l0: bool,
    /// Modification data present for list 1.
    pub l1: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SliceHeader {
    /// Specifies the address of the first macroblock in the slice.
    pub first_mb_in_slice: u32,

    /// Specifies the coding type of the slice according to Table 7-6.
    pub slice_type: SliceType,

    // Specifies the picture parameter set in use
    pub pic_parameter_set_id: u8,

    /// Specifies the colour plane associated with the current slice RBSP when
    /// `separate_colour_plane_flag` is set.
    pub colour_plane_id: u8,

    /// Used as an identifier for pictures and shall be represented by
    /// `log2_max_frame_num_minus4 + 4` bits in the bitstream.
    pub frame_num: u16,

    /// Picture structure coded by the slice.
    pub field: Field,

    /// Identifies an IDR picture. The values of `idr_pic_id` in all the slices
    /// of an IDR picture shall remain unchanged. When two consecutive access
    /// units in decoding order are both IDR access units, the value of
    /// `idr_pic_id` in the slices of the first such IDR access unit shall
    /// differ from the `idr_pic_id` in the second such IDR access unit
    pub idr_pic_id: u16,

    /// Specifies the picture order count modulo `MaxPicOrderCntLsb` for the top
    /// field of a coded frame or for a coded field. The length of the
    /// `pic_order_cnt_lsb` syntax element is
    /// `log2_max_pic_order_cnt_lsb_minus4` + 4 bits.
    pub pic_order_cnt_lsb: u16,

    ///  Specifies the picture order count difference between the bottom field
    ///  and the top field of a coded frame as follows
    pub delta_pic_order_cnt_bottom: i32,

    /// The first entry specifies the picture order count difference from the
    /// expected picture order count for the top field of a coded frame or for a
    /// coded field as specified in clause 8.2.1 The second entry  specifies the
    /// picture order count difference from the expected picture order count for
    /// the bottom field of a coded frame specified in clause 8.2.1.
    pub delta_pic_order_cnt: [i32; 2],

    /// This value is required by V4L2 stateless decode params so it is calculated
    /// by parser while processing slice header.
    pub pic_order_cnt_bit_size: usize,

    /// Shall be equal to 0 for slices and slice data partitions belonging to
    /// the primary coded picture. The value of `redundant_pic_cnt shall` be
    /// greater than 0 for coded slices or coded slice data partitions of a
    /// redundant coded picture
    pub redundant_pic_cnt: u8,

    /// Specifies the method used in the decoding process to derive motion
    /// vectors and reference indices for inter prediction >
    pub direct_spatial_mv_pred_flag: bool,

    /// If set, specifies that the syntax element `num_ref_idx_l0_active_minus1`
    /// is present for P, SP, and B slices and that the syntax element
    /// `num_ref_idx_l1_active_minus1` is present for B slices. If not set,
    /// specifies that the syntax elements `num_ref_idx_l0_active_minus1` and
    /// `num_ref_idx_l1_active_minus1` are not present.
    pub num_ref_idx_active_override_flag: bool,

    /// Specifies the maximum reference index for reference picture list 0 that
    /// shall be used to decode the slice.
    pub num_ref_idx_l0_active_minus1: u8,

    /// Specifies the maximum reference index for reference picture list 1 that
    /// shall be used to decode the slice.
    pub num_ref_idx_l1_active_minus1: u8,

    /// Reference picture list 0 modification presence.
    pub list_modification: ListModificationFlags,

    /// Reference picture list 0 modification as parsed with the
    /// `ref_pic_list_modification()` process.
    pub ref_pic_list_modification_l0: Vec<RefPicListModification>,

    /// Reference picture list 1 modification as parsed with the
    /// `ref_pic_list_modification()` process.
    pub ref_pic_list_modification_l1: Vec<RefPicListModification>,

    /// Prediction weight table as parsed using 7.3.3.2
    pub pred_weight_table: PredWeightTable,

    /// Decoded reference picture marking parsed using 7.3.3.3
    pub dec_ref_pic_marking: RefPicMarking,

    /// This value is required by V4L2 stateless decode params so it is calculated
    /// by parser while processing slice header.
    pub dec_ref_pic_marking_bit_size: usize,

    /// Specifies the index for determining the initialization table used in the
    /// initialization process for context variables.
    pub cabac_init_idc: u8,

    /// Specifies the initial value of QP Y to be used for all the macroblocks
    /// in the slice until modified by the value of `mb_qp_delta` in the
    /// macroblock layer. The initial QPY quantization parameter for the slice
    /// is computed using 7-30.
    pub slice_qp_delta: i8,

    /// Specifies the decoding process to be used to decode P macroblocks in an
    /// SP slice.
    pub sp_for_switch_flag: bool,

    /// Specifies the value of QSY for all the macroblocks in SP and SI slices.
    /// The QSY quantization parameter for the slice is computed using 7-31.
    pub slice_qs_delta: i8,

    /// Specifies whether the operation of the deblocking filter shall be
    /// disabled across some block edges of the slice and specifies for which
    /// edges the filtering is disabled.
    pub disable_deblocking_filter_idc: u8,

    /// Specifies the offset used in accessing the α and tC0 deblocking filter
    /// tables for filtering operations controlled by the macroblocks within the
    /// slice. From this value, the offset that shall be applied when addressing
    /// these tables shall be computed using 7-32.
    pub slice_alpha_c0_offset_div2: i8,

    /// Specifies the offset used in accessing the β deblocking filter table for
    /// filtering operations controlled by the macroblocks within the slice.
    /// From this value, the offset that is applied when addressing the β table
    /// of the deblocking filter shall be computed using 7-33.
    pub slice_beta_offset_div2: i8,

    /// Same as `MaxPicNum` in the specification.
    pub max_pic_num: u32,

    /// Size of the `slice_header()` in bits
    pub header_bit_size: usize,

    /// Number of emulation prevention bytes (EPB) in this `slice_header()`
    pub n_emulation_prevention_bytes: usize,
}

pub struct SliceHeaderBuilder(SliceHeader);

impl SliceHeaderBuilder {
    pub fn new(pps: &Pps) -> Self {
        SliceHeaderBuilder(SliceHeader {
            pic_parameter_set_id: pps.pic_parameter_set_id,
            ..Default::default()
        })
    }

    pub fn slice_type(mut self, type_: SliceType) -> Self {
        self.0.slice_type = type_;
        self
    }

    pub fn first_mb_in_slice(mut self, value: u32) -> Self {
        self.0.first_mb_in_slice = value;
        self
    }

    pub fn pic_order_cnt_lsb(mut self, value: u16) -> Self {
        self.0.pic_order_cnt_lsb = value;
        self
    }

    pub fn idr_pic_id(mut self, value: u16) -> Self {
        self.0.idr_pic_id = value;
        self
    }

    pub fn num_ref_idx_active_override_flag(mut self, value: bool) -> Self {
        self.0.num_ref_idx_active_override_flag = value;
        self
    }

    pub fn num_ref_idx_l0_active_minus1(mut self, value: u8) -> Self {
        self = self.num_ref_idx_active_override_flag(true);
        self.0.num_ref_idx_l0_active_minus1 = value;
        self
    }

    pub fn num_ref_idx_l0_active(self, value: u8) -> Self {
        self.num_ref_idx_l0_active_minus1(value - 1)
    }

    pub fn num_ref_idx_l1_active_minus1(mut self, value: u8) -> Self {
        self = self.num_ref_idx_active_override_flag(true);
        self.0.num_ref_idx_l1_active_minus1 = value;
        self
    }

    pub fn num_ref_idx_l1_active(self, value: u8) -> Self {
        self.num_ref_idx_l1_active_minus1(value - 1)
    }

    pub fn build(self) -> SliceHeader {
        self.0
    }
}

/// A H264 slice. An integer number of macroblocks or macroblock pairs ordered
/// consecutively in the raster scan within a particular slice group
pub struct Slice<'a> {
    /// The slice header.
    pub header: SliceHeader,
    /// The NAL unit backing this slice.
    pub nalu: Nalu<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// See table 7-6 in the specification.
#[derive(Default)]
pub enum SliceType {
    #[default]
    P = 0,
    B = 1,
    I = 2,
    Sp = 3,
    Si = 4,
}

impl TryFrom<u8> for SliceType {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(SliceType::P),
            1 => Ok(SliceType::B),
            2 => Ok(SliceType::I),
            3 => Ok(SliceType::Sp),
            4 => Ok(SliceType::Si),
            _ => Err(format!("Invalid SliceType {value}")),
        }
    }
}

impl SliceType {
    /// Whether this is a P slice. See table 7-6 in the specification.
    pub fn is_p(self) -> bool {
        matches!(self, SliceType::P)
    }

    /// Whether this is a B slice. See table 7-6 in the specification.
    pub fn is_b(self) -> bool {
        matches!(self, SliceType::B)
    }

    /// Whether this is an I slice. See table 7-6 in the specification.
    pub fn is_i(self) -> bool {
        matches!(self, SliceType::I)
    }

    /// Whether this is a SP slice. See table 7-6 in the specification.
    pub fn is_sp(self) -> bool {
        matches!(self, SliceType::Sp)
    }

    /// Whether this is a SI slice. See table 7-6 in the specification.
    pub fn is_si(self) -> bool {
        matches!(self, SliceType::Si)
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Profile {
    Baseline = 66,
    Main = 77,
    Extended = 88,
    High = 100,
    High10 = 110,
    High422P = 122,
}

impl TryFrom<u8> for Profile {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            66 => Ok(Profile::Baseline),
            77 => Ok(Profile::Main),
            88 => Ok(Profile::Extended),
            100 => Ok(Profile::High),
            110 => Ok(Profile::High10),
            122 => Ok(Profile::High422P),
            _ => Err(format!("Invalid Profile {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    L1 = 10,
    L1B = 9,
    L1_1 = 11,
    L1_2 = 12,
    L1_3 = 13,
    L2_0 = 20,
    L2_1 = 21,
    L2_2 = 22,
    L3 = 30,
    L3_1 = 31,
    L3_2 = 32,
    L4 = 40,
    L4_1 = 41,
    L4_2 = 42,
    L5 = 50,
    L5_1 = 51,
    L5_2 = 52,
    L6 = 60,
    L6_1 = 61,
    L6_2 = 62,
}

impl TryFrom<u8> for Level {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            10 => Ok(Level::L1),
            9 => Ok(Level::L1B),
            11 => Ok(Level::L1_1),
            12 => Ok(Level::L1_2),
            13 => Ok(Level::L1_3),
            20 => Ok(Level::L2_0),
            21 => Ok(Level::L2_1),
            22 => Ok(Level::L2_2),
            30 => Ok(Level::L3),
            31 => Ok(Level::L3_1),
            32 => Ok(Level::L3_2),
            40 => Ok(Level::L4),
            41 => Ok(Level::L4_1),
            42 => Ok(Level::L4_2),
            50 => Ok(Level::L5),
            51 => Ok(Level::L5_1),
            52 => Ok(Level::L5_2),
            60 => Ok(Level::L6),
            61 => Ok(Level::L6_1),
            62 => Ok(Level::L6_2),
            _ => Err(format!("Invalid Level {value}")),
        }
    }
}

/// Chroma sampling of the sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromaFormat {
    /// `chroma_format_idc` value.
    pub idc: u8,
    /// Colour planes coded separately.
    pub separate_plane: bool,
}

impl ChromaFormat {
    /// `ChromaArrayType`: 0 for separate planes, else the format value.
    pub const fn array_type(self) -> u8 {
        if self.separate_plane {
            0
        } else {
            self.idc
        }
    }

    /// `(SubWidthC, SubHeightC)` per Table 6-1.
    const fn sub_width_height_c(self) -> (u32, u32) {
        match (self.idc, self.separate_plane) {
            (1, false) => (2, 2),
            (2, false) => (2, 1),
            _ => (1, 1),
        }
    }
}

/// Luma/chroma bit depth of the sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BitDepth {
    /// `bit_depth_luma_minus8` value.
    pub luma_minus8: u8,
    /// `bit_depth_chroma_minus8` value.
    pub chroma_minus8: u8,
    /// Transform bypass for zero luma QP.
    pub transform_bypass: bool,
}

/// Sequence scaling lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceScaling {
    /// Scaling-matrix syntax present.
    pub present: bool,
    /// 4x4 lists.
    pub lists_4x4: [[u8; 16]; 6],
    /// 8x8 lists.
    pub lists_8x8: [[u8; 64]; 6],
}

impl Default for SequenceScaling {
    fn default() -> Self {
        Self {
            present: false,
            lists_4x4: [[0; 16]; 6],
            lists_8x8: [[0; 64]; 6],
        }
    }
}

/// Frame/field macroblock coding of the sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameFieldCoding {
    /// Every picture is a frame of frame macroblocks.
    pub frame_mbs_only: bool,
    /// Frame/field macroblock switching within frames.
    pub mb_adaptive: bool,
}

/// Frame-crop rectangle in crop units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCrop {
    /// Left offset.
    pub left: u32,
    /// Right offset.
    pub right: u32,
    /// Top offset.
    pub top: u32,
    /// Bottom offset.
    pub bottom: u32,
}

/// A H264 Sequence Parameter Set. A syntax structure containing syntax elements
/// that apply to zero or more entire coded video sequences as determined by the
/// content of a `seq_parameter_set_id` syntax element found in the picture
/// parameter set referred to by the `pic_parameter_set_id` syntax element found
/// in each slice header.
#[derive(Debug, PartialEq, Eq)]
pub struct Sps {
    /// Identifies the sequence parameter set that is referred to by the picture
    /// parameter set
    pub seq_parameter_set_id: u8,

    /// Profile to which the coded video sequence conforms
    pub profile_idc: u8,

    /// `constraint_set0_flag` through `constraint_set5_flag`, indexed by set
    /// number. See 7.4.2.1.1.
    pub constraints: [bool; 6],

    /// Level to which the coded video sequence conforms
    pub level_idc: Level,

    /// Chroma sampling relative to luma sampling, clause 6.2.
    pub chroma: ChromaFormat,

    /// Bit depth of luma/chroma samples and the luma QP range offset.
    /// See 7-3 through 7-6.
    pub bit_depth: BitDepth,

    /// Sequence scaling lists, 7.3.2.1.1.1.
    pub scaling: SequenceScaling,

    /// Specifies the value of the variable `MaxFrameNum` that is used in
    /// `frame_num` related derivations as follows: `MaxFrameNum` = 2 ^
    /// (`log2_max_frame_num_minus4` + 4 )
    pub log2_max_frame_num_minus4: u8,

    /// Specifies the method to decode picture order count (as specified in
    /// clause 8.2.1)
    pub pic_order_cnt_type: u8,

    /// Specifies the value of the variable `MaxPicOrderCntLsb` that is used in
    /// the decoding process for picture order count as specified in clause
    /// 8.2.1 as follows: `MaxPicOrderCntLsb` = 2 ^ (
    /// `log2_max_pic_order_cnt_lsb_minus4` + 4 ).
    pub log2_max_pic_order_cnt_lsb_minus4: u8,

    /// If true, specifies that `delta_pic_order_cnt[0]` and
    /// `delta_pic_order_cnt[1]` are not present in the slice headers of the
    /// sequence and shall be inferred to be equal to 0.
    /// If false, specifies that `delta_pic_order_cnt[0]` is present in the
    /// slice headers of the sequence and `delta_pic_order_cnt[1]` may be
    /// present in the slice headers of the sequence.
    pub delta_pic_order_always_zero_flag: bool,

    /// Used to calculate the picture order count of a non-reference picture as
    /// specified in clause 8.2.1.
    pub offset_for_non_ref_pic: i32,

    /// Used to calculate the picture order count of a bottom field as specified
    /// in clause 8.2.1.
    pub offset_for_top_to_bottom_field: i32,

    /// Used in the decoding process for picture order count as specified in
    /// clause 8.2.1
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,

    /// An element of a list of `num_ref_frames_in_pic_order_cnt_cycle` values
    /// used in the decoding process for picture order count as specified in
    /// clause 8.2.
    pub offset_for_ref_frame: [i32; 255],

    /// Specifies the maximum number of short-term and long-term reference
    /// frames, complementary reference field pairs, and non-paired reference
    /// fields that may be used by the decoding process for inter prediction of
    /// any picture in the coded video sequence. Also
    /// determines the size of the sliding window operation as specified in
    /// clause 8.2.5.3.
    pub max_num_ref_frames: u8,

    /// Specifies the allowed values of `frame_num` as specified in clause 7.4.3
    /// and the decoding process in case of an inferred gap between values of
    /// `frame_num` as specified in clause 8.2.5.2
    pub gaps_in_frame_num_value_allowed_flag: bool,

    /// Plus 1 specifies the width of each decoded picture in units of
    /// macroblocks.
    pub pic_width_in_mbs_minus1: u16,
    /// Plus 1 specifies the height in slice group map units of a decoded frame
    /// or field.
    pub pic_height_in_map_units_minus1: u16,

    /// If true,  specifies that every coded picture of the coded video sequence
    /// is a coded frame containing only frame macroblocks, else specifies that
    /// coded pictures of the coded video sequence may either be coded fields or
    /// coded frames.
    pub frame_field: FrameFieldCoding,

    /// Specifies the method used in the derivation process for luma motion
    /// vectors for `B_Skip`, `B_Direct_16x16` and `B_Direct_8x8` as specified in
    /// clause 8.4.1.2.
    pub direct_8x8_inference_flag: bool,

    /// Frame-crop rectangle in crop units, absent when the sequence is uncropped.
    pub crop: Option<FrameCrop>,

    // Calculated
    /// Same as `ExpectedDeltaPerPicOrderCntCycle`, see 7-12 in the specification.
    pub expected_delta_per_pic_order_cnt_cycle: i32,

    /// VUI parameters, absent when `vui_parameters_present_flag` is unset.
    pub vui: Option<VuiParams>,
}

impl Sps {
    /// `constraint_set3_flag`: gates level-1b inference, Table A.1.
    fn constraint_set3(&self) -> bool {
        self.constraints[3]
    }

    /// Returns the coded width of the stream.
    ///
    /// See 7-13 through 7-17 in the specification.
    pub const fn width(&self) -> u32 {
        (self.pic_width_in_mbs_minus1 as u32 + 1) * 16
    }

    /// Returns the coded height of the stream.
    ///
    /// See 7-13 through 7-17 in the specification.
    pub const fn height(&self) -> u32 {
        (self.pic_height_in_map_units_minus1 as u32 + 1)
            * 16
            * (2 - self.frame_field.frame_mbs_only as u32)
    }

    /// Returns `ChromaArrayType`, as computed in the specification.
    pub const fn chroma_array_type(&self) -> u8 {
        self.chroma.array_type()
    }

    /// Returns `SubWidthC` and `SubHeightC`.
    ///
    /// See table 6-1 in the specification.
    fn sub_width_height_c(&self) -> (u32, u32) {
        self.chroma.sub_width_height_c()
    }

    /// Returns `CropUnitX` and `CropUnitY`.
    ///
    /// See 7-19 through 7-22 in the specification.
    fn crop_unit_x_y(&self) -> (u32, u32) {
        if self.chroma_array_type() == 0 {
            (1, 2 - u32::from(self.frame_field.frame_mbs_only))
        } else {
            let (sub_width_c, sub_height_c) = self.sub_width_height_c();
            (
                sub_width_c,
                sub_height_c * (2 - u32::from(self.frame_field.frame_mbs_only)),
            )
        }
    }

    /// Same as `MaxFrameNum`. See 7-10 in the specification.
    pub fn max_frame_num(&self) -> u32 {
        1 << (self.log2_max_frame_num_minus4 + 4)
    }

    pub fn visible_rectangle(&self) -> Rect<u32> {
        let Some(crop) = &self.crop else {
            return Rect {
                min: Point { x: 0, y: 0 },
                max: Point {
                    x: self.width(),
                    y: self.height(),
                },
            };
        };

        let (crop_unit_x, crop_unit_y) = self.crop_unit_x_y();

        let crop_left = crop_unit_x * crop.left;
        let crop_right = crop_unit_x * crop.right;
        let crop_top = crop_unit_y * crop.top;
        let crop_bottom = crop_unit_y * crop.bottom;

        Rect {
            min: Point {
                x: crop_left,
                y: crop_top,
            },
            max: Point {
                x: self.width() - crop_left - crop_right,
                y: self.height() - crop_top - crop_bottom,
            },
        }
    }

    pub fn max_dpb_frames(&self) -> usize {
        let profile = self.profile_idc;
        let mut level = self.level_idc;

        // A.3.1 and A.3.2: Level 1b for Baseline, Constrained Baseline and Main
        // profile if level_idc == 11 and constraint_set3_flag == 1
        if matches!(level, Level::L1_1)
            && (profile == Profile::Baseline as u8 || profile == Profile::Main as u8)
            && self.constraint_set3()
        {
            level = Level::L1B;
        }

        // Table A.1
        let max_dpb_mbs = match level {
            Level::L1 | Level::L1B => 396,
            Level::L1_1 => 900,
            Level::L1_2 | Level::L1_3 | Level::L2_0 => 2376,
            Level::L2_1 => 4752,
            Level::L2_2 | Level::L3 => 8100,
            Level::L3_1 => 18000,
            Level::L3_2 => 20480,
            Level::L4 | Level::L4_1 => 32768,
            Level::L4_2 => 34816,
            Level::L5 => 110_400,
            Level::L5_1 | Level::L5_2 => 184_320,
            Level::L6 | Level::L6_1 | Level::L6_2 => 696_320,
        };

        let width_mb = self.width() / 16;
        let height_mb = self.height() / 16;

        let max_dpb_frames =
            std::cmp::min(max_dpb_mbs / (width_mb * height_mb), DPB_MAX_SIZE) as usize;

        let mut max_dpb_frames = std::cmp::max(max_dpb_frames, self.max_num_ref_frames as usize);

        if let Some(vui) = &self.vui {
            if let Some(restriction) = &vui.restriction {
                max_dpb_frames = std::cmp::max(1, restriction.max_dec_frame_buffering as usize);
            }
        }

        max_dpb_frames
    }

    pub fn max_num_order_frames(&self) -> u32 {
        if let Some(restriction) = self.vui.as_ref().and_then(|vui| vui.restriction.as_ref()) {
            return restriction.max_num_reorder_frames;
        }

        let profile = self.profile_idc;
        if (profile == 44
            || profile == 86
            || profile == 100
            || profile == 110
            || profile == 122
            || profile == 244)
            && self.constraint_set3()
        {
            0
        } else {
            u32::try_from(self.max_dpb_frames()).unwrap_or(u32::MAX)
        }
    }
}

// TODO: Replace with builder
impl Default for Sps {
    fn default() -> Self {
        Self {
            scaling: SequenceScaling::default(),
            offset_for_ref_frame: [0; 255],
            seq_parameter_set_id: Default::default(),
            profile_idc: Default::default(),
            constraints: Default::default(),
            level_idc: Level::default(),
            chroma: ChromaFormat::default(),
            bit_depth: BitDepth::default(),
            log2_max_frame_num_minus4: Default::default(),
            pic_order_cnt_type: Default::default(),
            log2_max_pic_order_cnt_lsb_minus4: Default::default(),
            delta_pic_order_always_zero_flag: Default::default(),
            offset_for_non_ref_pic: Default::default(),
            offset_for_top_to_bottom_field: Default::default(),
            num_ref_frames_in_pic_order_cnt_cycle: Default::default(),
            max_num_ref_frames: Default::default(),
            gaps_in_frame_num_value_allowed_flag: Default::default(),
            pic_width_in_mbs_minus1: Default::default(),
            pic_height_in_map_units_minus1: Default::default(),
            frame_field: FrameFieldCoding::default(),
            direct_8x8_inference_flag: Default::default(),
            crop: None,
            expected_delta_per_pic_order_cnt_cycle: Default::default(),
            vui: None,
        }
    }
}

#[derive(Default)]
pub struct SpsBuilder(Sps);

impl SpsBuilder {
    pub fn new() -> Self {
        SpsBuilder::default()
    }

    pub fn seq_parameter_set_id(mut self, value: u8) -> Self {
        self.0.seq_parameter_set_id = value;
        self
    }

    pub fn profile_idc(mut self, value: Profile) -> Self {
        self.0.profile_idc = value as u8;
        self
    }

    pub fn level_idc(mut self, value: Level) -> Self {
        self.0.level_idc = value;
        self
    }

    pub fn frame_crop_offsets(mut self, top: u32, bottom: u32, left: u32, right: u32) -> Self {
        self.0.crop = Some(FrameCrop {
            left,
            right,
            top,
            bottom,
        });
        self
    }

    pub fn frame_crop(self, top: u32, bottom: u32, left: u32, right: u32) -> Self {
        let sub_width_c = if self.0.chroma.idc > 2 { 1 } else { 2 };
        let sub_height_c = if self.0.chroma.idc > 1 { 1 } else { 2 };

        let crop_unit_x = sub_width_c;
        let crop_unit_y = sub_height_c
            * (if self.0.frame_field.frame_mbs_only {
                1
            } else {
                2
            });

        self.frame_crop_offsets(
            top / crop_unit_y,
            bottom / crop_unit_y,
            left / crop_unit_x,
            right / crop_unit_x,
        )
    }

    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        const MB_SIZE: u32 = 16;

        let mb_width = width.div_ceil(MB_SIZE);
        let mb_height = height.div_ceil(MB_SIZE);

        self.0.pic_width_in_mbs_minus1 = u16::try_from(mb_width - 1).unwrap_or(u16::MAX);
        self.0.pic_height_in_map_units_minus1 = u16::try_from(mb_height - 1).unwrap_or(u16::MAX);

        let compressed_width = mb_width * MB_SIZE;
        let compressed_height = mb_height * MB_SIZE;

        if compressed_width != width || compressed_height != height {
            self = self.frame_crop(0, compressed_height - height, 0, compressed_width - width);
        }

        self
    }

    pub fn chroma_format_idc(mut self, value: u8) -> Self {
        self.0.chroma.idc = value;
        self
    }

    pub fn max_num_ref_frames(mut self, value: u8) -> Self {
        self.0.max_num_ref_frames = value;
        self
    }

    pub fn frame_mbs_only_flag(mut self, value: bool) -> Self {
        self.0.frame_field.frame_mbs_only = value;
        self
    }

    pub fn mb_adaptive_frame_field_flag(mut self, value: bool) -> Self {
        self.0.frame_field.mb_adaptive = value;
        self
    }

    pub fn seq_scaling_matrix_present_flag(mut self, value: bool) -> Self {
        self.0.scaling.present = value;
        self
    }

    pub fn direct_8x8_inference_flag(mut self, value: bool) -> Self {
        self.0.direct_8x8_inference_flag = value;
        self
    }

    pub fn vui_parameters_present(mut self) -> Self {
        self.0.vui.get_or_insert_with(VuiParams::default);
        self
    }

    pub fn aspect_ratio_idc(mut self, value: u8) -> Self {
        self = self.vui_parameters_present();
        let vui = self.0.vui.as_mut().expect("vui present");
        vui.aspect_ratio = Some(AspectRatio {
            idc: value,
            ..vui.aspect_ratio.take().unwrap_or_default()
        });
        self
    }

    pub fn sar_resolution(mut self, width: u16, height: u16) -> Self {
        self = self.aspect_ratio_idc(255);
        let aspect_ratio = self
            .0
            .vui
            .as_mut()
            .expect("vui present")
            .aspect_ratio
            .as_mut()
            .expect("aspect ratio present");
        aspect_ratio.sar_width = width;
        aspect_ratio.sar_height = height;
        self
    }

    pub fn aspect_ratio(self, width_ratio: u16, height_ratio: u16) -> Self {
        // H.264 Table E-1
        match (width_ratio, height_ratio) {
            (1, 1) => self.aspect_ratio_idc(1),
            (12, 11) => self.aspect_ratio_idc(2),
            (10, 11) => self.aspect_ratio_idc(3),
            (16, 11) => self.aspect_ratio_idc(4),
            (40, 33) => self.aspect_ratio_idc(5),
            (24, 11) => self.aspect_ratio_idc(6),
            (20, 11) => self.aspect_ratio_idc(7),
            (32, 11) => self.aspect_ratio_idc(8),
            (80, 33) => self.aspect_ratio_idc(9),
            (18, 11) => self.aspect_ratio_idc(10),
            (15, 11) => self.aspect_ratio_idc(11),
            (64, 33) => self.aspect_ratio_idc(12),
            (160, 99) => self.aspect_ratio_idc(13),
            (4, 3) => self.aspect_ratio_idc(14),
            (3, 2) => self.aspect_ratio_idc(15),
            (2, 1) => self.aspect_ratio_idc(16),

            _ => self.sar_resolution(width_ratio, height_ratio),
        }
    }

    pub fn timing_info(
        mut self,
        num_units_in_tick: u32,
        time_scale: u32,
        fixed_frame_rate_flag: bool,
    ) -> Self {
        self = self.vui_parameters_present();
        self.0.vui.as_mut().expect("vui present").timing = Some(TimingInfo {
            num_units_in_tick,
            time_scale,
            fixed_frame_rate: fixed_frame_rate_flag,
        });
        self
    }

    pub fn log2_max_frame_num_minus4(mut self, value: u8) -> Self {
        self.0.log2_max_frame_num_minus4 = value;
        self
    }

    pub fn max_frame_num(self, value: u32) -> Self {
        self.log2_max_frame_num_minus4(u8::try_from(value.ilog2()).unwrap_or(u8::MAX) - 4u8)
    }

    pub fn pic_order_cnt_type(mut self, value: u8) -> Self {
        self.0.pic_order_cnt_type = value;
        self
    }

    pub fn log2_max_pic_order_cnt_lsb_minus4(mut self, value: u8) -> Self {
        self.0.log2_max_pic_order_cnt_lsb_minus4 = value;
        self
    }

    pub fn max_pic_order_cnt_lsb(self, value: u32) -> Self {
        self.log2_max_pic_order_cnt_lsb_minus4(u8::try_from(value.ilog2()).unwrap_or(u8::MAX) - 4u8)
    }

    pub fn delta_pic_order_always_zero_flag(mut self, value: bool) -> Self {
        self.0.delta_pic_order_always_zero_flag = value;
        self
    }

    pub fn bit_depth_chroma_minus8(mut self, value: u8) -> Self {
        self.0.bit_depth.chroma_minus8 = value;
        self
    }

    pub fn bit_depth_chroma(self, value: u8) -> Self {
        self.bit_depth_luma_minus8(value - 8u8)
    }

    pub fn bit_depth_luma_minus8(mut self, value: u8) -> Self {
        self.0.bit_depth.luma_minus8 = value;
        self
    }

    pub fn bit_depth_luma(self, value: u8) -> Self {
        self.bit_depth_luma_minus8(value - 8u8)
    }

    pub fn build(self) -> Rc<Sps> {
        Rc::new(self.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HrdParams {
    /// Plus 1 specifies the number of alternative CPB specifications in the
    /// bitstream. The value of `cpb_cnt_minus1` shall be in the range of 0 to 31,
    /// inclusive
    pub cpb_cnt_minus1: u8,
    /// Together with `bit_rate_value_minus1[ SchedSelIdx ]` specifies the
    /// maximum input bit rate of the `SchedSelIdx`-th CPB.
    pub bit_rate_scale: u8,
    /// Together with `cpb_size_value_minus1[ SchedSelIdx ]` specifies the CPB
    /// size of the SchedSelIdx-th CPB.
    pub cpb_size_scale: u8,

    /// `[ SchedSelIdx ]` (together with `bit_rate_scale`) specifies the maximum
    /// input bit rate for the SchedSelIdx-th CPB.
    pub bit_rate_value_minus1: [u32; 32],
    /// `[ SchedSelIdx ]` is used together with `cpb_size_scale` to specify the
    /// SchedSelIdx-th CPB size.
    pub cpb_size_value_minus1: [u32; 32],
    /// `[ SchedSelIdx ]` equal to 0 specifies that to decode this bitstream by
    /// the HRD using the `SchedSelIdx`-th CPB specification, the hypothetical
    /// stream delivery scheduler (HSS) operates in an intermittent bit rate
    /// mode. `cbr_flag[ SchedSelIdx ]` equal to 1 specifies that the HSS operates
    /// in a constant bit rate (CBR) mode
    pub cbr_flag: [bool; 32],

    /// Specifies the length in bits of the `initial_cpb_removal_delay[
    /// SchedSelIdx ]` and `initial_cpb_removal_delay_offset[ SchedSelIdx ]` syntax
    /// elements of the buffering period SEI message.
    pub initial_cpb_removal_delay_length_minus1: u8,
    /// Specifies the length in bits of the `cpb_removal_delay` syntax element.
    pub cpb_removal_delay_length_minus1: u8,
    /// Specifies the length in bits of the `dpb_output_delay` syntax element.
    pub dpb_output_delay_length_minus1: u8,
    /// If greater than 0, specifies the length in bits of the `time_offset`
    /// syntax element. `time_offset_length` equal to 0 specifies that the
    /// `time_offset` syntax element is not present
    pub time_offset_length: u8,
}

/// Sample aspect ratio from the VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AspectRatio {
    /// `aspect_ratio_idc` value.
    pub idc: u8,
    /// Extended SAR width, valid when `idc` is 255.
    pub sar_width: u16,
    /// Extended SAR height, valid when `idc` is 255.
    pub sar_height: u16,
}

/// Colour description from the VUI video signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColourDescription {
    /// `colour_primaries` value.
    pub primaries: u8,
    /// `transfer_characteristics` value.
    pub transfer: u8,
    /// `matrix_coefficients` value.
    pub matrix: u8,
}

/// Video signal type from the VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoSignalType {
    /// `video_format` value.
    pub format: u8,
    /// Full-range luma/chroma levels.
    pub full_range: bool,
    /// Colour description, absent when not signalled.
    pub colour: Option<ColourDescription>,
}

/// Chroma sample location from the VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromaLoc {
    /// Top-field location type.
    pub top: u8,
    /// Bottom-field location type.
    pub bottom: u8,
}

/// Timing info from the VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimingInfo {
    /// Clock ticks per counter increment.
    pub num_units_in_tick: u32,
    /// Clock ticks per second.
    pub time_scale: u32,
    /// Fixed frame rate.
    pub fixed_frame_rate: bool,
}

/// Bitstream restriction from the VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BitstreamRestriction {
    /// Motion vectors may cross picture boundaries.
    pub motion_vectors_over_pic_boundaries: bool,
    /// `max_bytes_per_pic_denom` value.
    pub max_bytes_per_pic_denom: u32,
    /// `max_bits_per_mb_denom` value.
    pub max_bits_per_mb_denom: u32,
    /// `log2_max_mv_length_horizontal` value.
    pub log2_max_mv_length_horizontal: u32,
    /// `log2_max_mv_length_vertical` value.
    pub log2_max_mv_length_vertical: u32,
    /// `max_num_reorder_frames` value.
    pub max_num_reorder_frames: u32,
    /// `max_dec_frame_buffering` value.
    pub max_dec_frame_buffering: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VuiParams {
    /// Sample aspect ratio, absent when not signalled.
    pub aspect_ratio: Option<AspectRatio>,
    /// Overscan display suitability, absent when not signalled.
    pub overscan_appropriate: Option<bool>,
    /// Video signal type, absent when not signalled.
    pub video_signal: Option<VideoSignalType>,
    /// Chroma sample location, absent when not signalled.
    pub chroma_loc: Option<ChromaLoc>,
    /// Timing info, absent when not signalled.
    pub timing: Option<TimingInfo>,
    /// NAL HRD parameters, absent when not signalled.
    pub nal_hrd: Option<HrdParams>,
    /// VCL HRD parameters, absent when not signalled.
    pub vcl_hrd: Option<HrdParams>,
    /// HRD low-delay mode.
    pub low_delay_hrd_flag: bool,
    /// Picture-timing SEI carries `pic_struct`.
    pub pic_struct_present_flag: bool,
    /// Bitstream restriction, absent when not signalled.
    pub restriction: Option<BitstreamRestriction>,
}

/// Entropy-coding tools of the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PpsCodingFlags {
    /// CABAC in use for the picture.
    pub entropy_coding_mode_flag: bool,
    /// 8x8 transform decoding may be in use.
    pub transform_8x8_mode_flag: bool,
}

/// Prediction constraints of the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PpsPredictionFlags {
    /// Explicit weighted prediction for P/SP slices.
    pub weighted_pred_flag: bool,
    /// Constrained intra prediction.
    pub constrained_intra_pred_flag: bool,
}

/// Slice-header presence flags of the picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PpsPresenceFlags {
    /// Bottom-field order counts may be present.
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    /// Redundant picture counts may be present.
    pub redundant_pic_cnt_present_flag: bool,
    /// Deblocking-filter parameters may be present.
    pub deblocking_filter_control_present_flag: bool,
}

/// A H264 Picture Parameter Set. A syntax structure containing syntax elements
/// that apply to zero or more entire coded pictures as determined by the
/// `pic_parameter_set_id` syntax element found in each slice header.
#[derive(Debug, PartialEq, Eq)]
pub struct Pps {
    /// Identifies the picture parameter set that is referred to in the slice header.
    pub pic_parameter_set_id: u8,

    /// Refers to the active sequence parameter set.
    pub seq_parameter_set_id: u8,

    /// Entropy-coding tools of the picture.
    pub coding: PpsCodingFlags,

    /// Prediction constraints of the picture.
    pub prediction: PpsPredictionFlags,

    /// Slice-header presence flags of the picture.
    pub presence: PpsPresenceFlags,

    /// Plus 1 specifies the number of slice groups for a picture. When
    /// `num_slice_groups_minus1` is equal to 0, all slices of the picture
    /// belong to the same slice group. The allowed range of
    /// `num_slice_groups_minus1` is specified in Annex A.
    pub num_slice_groups_minus1: u32,

    /// Specifies how `num_ref_idx_l0_active_minus1` is inferred for P, SP, and
    /// B slices with `num_ref_idx_active_override_flag` not set.
    pub num_ref_idx_l0_default_active_minus1: u8,

    /// Specifies how `num_ref_idx_l1_active_minus1` is inferred for B slices
    /// with `num_ref_idx_active_override_flag` not set.
    pub num_ref_idx_l1_default_active_minus1: u8,

    /// `weighted_bipred_idc` equal to 0 specifies that the default weighted
    /// prediction shall be applied to B slices. `weighted_bipred_idc` equal to
    /// 1 specifies that explicit weighted prediction shall be applied to B
    /// slices. `weighted_bipred_idc` equal to 2 specifies that implicit
    /// weighted prediction shall be applied to B slices
    pub weighted_bipred_idc: u8,

    /// Specifies the initial value minus 26 of `SliceQPY` for each slice. The
    /// initial value is modified at the slice layer when a non-zero value of
    /// `slice_qp_delta` is decoded, and is modified further when a non-zero
    /// value of `mb_qp_delta` is decoded at the macroblock layer.
    pub pic_init_qp_minus26: i8,

    /// Specifies the initial value minus 26 of `SliceQSY` for all macroblocks in
    /// SP or SI slices. The initial value is modified at the slice layer when a
    /// non-zero value of `slice_qs_delta` is decoded.
    pub pic_init_qs_minus26: i8,

    /// Specifies the offset that shall be added to QP Y and QSY for addressing
    /// the table of QPC values for the Cb chroma component.
    pub chroma_qp_index_offset: i8,

    ///  If set, specifies that parameters are present to modify the scaling
    ///  lists specified in the sequence parameter set. If not set, specifies
    ///  that the scaling lists used for the picture shall be inferred to be
    ///  equal to those specified by the sequence parameter set.
    pub pic_scaling_matrix_present_flag: bool,

    /// 4x4 Scaling list as read with 7.3.2.1.1.1
    pub scaling_lists_4x4: [[u8; 16]; 6],
    /// 8x8 Scaling list as read with 7.3.2.1.1.1
    pub scaling_lists_8x8: [[u8; 64]; 6],

    /// Specifies the offset that shall be added to QPY and QSY for addressing
    /// the table of QPC values for the Cr chroma component. When
    /// `second_chroma_qp_index_offset` is not present, it shall be inferred to be
    /// equal to `chroma_qp_index_offset`.
    pub second_chroma_qp_index_offset: i8,

    /// The SPS referenced by this PPS.
    pub sps: Rc<Sps>,
}

pub struct PpsBuilder(Pps);

impl PpsBuilder {
    pub fn new(sps: Rc<Sps>) -> Self {
        PpsBuilder(Pps {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: sps.seq_parameter_set_id,
            coding: PpsCodingFlags::default(),
            prediction: PpsPredictionFlags::default(),
            presence: PpsPresenceFlags::default(),
            num_slice_groups_minus1: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_bipred_idc: 0,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            pic_scaling_matrix_present_flag: false,
            scaling_lists_4x4: [[0; 16]; 6],
            scaling_lists_8x8: [[0; 64]; 6],
            second_chroma_qp_index_offset: 0,
            sps,
        })
    }

    pub fn pic_parameter_set_id(mut self, value: u8) -> Self {
        self.0.pic_parameter_set_id = value;
        self
    }

    pub fn pic_init_qp_minus26(mut self, value: i8) -> Self {
        self.0.pic_init_qp_minus26 = value;
        self
    }

    pub fn pic_init_qp(self, value: u8) -> Self {
        self.pic_init_qp_minus26(value.cast_signed() - 26)
    }

    pub fn deblocking_filter_control_present_flag(mut self, value: bool) -> Self {
        self.0.presence.deblocking_filter_control_present_flag = value;
        self
    }

    pub fn num_ref_idx_l0_default_active_minus1(mut self, value: u8) -> Self {
        self.0.num_ref_idx_l0_default_active_minus1 = value;
        self
    }

    pub fn num_ref_idx_l0_default_active(self, value: u8) -> Self {
        self.num_ref_idx_l0_default_active_minus1(value - 1)
    }

    pub fn num_ref_idx_l1_default_active_minus1(mut self, value: u8) -> Self {
        self.0.num_ref_idx_l1_default_active_minus1 = value;
        self
    }

    pub fn num_ref_idx_l1_default_active(self, value: u8) -> Self {
        self.num_ref_idx_l1_default_active_minus1(value - 1)
    }

    pub fn build(self) -> Rc<Pps> {
        Rc::new(self.0)
    }
}

#[derive(Debug, Default)]
pub struct Parser {
    active_spses: BTreeMap<u8, Rc<Sps>>,
    active_ppses: BTreeMap<u8, Rc<Pps>>,
}

impl Parser {
    fn fill_default_scaling_list_4x4(scaling_list4x4: &mut [u8; 16], i: usize) {
        // See table 7.2 in the spec.
        assert!(i < 6);
        if i < 3 {
            *scaling_list4x4 = DEFAULT_4X4_INTRA;
        } else if i < 6 {
            *scaling_list4x4 = DEFAULT_4X4_INTER;
        }
    }

    fn fill_default_scaling_list_8x8(scaling_list8x8: &mut [u8; 64], i: usize) {
        assert!(i < 6);
        if i.is_multiple_of(2) {
            *scaling_list8x8 = DEFAULT_8X8_INTRA;
        } else {
            *scaling_list8x8 = DEFAULT_8X8_INTER;
        }
    }

    fn fill_fallback_scaling_list_4x4(
        scaling_list4x4: &mut [[u8; 16]; 6],
        i: usize,
        default_scaling_list_intra: &[u8; 16],
        default_scaling_list_inter: &[u8; 16],
    ) {
        // See table 7.2 in the spec.
        scaling_list4x4[i] = match i {
            0 => *default_scaling_list_intra,
            1 => scaling_list4x4[0],
            2 => scaling_list4x4[1],
            3 => *default_scaling_list_inter,
            4 => scaling_list4x4[3],
            5 => scaling_list4x4[4],
            _ => panic!("Unexpected value {i}"),
        }
    }

    fn fill_fallback_scaling_list_8x8(
        scaling_list8x8: &mut [[u8; 64]; 6],
        i: usize,
        default_scaling_list_intra: &[u8; 64],
        default_scaling_list_inter: &[u8; 64],
    ) {
        // See table 7.2 in the spec.
        scaling_list8x8[i] = match i {
            0 => *default_scaling_list_intra,
            1 => *default_scaling_list_inter,
            2 => scaling_list8x8[0],
            3 => scaling_list8x8[1],
            4 => scaling_list8x8[2],
            5 => scaling_list8x8[3],
            _ => panic!("Unexpected value {i}"),
        }
    }

    fn fill_scaling_list_flat(
        scaling_list4x4: &mut [[u8; 16]; 6],
        scaling_list8x8: &mut [[u8; 64]; 6],
    ) {
        // (7-8) in the spec.
        for outer in scaling_list4x4 {
            for inner in outer {
                *inner = 16;
            }
        }

        // (7-9) in the spec.
        for outer in scaling_list8x8 {
            for inner in outer {
                *inner = 16;
            }
        }
    }

    fn parse_scaling_list<U: AsMut<[u8]>>(
        r: &mut BitReader,
        scaling_list: &mut U,
        use_default: &mut bool,
    ) -> Result<(), String> {
        // 7.3.2.1.1.1
        let mut last_scale = 8u8;
        let mut next_scale = 8u8;

        for j in 0..scaling_list.as_mut().len() {
            if next_scale != 0 {
                let delta_scale = r.read_se::<i32>()?;
                // Fold to 0..256: `%` alone keeps a negative sign.
                next_scale =
                    u8::try_from((((i32::from(last_scale) + delta_scale + 256) % 256) + 256) % 256)
                        .map_err(|_| "scale range")?;
                *use_default = j == 0 && next_scale == 0;
                if *use_default {
                    return Ok(());
                }
            }

            scaling_list.as_mut()[j] = if next_scale == 0 {
                last_scale
            } else {
                next_scale
            };

            last_scale = scaling_list.as_mut()[j];
        }

        Ok(())
    }

    fn parse_sps_scaling_lists(r: &mut BitReader, sps: &mut Sps) -> Result<(), String> {
        let scaling_lists4x4 = &mut sps.scaling.lists_4x4;
        let scaling_lisst8x8 = &mut sps.scaling.lists_8x8;

        // Parse scaling_list4x4
        for i in 0..6 {
            let seq_scaling_list_present_flag = r.read_bit()?;
            if seq_scaling_list_present_flag {
                let mut use_default = false;

                Parser::parse_scaling_list(r, &mut scaling_lists4x4[i], &mut use_default)?;

                if use_default {
                    Parser::fill_default_scaling_list_4x4(&mut scaling_lists4x4[i], i);
                }
            } else {
                Parser::fill_fallback_scaling_list_4x4(
                    scaling_lists4x4,
                    i,
                    &DEFAULT_4X4_INTRA,
                    &DEFAULT_4X4_INTER,
                );
            }
        }

        // Parse scaling_list8x8
        let num_8x8 = if sps.chroma.idc == 3 { 6 } else { 2 };
        for i in 0..num_8x8 {
            let seq_scaling_list_present_flag = r.read_bit()?;
            if seq_scaling_list_present_flag {
                let mut use_default = false;
                Parser::parse_scaling_list(r, &mut scaling_lisst8x8[i], &mut use_default)?;

                if use_default {
                    Parser::fill_default_scaling_list_8x8(&mut scaling_lisst8x8[i], i);
                }
            } else {
                Parser::fill_fallback_scaling_list_8x8(
                    scaling_lisst8x8,
                    i,
                    &DEFAULT_8X8_INTRA,
                    &DEFAULT_8X8_INTER,
                );
            }
        }
        Ok(())
    }

    fn parse_pps_scaling_lists(r: &mut BitReader, pps: &mut Pps, sps: &Sps) -> Result<(), String> {
        let scaling_lists4x4 = &mut pps.scaling_lists_4x4;
        let scaling_lists8x8 = &mut pps.scaling_lists_8x8;

        for i in 0..6 {
            let pic_scaling_list_present_flag = r.read_bit()?;
            if pic_scaling_list_present_flag {
                let mut use_default = false;

                Parser::parse_scaling_list(r, &mut scaling_lists4x4[i], &mut use_default)?;

                if use_default {
                    Parser::fill_default_scaling_list_4x4(&mut scaling_lists4x4[i], i);
                }
            } else if !sps.scaling.present {
                // Table 7-2: Fallback rule A
                Parser::fill_fallback_scaling_list_4x4(
                    scaling_lists4x4,
                    i,
                    &DEFAULT_4X4_INTRA,
                    &DEFAULT_4X4_INTER,
                );
            } else {
                // Table 7-2: Fallback rule B
                Parser::fill_fallback_scaling_list_4x4(
                    scaling_lists4x4,
                    i,
                    &sps.scaling.lists_4x4[0],
                    &sps.scaling.lists_4x4[3],
                );
            }
        }

        if pps.coding.transform_8x8_mode_flag {
            let num8x8 = if sps.chroma.idc == 3 { 6 } else { 2 };

            for i in 0..num8x8 {
                let pic_scaling_list_present_flag = r.read_bit()?;
                if pic_scaling_list_present_flag {
                    let mut use_default = false;

                    Parser::parse_scaling_list(r, &mut scaling_lists8x8[i], &mut use_default)?;

                    if use_default {
                        Parser::fill_default_scaling_list_8x8(&mut scaling_lists8x8[i], i);
                    }
                } else if !sps.scaling.present {
                    // Table 7-2: Fallback rule A
                    Parser::fill_fallback_scaling_list_8x8(
                        scaling_lists8x8,
                        i,
                        &DEFAULT_8X8_INTRA,
                        &DEFAULT_8X8_INTER,
                    );
                } else {
                    // Table 7-2: Fallback rule B
                    Parser::fill_fallback_scaling_list_8x8(
                        scaling_lists8x8,
                        i,
                        &sps.scaling.lists_8x8[0],
                        &sps.scaling.lists_8x8[1],
                    );
                }
            }
        }

        Ok(())
    }

    fn parse_hrd(r: &mut BitReader, hrd: &mut HrdParams) -> Result<(), String> {
        hrd.cpb_cnt_minus1 = r.read_ue_max(31)?;
        hrd.bit_rate_scale = r.read_bits(4)?;
        hrd.cpb_size_scale = r.read_bits(4)?;

        for sched_sel_idx in 0..=usize::from(hrd.cpb_cnt_minus1) {
            hrd.bit_rate_value_minus1[sched_sel_idx] = r.read_ue()?;
            hrd.cpb_size_value_minus1[sched_sel_idx] = r.read_ue()?;
            hrd.cbr_flag[sched_sel_idx] = r.read_bit()?;
        }

        hrd.initial_cpb_removal_delay_length_minus1 = r.read_bits(5)?;
        hrd.cpb_removal_delay_length_minus1 = r.read_bits(5)?;
        hrd.dpb_output_delay_length_minus1 = r.read_bits(5)?;
        hrd.time_offset_length = r.read_bits(5)?;
        Ok(())
    }

    fn parse_vui(r: &mut BitReader, sps: &mut Sps) -> Result<(), String> {
        let vui = sps.vui.as_mut().expect("vui allocated by caller");

        if r.read_bit()? {
            let idc = r.read_bits(8)?;
            let (sar_width, sar_height) = if idc == 255 {
                (r.read_bits(16)?, r.read_bits(16)?)
            } else {
                (0, 0)
            };
            vui.aspect_ratio = Some(AspectRatio {
                idc,
                sar_width,
                sar_height,
            });
        }

        if r.read_bit()? {
            vui.overscan_appropriate = Some(r.read_bit()?);
        }

        if r.read_bit()? {
            let format = r.read_bits(3)?;
            let full_range = r.read_bit()?;
            let colour = if r.read_bit()? {
                Some(ColourDescription {
                    primaries: r.read_bits(8)?,
                    transfer: r.read_bits(8)?,
                    matrix: r.read_bits(8)?,
                })
            } else {
                None
            };
            vui.video_signal = Some(VideoSignalType {
                format,
                full_range,
                colour,
            });
        }

        if r.read_bit()? {
            vui.chroma_loc = Some(ChromaLoc {
                top: r.read_ue_max(5)?,
                bottom: r.read_ue_max(5)?,
            });
        }

        if r.read_bit()? {
            let mut num_units_in_tick = r.read_bits::<u32>(31)? << 1;
            num_units_in_tick |= u32::from(r.read_bit()?);
            if num_units_in_tick == 0 {
                return Err("num_units_in_tick == 0, which is not allowed by E.2.1".into());
            }

            let mut time_scale = r.read_bits::<u32>(31)? << 1;
            time_scale |= u32::from(r.read_bit()?);
            if time_scale == 0 {
                return Err("time_scale == 0, which is not allowed by E.2.1".into());
            }

            vui.timing = Some(TimingInfo {
                num_units_in_tick,
                time_scale,
                fixed_frame_rate: r.read_bit()?,
            });
        }

        let nal_present = r.read_bit()?;
        if nal_present {
            let mut parameters = HrdParams::default();
            Parser::parse_hrd(r, &mut parameters)?;
            vui.nal_hrd = Some(parameters);
        }

        let vcl_present = r.read_bit()?;
        if vcl_present {
            let mut parameters = HrdParams::default();
            Parser::parse_hrd(r, &mut parameters)?;
            vui.vcl_hrd = Some(parameters);
        }

        if nal_present || vcl_present {
            vui.low_delay_hrd_flag = r.read_bit()?;
        }

        vui.pic_struct_present_flag = r.read_bit()?;

        if r.read_bit()? {
            vui.restriction = Some(BitstreamRestriction {
                motion_vectors_over_pic_boundaries: r.read_bit()?,
                max_bytes_per_pic_denom: r.read_ue()?,
                max_bits_per_mb_denom: r.read_ue_max(16)?,
                log2_max_mv_length_horizontal: r.read_ue_max(16)?,
                log2_max_mv_length_vertical: r.read_ue_max(16)?,
                max_num_reorder_frames: r.read_ue()?,
                max_dec_frame_buffering: r.read_ue()?,
            });
        }

        Ok(())
    }

    fn parse_sps_high_profile(r: &mut BitReader, sps: &mut Sps) -> Result<(), String> {
        sps.chroma.idc = r.read_ue_max(3)?;
        if sps.chroma.idc == 3 {
            sps.chroma.separate_plane = r.read_bit()?;
        }

        sps.bit_depth.luma_minus8 = r.read_ue_max(6)?;
        sps.bit_depth.chroma_minus8 = r.read_ue_max(6)?;
        sps.bit_depth.transform_bypass = r.read_bit()?;
        sps.scaling.present = r.read_bit()?;

        if sps.scaling.present {
            Parser::parse_sps_scaling_lists(r, sps)?;
        } else {
            Parser::fill_scaling_list_flat(&mut sps.scaling.lists_4x4, &mut sps.scaling.lists_8x8);
        }

        Ok(())
    }

    fn parse_sps_poc(r: &mut BitReader, sps: &mut Sps) -> Result<(), String> {
        sps.pic_order_cnt_type = r.read_ue_max(2)?;

        if sps.pic_order_cnt_type == 0 {
            sps.log2_max_pic_order_cnt_lsb_minus4 = r.read_ue_max(12)?;
            sps.expected_delta_per_pic_order_cnt_cycle = 0;
        } else if sps.pic_order_cnt_type == 1 {
            sps.delta_pic_order_always_zero_flag = r.read_bit()?;
            sps.offset_for_non_ref_pic = r.read_se()?;
            sps.offset_for_top_to_bottom_field = r.read_se()?;
            sps.num_ref_frames_in_pic_order_cnt_cycle = r.read_ue_max(254)?;

            let mut offset_acc = 0;
            for i in 0..usize::from(sps.num_ref_frames_in_pic_order_cnt_cycle) {
                sps.offset_for_ref_frame[i] = r.read_se()?;

                // (7-12) in the spec.
                offset_acc += sps.offset_for_ref_frame[i];
            }

            sps.expected_delta_per_pic_order_cnt_cycle = offset_acc;
        }

        Ok(())
    }

    /// Parse a SPS and add it to the list of active SPSes.
    ///
    /// Returns a reference to the new SPS.
    pub fn parse_sps(&mut self, nalu: &Nalu) -> Result<&Rc<Sps>, String> {
        if !matches!(nalu.header.type_, NaluType::Sps) {
            return Err(format!(
                "Invalid NALU type, expected {:?}, got {:?}",
                NaluType::Sps,
                nalu.header.type_
            ));
        }

        let data = nalu.as_ref();
        // Skip the header
        let mut r = BitReader::new(&data[nalu.header.len()..], true);
        let mut sps = Sps {
            profile_idc: r.read_bits(8)?,
            constraints: [
                r.read_bit()?,
                r.read_bit()?,
                r.read_bit()?,
                r.read_bit()?,
                r.read_bit()?,
                r.read_bit()?,
            ],
            ..Default::default()
        };

        // skip reserved_zero_2bits
        r.skip_bits(2)?;

        let level: u8 = r.read_bits(8)?;
        sps.level_idc = Level::try_from(level)?;
        sps.seq_parameter_set_id = r.read_ue_max(31)?;

        if sps.profile_idc == 100
            || sps.profile_idc == 110
            || sps.profile_idc == 122
            || sps.profile_idc == 244
            || sps.profile_idc == 44
            || sps.profile_idc == 83
            || sps.profile_idc == 86
            || sps.profile_idc == 118
            || sps.profile_idc == 128
            || sps.profile_idc == 138
            || sps.profile_idc == 139
            || sps.profile_idc == 134
            || sps.profile_idc == 135
        {
            Parser::parse_sps_high_profile(&mut r, &mut sps)?;
        } else {
            sps.chroma.idc = 1;
            Parser::fill_scaling_list_flat(&mut sps.scaling.lists_4x4, &mut sps.scaling.lists_8x8);
        }

        sps.log2_max_frame_num_minus4 = r.read_ue_max(12)?;

        Parser::parse_sps_poc(&mut r, &mut sps)?;

        sps.max_num_ref_frames = r.read_ue_max(DPB_MAX_SIZE)?;
        sps.gaps_in_frame_num_value_allowed_flag = r.read_bit()?;
        sps.pic_width_in_mbs_minus1 = r.read_ue()?;
        sps.pic_height_in_map_units_minus1 = r.read_ue()?;
        sps.frame_field.frame_mbs_only = r.read_bit()?;

        if !sps.frame_field.frame_mbs_only {
            sps.frame_field.mb_adaptive = r.read_bit()?;
        }

        sps.direct_8x8_inference_flag = r.read_bit()?;

        if r.read_bit()? {
            let crop = FrameCrop {
                left: r.read_ue()?,
                right: r.read_ue()?,
                top: r.read_ue()?,
                bottom: r.read_ue()?,
            };
            sps.crop = Some(crop);

            // Validate that cropping info is valid.
            let (crop_unit_x, crop_unit_y) = sps.crop_unit_x_y();
            let crop = sps.crop.expect("crop just parsed");

            let _ = crop
                .left
                .checked_add(crop.right)
                .and_then(|r| r.checked_mul(crop_unit_x))
                .and_then(|r| sps.width().checked_sub(r))
                .ok_or::<String>("Invalid frame crop width".into())?;

            let _ = crop
                .top
                .checked_add(crop.bottom)
                .and_then(|r| r.checked_mul(crop_unit_y))
                .and_then(|r| sps.height().checked_sub(r))
                .ok_or::<String>("invalid frame crop height".into())?;
        }

        if r.read_bit()? {
            sps.vui = Some(VuiParams::default());
            Parser::parse_vui(&mut r, &mut sps)?;
        }

        let key = sps.seq_parameter_set_id;

        if self.active_spses.keys().len() >= MAX_SPS_COUNT as usize {
            return Err("Broken data: Number of active SPSs > MAX_SPS_COUNT".into());
        }

        let sps = Rc::new(sps);
        self.active_spses.remove(&key);
        Ok(self.active_spses.entry(key).or_insert(sps))
    }

    pub fn parse_pps(&mut self, nalu: &Nalu) -> Result<&Pps, String> {
        if !matches!(nalu.header.type_, NaluType::Pps) {
            return Err(format!(
                "Invalid NALU type, expected {:?}, got {:?}",
                NaluType::Pps,
                nalu.header.type_
            ));
        }

        let data = nalu.as_ref();
        // Skip the header
        let mut r = BitReader::new(&data[nalu.header.len()..], true);
        let pic_parameter_set_id = r.read_ue_max(u32::from(MAX_PPS_COUNT) - 1)?;
        let seq_parameter_set_id = r.read_ue_max(u32::from(MAX_SPS_COUNT) - 1)?;
        let sps = self.get_sps(seq_parameter_set_id).ok_or::<String>(format!(
            "Could not get SPS for seq_parameter_set_id {seq_parameter_set_id}"
        ))?;
        let mut pps = Pps {
            pic_parameter_set_id,
            seq_parameter_set_id,
            sps: Rc::clone(sps),
            scaling_lists_4x4: [[0; 16]; 6],
            scaling_lists_8x8: [[0; 64]; 6],
            coding: PpsCodingFlags::default(),
            prediction: PpsPredictionFlags::default(),
            presence: PpsPresenceFlags::default(),
            num_slice_groups_minus1: Default::default(),
            num_ref_idx_l0_default_active_minus1: Default::default(),
            num_ref_idx_l1_default_active_minus1: Default::default(),
            weighted_bipred_idc: Default::default(),
            pic_init_qp_minus26: Default::default(),
            pic_init_qs_minus26: Default::default(),
            chroma_qp_index_offset: Default::default(),
            second_chroma_qp_index_offset: Default::default(),
            pic_scaling_matrix_present_flag: Default::default(),
        };

        pps.coding.entropy_coding_mode_flag = r.read_bit()?;
        pps.presence.bottom_field_pic_order_in_frame_present_flag = r.read_bit()?;
        pps.num_slice_groups_minus1 = r.read_ue_max(7)?;

        if pps.num_slice_groups_minus1 > 0 {
            return Err("Stream contain unsupported/unimplemented NALs".into());
        }

        pps.num_ref_idx_l0_default_active_minus1 = r.read_ue_max(31)?;
        pps.num_ref_idx_l1_default_active_minus1 = r.read_ue_max(31)?;

        pps.prediction.weighted_pred_flag = r.read_bit()?;
        pps.weighted_bipred_idc = r.read_bits(2)?;

        let qp_bd_offset_y = i32::from(6 * (sps.bit_depth.luma_minus8));
        pps.pic_init_qp_minus26 = r.read_se_bounded(-(26 + qp_bd_offset_y), 25)?;
        pps.pic_init_qs_minus26 = r.read_se_bounded(-26, 25)?;

        pps.chroma_qp_index_offset = r.read_se_bounded(-12, 12)?;

        // When second_chroma_qp_index_offset is not present, it shall be
        // inferred to be equal to chroma_qp_index_offset.
        pps.second_chroma_qp_index_offset = pps.chroma_qp_index_offset;

        pps.presence.deblocking_filter_control_present_flag = r.read_bit()?;
        pps.prediction.constrained_intra_pred_flag = r.read_bit()?;
        pps.presence.redundant_pic_cnt_present_flag = r.read_bit()?;

        if r.has_more_rsbp_data() {
            pps.coding.transform_8x8_mode_flag = r.read_bit()?;
            pps.pic_scaling_matrix_present_flag = r.read_bit()?;

            if pps.pic_scaling_matrix_present_flag {
                Parser::parse_pps_scaling_lists(&mut r, &mut pps, sps)?;
            }

            pps.second_chroma_qp_index_offset = r.read_se()?;
        }

        if !pps.pic_scaling_matrix_present_flag {
            // If not set, specifies that the scaling lists used for the picture
            // shall be inferred to be equal to those specified by the sequence
            // parameter set. When pic_scaling_matrix_present_flag is not
            // present, it shall be inferred to be not set.
            pps.scaling_lists_4x4 = sps.scaling.lists_4x4;
            pps.scaling_lists_8x8 = sps.scaling.lists_8x8;
        }

        let key = pps.pic_parameter_set_id;

        if self.active_ppses.keys().len() >= MAX_PPS_COUNT as usize {
            return Err("Broken Data: number of active PPSs > MAX_PPS_COUNT".into());
        }

        let pps = Rc::new(pps);
        self.active_ppses.remove(&key);
        Ok(self.active_ppses.entry(key).or_insert(pps))
    }

    fn parse_ref_pic_list_modification(
        r: &mut BitReader,
        num_ref_idx_active_minus1: u8,
        ref_list_mods: &mut Vec<RefPicListModification>,
    ) -> Result<(), String> {
        if num_ref_idx_active_minus1 >= 32 {
            return Err("Broken Data: num_ref_idx_active_minus1 >= 32".into());
        }

        loop {
            let mut pic_num_mod = RefPicListModification {
                modification_of_pic_nums_idc: r.read_ue_max(3)?,
                ..Default::default()
            };

            match pic_num_mod.modification_of_pic_nums_idc {
                0 | 1 => {
                    pic_num_mod.abs_diff_pic_num_minus1 = r.read_ue()?;
                }

                2 => {
                    pic_num_mod.long_term_pic_num = r.read_ue()?;
                }

                3 => {
                    ref_list_mods.push(pic_num_mod);
                    break;
                }

                _ => return Err("Broken Data: modification_of_pic_nums_idc > 3".into()),
            }

            ref_list_mods.push(pic_num_mod);
        }

        Ok(())
    }

    fn parse_ref_pic_list_modifications(
        r: &mut BitReader,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        if !header.slice_type.is_i() && !header.slice_type.is_si() {
            header.list_modification.l0 = r.read_bit()?;
            if header.list_modification.l0 {
                Parser::parse_ref_pic_list_modification(
                    r,
                    header.num_ref_idx_l0_active_minus1,
                    &mut header.ref_pic_list_modification_l0,
                )?;
            }
        }

        if header.slice_type.is_b() {
            header.list_modification.l1 = r.read_bit()?;
            if header.list_modification.l1 {
                Parser::parse_ref_pic_list_modification(
                    r,
                    header.num_ref_idx_l1_active_minus1,
                    &mut header.ref_pic_list_modification_l1,
                )?;
            }
        }

        Ok(())
    }

    fn parse_pred_weight_table(
        r: &mut BitReader,
        sps: &Sps,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        let pt = &mut header.pred_weight_table;
        pt.luma_log2_weight_denom = r.read_ue_max(7)?;

        // When luma_weight_l0_flag is equal to 0, luma_weight_l0[i] shall be
        // inferred to be equal to 2 ^ luma_log2_weight_denom for
        // RefPicList0[i].
        let default_luma_weight = 1 << pt.luma_log2_weight_denom;
        for i in 0..=header.num_ref_idx_l0_active_minus1 {
            pt.luma_weight_l0[usize::from(i)] = default_luma_weight;
        }

        // When luma_weight_l1_flag is equal to 1, luma_weight_l1[i] shall be
        // inferred to be equal to 2 ^ luma_log2_weight_denom for
        // RefPicList1[i].
        if header.slice_type.is_b() {
            for i in 0..=header.num_ref_idx_l1_active_minus1 {
                pt.luma_weight_l1[usize::from(i)] = default_luma_weight;
            }
        }

        if sps.chroma_array_type() != 0 {
            pt.chroma_log2_weight_denom = r.read_ue_max(7)?;
            let default_chroma_weight = 1 << pt.chroma_log2_weight_denom;

            // When chroma_weight_l0_flag is equal to 0, chroma_weight_l0[i]
            // shall be inferred to be equal to 2 ^ chroma_log2_weight_denom for
            // RefPicList0[i].
            for i in 0..=header.num_ref_idx_l0_active_minus1 {
                pt.chroma_weight_l0[usize::from(i)][0] = default_chroma_weight;
                pt.chroma_weight_l0[usize::from(i)][1] = default_chroma_weight;
            }

            // When chroma_weight_l1_flag is equal to 0, chroma_weight_l1[i]
            // shall be inferred to be equal to 2 ^ chroma_log2_weight_denom for
            // RefPicList1[i].
            for i in 0..=header.num_ref_idx_l1_active_minus1 {
                pt.chroma_weight_l1[usize::from(i)][0] = default_chroma_weight;
                pt.chroma_weight_l1[usize::from(i)][1] = default_chroma_weight;
            }
        }

        for i in 0..=header.num_ref_idx_l0_active_minus1 {
            let luma_weight_l0_flag = r.read_bit()?;

            if luma_weight_l0_flag {
                pt.luma_weight_l0[usize::from(i)] = r.read_se_bounded(-128, 127)?;
                pt.luma_offset_l0[usize::from(i)] = r.read_se_bounded(-128, 127)?;
            }

            if sps.chroma_array_type() != 0 {
                let chroma_weight_l0_flag = r.read_bit()?;
                if chroma_weight_l0_flag {
                    for j in 0..2 {
                        pt.chroma_weight_l0[usize::from(i)][j] = r.read_se_bounded(-128, 127)?;
                        pt.chroma_offset_l0[usize::from(i)][j] = r.read_se_bounded(-128, 127)?;
                    }
                }
            }
        }

        if header.slice_type.is_b() {
            for i in 0..=header.num_ref_idx_l1_active_minus1 {
                let luma_weight_l1_flag = r.read_bit()?;

                if luma_weight_l1_flag {
                    pt.luma_weight_l1[usize::from(i)] = r.read_se_bounded(-128, 127)?;
                    pt.luma_offset_l1[usize::from(i)] = r.read_se_bounded(-128, 127)?;
                }

                if sps.chroma_array_type() != 0 {
                    let chroma_weight_l1_flag = r.read_bit()?;
                    if chroma_weight_l1_flag {
                        for j in 0..2 {
                            pt.chroma_weight_l1[usize::from(i)][j] =
                                r.read_se_bounded(-128, 127)?;
                            pt.chroma_offset_l1[usize::from(i)][j] =
                                r.read_se_bounded(-128, 127)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn parse_dec_ref_pic_marking(
        r: &mut BitReader,
        nalu: &Nalu,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        let rpm = &mut header.dec_ref_pic_marking;

        let num_bits_left = r.num_bits_left();
        if nalu.header.idr_pic_flag {
            rpm.no_output_of_prior_pics_flag = r.read_bit()?;
            rpm.long_term_reference_flag = r.read_bit()?;
        } else {
            rpm.adaptive_ref_pic_marking_mode_flag = r.read_bit()?;

            if rpm.adaptive_ref_pic_marking_mode_flag {
                loop {
                    let mut marking = RefPicMarkingInner::default();

                    let mem_mgmt_ctrl_op = r.read_ue_max::<u8>(6)?;
                    marking.memory_management_control_operation = mem_mgmt_ctrl_op;

                    if mem_mgmt_ctrl_op == 0 {
                        break;
                    }

                    if mem_mgmt_ctrl_op == 1 || mem_mgmt_ctrl_op == 3 {
                        marking.difference_of_pic_nums_minus1 = r.read_ue()?;
                    }

                    if mem_mgmt_ctrl_op == 2 {
                        marking.long_term_pic_num = r.read_ue()?;
                    }

                    if mem_mgmt_ctrl_op == 3 || mem_mgmt_ctrl_op == 6 {
                        marking.long_term_frame_idx = r.read_ue()?;
                    }

                    if mem_mgmt_ctrl_op == 4 {
                        marking.max_long_term_frame_idx =
                            MaxLongTermFrameIdx::from_value_plus1(r.read_ue()?);
                    }

                    rpm.inner.push(marking);
                }
            }
        }
        header.dec_ref_pic_marking_bit_size = num_bits_left - r.num_bits_left();

        Ok(())
    }

    fn parse_slice_poc(
        r: &mut BitReader,
        sps: &Sps,
        pps: &Pps,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        let num_bits_left = r.num_bits_left();
        let is_frame = header.field == Field::Frame;
        if sps.pic_order_cnt_type == 0 {
            header.pic_order_cnt_lsb =
                r.read_bits(usize::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4)?;

            if pps.presence.bottom_field_pic_order_in_frame_present_flag && is_frame {
                header.delta_pic_order_cnt_bottom = r.read_se()?;
            }
        }

        if sps.pic_order_cnt_type == 1 && !sps.delta_pic_order_always_zero_flag {
            header.delta_pic_order_cnt[0] = r.read_se()?;
            if pps.presence.bottom_field_pic_order_in_frame_present_flag && is_frame {
                header.delta_pic_order_cnt[1] = r.read_se()?;
            }
        }
        header.pic_order_cnt_bit_size = num_bits_left - r.num_bits_left();

        Ok(())
    }

    fn parse_slice_ref_idx(
        r: &mut BitReader,
        pps: &Pps,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        if !(header.slice_type.is_p() || header.slice_type.is_sp() || header.slice_type.is_b()) {
            return Ok(());
        }

        header.num_ref_idx_active_override_flag = r.read_bit()?;
        if header.num_ref_idx_active_override_flag {
            header.num_ref_idx_l0_active_minus1 = r.read_ue()?;
            if header.slice_type.is_b() {
                header.num_ref_idx_l1_active_minus1 = r.read_ue()?;
            }
        } else {
            header.num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
            if header.slice_type.is_b() {
                header.num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
            }
        }

        Ok(())
    }

    fn parse_slice_tail(
        r: &mut BitReader,
        sps: &Sps,
        pps: &Pps,
        nalu: &Nalu,
        header: &mut SliceHeader,
    ) -> Result<(), String> {
        Parser::parse_ref_pic_list_modifications(r, header)?;

        if (pps.prediction.weighted_pred_flag
            && (header.slice_type.is_p() || header.slice_type.is_sp()))
            || (pps.weighted_bipred_idc == 1 && header.slice_type.is_b())
        {
            Parser::parse_pred_weight_table(r, sps, header)?;
        }

        // H.264 (01/2012) 7.3.3 order: list modification, weight table,
        // marking, cabac init, qp delta.
        if nalu.header.ref_idc != 0 {
            Parser::parse_dec_ref_pic_marking(r, nalu, header)?;
        }

        if pps.coding.entropy_coding_mode_flag
            && !header.slice_type.is_i()
            && !header.slice_type.is_si()
        {
            header.cabac_init_idc = r.read_ue_max(2)?;
        }

        header.slice_qp_delta = r.read_se_bounded(-87, 77)?;

        if header.slice_type.is_sp() || header.slice_type.is_si() {
            if header.slice_type.is_sp() {
                header.sp_for_switch_flag = r.read_bit()?;
            }

            header.slice_qs_delta = r.read_se_bounded(-51, 51)?;
        }

        if pps.presence.deblocking_filter_control_present_flag {
            header.disable_deblocking_filter_idc = r.read_ue_max(2)?;

            if header.disable_deblocking_filter_idc != 1 {
                header.slice_alpha_c0_offset_div2 = r.read_se_bounded(-6, 6)?;
                header.slice_beta_offset_div2 = r.read_se_bounded(-6, 6)?;
            }
        }

        Ok(())
    }

    pub fn parse_slice_header<'a>(&self, nalu: Nalu<'a>) -> Result<Slice<'a>, String> {
        if !matches!(
            nalu.header.type_,
            NaluType::Slice
                | NaluType::SliceDpa
                | NaluType::SliceDpb
                | NaluType::SliceDpc
                | NaluType::SliceIdr
                | NaluType::SliceExt
        ) {
            return Err(format!(
                "Invalid NALU type: {:?} is not a slice NALU",
                nalu.header.type_
            ));
        }

        let data = nalu.as_ref();
        // Skip the header
        let mut r = BitReader::new(&data[nalu.header.len()..], true);

        let mut header = SliceHeader {
            first_mb_in_slice: r.read_ue()?,
            ..Default::default()
        };

        let slice_type = r.read_ue_max::<u8>(9)? % 5;
        header.slice_type = SliceType::try_from(slice_type)?;

        header.pic_parameter_set_id = r.read_ue()?;

        let pps = self
            .get_pps(header.pic_parameter_set_id)
            .ok_or::<String>(format!(
                "Could not get PPS for pic_parameter_set_id {}",
                header.pic_parameter_set_id
            ))?;

        let sps = &pps.sps;

        if sps.chroma.separate_plane {
            header.colour_plane_id = r.read_bits(2)?;
        }

        header.frame_num = r.read_bits(usize::from(sps.log2_max_frame_num_minus4) + 4)?;

        if !sps.frame_field.frame_mbs_only {
            let field_pic = r.read_bit()?;
            header.field = if field_pic && r.read_bit()? {
                Field::Bottom
            } else if field_pic {
                Field::Top
            } else {
                Field::Frame
            };
        }

        header.max_pic_num = if header.field == Field::Frame {
            sps.max_frame_num()
        } else {
            2 * sps.max_frame_num()
        };

        if nalu.header.idr_pic_flag {
            header.idr_pic_id = r.read_ue_max(0xffff)?;
        }

        Parser::parse_slice_poc(&mut r, sps, pps, &mut header)?;

        if pps.presence.redundant_pic_cnt_present_flag {
            header.redundant_pic_cnt = r.read_ue_max(127)?;
        }

        if header.slice_type.is_b() {
            header.direct_spatial_mv_pred_flag = r.read_bit()?;
        }

        Parser::parse_slice_ref_idx(&mut r, pps, &mut header)?;

        if header.field != Field::Frame {
            if header.num_ref_idx_l0_active_minus1 > 31 || header.num_ref_idx_l1_active_minus1 > 31
            {
                return Err("Broken Data".into());
            }
        } else if header.num_ref_idx_l0_active_minus1 > 15
            || header.num_ref_idx_l1_active_minus1 > 15
        {
            return Err("Broken Data".into());
        }

        if let NaluType::SliceExt = nalu.header.type_ {
            return Err("Stream contain unsupported/unimplemented NALs".into());
        }

        Parser::parse_slice_tail(&mut r, sps, pps, &nalu, &mut header)?;

        if pps.num_slice_groups_minus1 > 0 {
            return Err("Stream contain unsupported/unimplemented NALs".into());
        }

        let epb = r.num_epb();
        header.header_bit_size = (nalu.size - epb) * 8 - r.num_bits_left();

        header.n_emulation_prevention_bytes = epb;

        Ok(Slice { header, nalu })
    }

    pub fn get_sps(&self, sps_id: u8) -> Option<&Rc<Sps>> {
        self.active_spses.get(&sps_id)
    }

    pub fn get_pps(&self, pps_id: u8) -> Option<&Rc<Pps>> {
        self.active_ppses.get(&pps_id)
    }
}

#[derive(Debug)]
pub struct NaluHeader {
    pub ref_idc: u8,
    pub type_: NaluType,
    pub idr_pic_flag: bool,
}

impl Header for NaluHeader {
    fn parse<T: AsRef<[u8]>>(cursor: &mut Cursor<T>) -> Result<Self, String> {
        let mut byte_buf = [0u8; 1];
        cursor
            .read_exact(&mut byte_buf)
            .map_err(|_| String::from("Broken Data"))?;
        let byte = byte_buf[0];
        let _ = cursor.seek(SeekFrom::Current(
            -(i64::try_from(byte_buf.len()).map_err(|_| "rewind range")?),
        ));

        let type_ = NaluType::try_from(byte & 0x1f)?;

        if let NaluType::SliceExt = type_ {
            return Err("Stream contain unsupported/unimplemented NALs".into());
        }

        let ref_idc = (byte & 0x60) >> 5;
        let idr_pic_flag = matches!(type_, NaluType::SliceIdr);

        Ok(NaluHeader {
            ref_idc,
            type_,
            idr_pic_flag,
        })
    }

    fn is_end(&self) -> bool {
        matches!(self.type_, NaluType::SeqEnd | NaluType::StreamEnd)
    }

    fn len(&self) -> usize {
        1
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{Nalu, Parser};

    /// Annex B buffer: start code, `header`, `payload`.
    fn annex_b(header: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0, 0, 0, 1, header];
        bytes.extend_from_slice(payload);
        bytes
    }

    /// First NAL unit of `bytes`.
    fn first(bytes: &[u8]) -> Nalu<'_> {
        let mut cursor = Cursor::new(bytes);
        Nalu::next(&mut cursor).expect("annex b unit")
    }

    #[test]
    fn start_code_missing_rejected() {
        let bytes: &[u8] = &[0xFF; 8];
        let mut cursor = Cursor::new(bytes);
        assert!(Nalu::next(&mut cursor).is_err());
    }

    #[test]
    fn empty_sps_payload_rejected() {
        let bytes = annex_b(0x67, &[]);
        let nalu = first(&bytes);
        let mut parser = Parser::default();
        assert_eq!(
            parser.parse_sps(&nalu).map(|_| ()),
            Err("reader ran out of bits".to_owned())
        );
    }

    #[test]
    fn truncated_sps_payload_rejected() {
        let bytes = annex_b(0x67, &[0x42]);
        let nalu = first(&bytes);
        let mut parser = Parser::default();
        assert!(parser.parse_sps(&nalu).is_err());
    }

    #[test]
    fn garbage_sps_payload_rejected() {
        let bytes = annex_b(0x67, &[0xFF; 6]);
        let nalu = first(&bytes);
        let mut parser = Parser::default();
        assert!(parser.parse_sps(&nalu).is_err());
    }

    #[test]
    fn empty_pps_payload_rejected() {
        let bytes = annex_b(0x68, &[]);
        let nalu = first(&bytes);
        let mut parser = Parser::default();
        assert!(parser.parse_pps(&nalu).is_err());
    }

    #[test]
    fn garbage_slice_payload_rejected() {
        let bytes = annex_b(0x65, &[0xFF; 4]);
        let nalu = first(&bytes);
        let parser = Parser::default();
        assert!(parser.parse_slice_header(nalu).is_err());
    }
}
