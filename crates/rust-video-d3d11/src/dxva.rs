//! DXVA picture structures behind D3D11 video decode.
//!
//! Field order matches dxva.h in the Windows SDK.

use std::mem::{offset_of, size_of};

/// H.264 picture entry: surface index plus one flag bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicEntryH264 {
    /// Index in bits 0-6, flag in bit 7.
    pub(crate) entry: u8,
}

impl PicEntryH264 {
    /// Entry naming no surface.
    pub(crate) const INVALID: Self = Self { entry: 0xFF };

    /// Entry for surface `index` with `flag` set.
    #[must_use]
    pub(crate) const fn new(index: u8, flag: bool) -> Self {
        Self {
            entry: (index & 0x7F) | ((flag as u8) << 7),
        }
    }

    /// Entry for surface `index` without the flag.
    #[must_use]
    pub(crate) const fn frame(index: u8) -> Self {
        Self::new(index, false)
    }
}

/// Chroma 4:2:0 in the picture bit fields.
pub(crate) const BITS_CHROMA_420: u16 = 1 << 4;
/// Current picture used for reference.
pub(crate) const BITS_REF_PIC: u16 = 1 << 6;
/// Constrained intra prediction.
pub(crate) const BITS_CONSTRAINED_INTRA: u16 = 1 << 7;
/// Weighted prediction.
pub(crate) const BITS_WEIGHTED_PRED: u16 = 1 << 8;
/// Weighted biprediction mode shift.
pub(crate) const BITS_WEIGHTED_BIPRED_SHIFT: u16 = 9;
/// Macroblocks consecutive in raster order.
pub(crate) const BITS_MBS_CONSECUTIVE: u16 = 1 << 11;
/// Frame-only coded sequence.
pub(crate) const BITS_FRAME_MBS_ONLY: u16 = 1 << 12;
/// 8x8 transform mode.
pub(crate) const BITS_TRANSFORM_8X8: u16 = 1 << 13;
/// Minimum luma biprediction size 8x8.
pub(crate) const BITS_MIN_LUMA_BIPRED_8X8: u16 = 1 << 14;
/// Intra-only picture.
pub(crate) const BITS_INTRA_PIC: u16 = 1 << 15;

/// H.264 picture parameters, 1040 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsH264 {
    /// Coded width in macroblocks minus one.
    pub(crate) frame_width_in_mbs_minus1: u16,
    /// Coded height in macroblocks minus one.
    pub(crate) frame_height_in_mbs_minus1: u16,
    /// Current surface; flag is the bottom-field flag.
    pub(crate) curr_pic: PicEntryH264,
    /// Maximum reference frame count.
    pub(crate) num_ref_frames: u8,
    /// Packed picture flags.
    pub(crate) bit_fields: u16,
    /// Luma bit depth minus 8.
    pub(crate) bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    pub(crate) bit_depth_chroma_minus8: u8,
    /// Reserved.
    pub(crate) reserved_16_a: u16,
    /// Driver-echoed picture serial.
    pub(crate) status_report_feedback_number: u32,
    /// Reference surfaces; flag is the long-term flag.
    pub(crate) ref_frame_list: [PicEntryH264; 16],
    /// Current picture order counts.
    pub(crate) curr_field_order_cnt: [i32; 2],
    /// Reference picture order counts.
    pub(crate) field_order_cnt_list: [[i32; 2]; 16],
    /// SP/SI quantizer init delta.
    pub(crate) pic_init_qs_minus26: i8,
    /// Chroma QP offset.
    pub(crate) chroma_qp_index_offset: i8,
    /// Second chroma QP offset.
    pub(crate) second_chroma_qp_index_offset: i8,
    /// Continuation flag.
    pub(crate) continuation_flag: u8,
    /// Luma QP init delta.
    pub(crate) pic_init_qp_minus26: i8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_idx_l0_active_minus1: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_idx_l1_active_minus1: u8,
    /// Reserved.
    pub(crate) reserved_8_a: u8,
    /// Reference frame numbers.
    pub(crate) frame_num_list: [u16; 16],
    /// Bit per reference surface.
    pub(crate) used_for_reference_flags: u32,
    /// Bit per nonexisting frame.
    pub(crate) non_existing_frame_flags: u16,
    /// Current frame number.
    pub(crate) frame_num: u16,
    /// Frame number width minus 4.
    pub(crate) log2_max_frame_num_minus4: u8,
    /// Picture order count type.
    pub(crate) pic_order_cnt_type: u8,
    /// POC LSB width minus 4.
    pub(crate) log2_max_pic_order_cnt_lsb_minus4: u8,
    /// POC deltas always zero.
    pub(crate) delta_pic_order_always_zero_flag: u8,
    /// Direct 8x8 inference.
    pub(crate) direct_8x8_inference_flag: u8,
    /// CABAC entropy coding.
    pub(crate) entropy_coding_mode_flag: u8,
    /// POC delta present in slices.
    pub(crate) pic_order_present_flag: u8,
    /// Slice group count minus one.
    pub(crate) num_slice_groups_minus1: u8,
    /// Slice group map type.
    pub(crate) slice_group_map_type: u8,
    /// Deblocking filter control present.
    pub(crate) deblocking_filter_control_present_flag: u8,
    /// Redundant pictures present.
    pub(crate) redundant_pic_cnt_present_flag: u8,
    /// Reserved.
    pub(crate) reserved_8_b: u8,
    /// Slice group change rate minus one.
    pub(crate) slice_group_change_rate_minus1: u16,
    /// Slice group map.
    pub(crate) slice_group_map: [u8; 810],
}

impl PicParamsH264 {
    /// Parameters with cleared references and map.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            frame_width_in_mbs_minus1: 0,
            frame_height_in_mbs_minus1: 0,
            curr_pic: PicEntryH264::INVALID,
            num_ref_frames: 0,
            bit_fields: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            reserved_16_a: 0,
            status_report_feedback_number: 0,
            ref_frame_list: [PicEntryH264::INVALID; 16],
            curr_field_order_cnt: [0; 2],
            field_order_cnt_list: [[0; 2]; 16],
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            continuation_flag: 0,
            pic_init_qp_minus26: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            reserved_8_a: 0,
            frame_num_list: [0; 16],
            used_for_reference_flags: 0,
            non_existing_frame_flags: 0,
            frame_num: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            delta_pic_order_always_zero_flag: 0,
            direct_8x8_inference_flag: 0,
            entropy_coding_mode_flag: 0,
            pic_order_present_flag: 0,
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            deblocking_filter_control_present_flag: 0,
            redundant_pic_cnt_present_flag: 0,
            reserved_8_b: 0,
            slice_group_change_rate_minus1: 0,
            slice_group_map: [0; 810],
        }
    }
}

/// H.264 inverse quantization matrices, 224 bytes in raster order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QmatrixH264 {
    /// Six 4x4 scaling lists.
    pub(crate) lists_4x4: [[u8; 16]; 6],
    /// Two 8x8 scaling lists.
    pub(crate) lists_8x8: [[u8; 64]; 2],
}

/// H.264 long slice control entry, 864 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SliceLongH264 {
    /// NAL offset in the bitstream buffer.
    pub(crate) location: u32,
    /// NAL length in bytes.
    pub(crate) bytes: u32,
    /// Bad slice chopping flag.
    pub(crate) chopping: u16,
    /// First macroblock address.
    pub(crate) first_mb: u16,
    /// Macroblocks in the slice.
    pub(crate) mbs_in_slice: u16,
    /// Bit offset to slice data.
    pub(crate) bit_offset: u16,
    /// Slice type number.
    pub(crate) slice_type: u8,
    /// Luma weight denominator.
    pub(crate) luma_denom: u8,
    /// Chroma weight denominator.
    pub(crate) chroma_denom: u8,
    /// Active L0 reference count minus one.
    pub(crate) num_ref_l0: u8,
    /// Active L1 reference count minus one.
    pub(crate) num_ref_l1: u8,
    /// Deblocking alpha offset.
    pub(crate) alpha_offset: i8,
    /// Deblocking beta offset.
    pub(crate) beta_offset: i8,
    /// Reserved.
    pub(crate) reserved_8: u8,
    /// Reference surfaces by list and index.
    pub(crate) ref_lists: [[PicEntryH264; 32]; 2],
    /// Weights by list, index, plane, and weight-offset pair.
    pub(crate) weights: [[[[i16; 2]; 3]; 32]; 2],
    /// SP/SI quantizer delta.
    pub(crate) qs_delta: i8,
    /// Slice QP delta.
    pub(crate) qp_delta: i8,
    /// Redundant picture count.
    pub(crate) redundant_cnt: u8,
    /// Spatial direct prediction flag.
    pub(crate) direct_spatial: u8,
    /// CABAC init idc.
    pub(crate) cabac_init_idc: u8,
    /// Deblocking filter idc.
    pub(crate) deblocking_idc: u8,
    /// Slice index in the picture.
    pub(crate) slice_id: u16,
}

/// HEVC picture entry: surface index plus one flag bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicEntryHevc {
    /// Index in bits 0-6, flag in bit 7.
    pub(crate) entry: u8,
}

impl PicEntryHevc {
    /// Entry naming no surface.
    pub(crate) const INVALID: Self = Self { entry: 0xFF };

    /// Entry for surface `index` with `flag` set.
    #[must_use]
    pub(crate) const fn new(index: u8, flag: bool) -> Self {
        Self {
            entry: (index & 0x7F) | ((flag as u8) << 7),
        }
    }

    /// Entry for surface `index` without the flag.
    #[must_use]
    pub(crate) const fn frame(index: u8) -> Self {
        Self::new(index, false)
    }
}

/// HEVC format flags: chroma 4:2:0 in bits 0-1.
pub(crate) const HEVC_FORMAT_CHROMA_420: u16 = 1;
/// HEVC format flags: POC LSB width shift.
pub(crate) const HEVC_FORMAT_POC_SHIFT: u16 = 9;

/// HEVC tool flags: scaling lists enabled.
pub(crate) const HEVC_TOOL_SCALING: u32 = 1;
/// HEVC tool flags: asymmetric motion partitions.
pub(crate) const HEVC_TOOL_AMP: u32 = 1 << 1;
/// HEVC tool flags: sample adaptive offset.
pub(crate) const HEVC_TOOL_SAO: u32 = 1 << 2;
/// HEVC tool flags: pulse code modulation.
pub(crate) const HEVC_TOOL_PCM: u32 = 1 << 3;
/// HEVC tool flags: PCM luma depth shift.
pub(crate) const HEVC_TOOL_PCM_LUMA_SHIFT: u32 = 4;
/// HEVC tool flags: PCM chroma depth shift.
pub(crate) const HEVC_TOOL_PCM_CHROMA_SHIFT: u32 = 8;
/// HEVC tool flags: PCM minimum block shift.
pub(crate) const HEVC_TOOL_PCM_MIN_SHIFT: u32 = 12;
/// HEVC tool flags: PCM block range shift.
pub(crate) const HEVC_TOOL_PCM_DIFF_SHIFT: u32 = 14;
/// HEVC tool flags: PCM loop filter disabled.
pub(crate) const HEVC_TOOL_PCM_LOOP_DISABLED: u32 = 1 << 16;
/// HEVC tool flags: long-term pictures present.
pub(crate) const HEVC_TOOL_LONG_TERM: u32 = 1 << 17;
/// HEVC tool flags: temporal motion vectors.
pub(crate) const HEVC_TOOL_TEMPORAL_MVP: u32 = 1 << 18;
/// HEVC tool flags: strong intra smoothing.
pub(crate) const HEVC_TOOL_SMOOTHING: u32 = 1 << 19;
/// HEVC tool flags: dependent slice segments.
pub(crate) const HEVC_TOOL_DEPENDENT_SLICES: u32 = 1 << 20;
/// HEVC tool flags: output flag present.
pub(crate) const HEVC_TOOL_OUTPUT_FLAG: u32 = 1 << 21;
/// HEVC tool flags: extra slice header bits shift.
pub(crate) const HEVC_TOOL_EXTRA_BITS_SHIFT: u32 = 22;
/// HEVC tool flags: sign data hiding.
pub(crate) const HEVC_TOOL_SIGN_HIDING: u32 = 1 << 25;
/// HEVC tool flags: CABAC init present.
pub(crate) const HEVC_TOOL_CABAC_INIT: u32 = 1 << 26;

/// HEVC picture flags: constrained intra prediction.
pub(crate) const HEVC_PIC_CONSTRAINED_INTRA: u32 = 1;
/// HEVC picture flags: transform skip.
pub(crate) const HEVC_PIC_TRANSFORM_SKIP: u32 = 1 << 1;
/// HEVC picture flags: CU QP deltas.
pub(crate) const HEVC_PIC_CU_QP_DELTA: u32 = 1 << 2;
/// HEVC picture flags: slice chroma QP offsets.
pub(crate) const HEVC_PIC_CHROMA_OFFSETS: u32 = 1 << 3;
/// HEVC picture flags: weighted prediction.
pub(crate) const HEVC_PIC_WEIGHTED_PRED: u32 = 1 << 4;
/// HEVC picture flags: weighted biprediction.
pub(crate) const HEVC_PIC_WEIGHTED_BIPRED: u32 = 1 << 5;
/// HEVC picture flags: transquant bypass.
pub(crate) const HEVC_PIC_TRANSQUANT_BYPASS: u32 = 1 << 6;
/// HEVC picture flags: tiles.
pub(crate) const HEVC_PIC_TILES: u32 = 1 << 7;
/// HEVC picture flags: wavefront sync.
pub(crate) const HEVC_PIC_SYNC: u32 = 1 << 8;
/// HEVC picture flags: uniform tile spacing.
pub(crate) const HEVC_PIC_UNIFORM_SPACING: u32 = 1 << 9;
/// HEVC picture flags: loop filter across tiles.
pub(crate) const HEVC_PIC_LOOP_ACROSS_TILES: u32 = 1 << 10;
/// HEVC picture flags: loop filter across slices.
pub(crate) const HEVC_PIC_LOOP_ACROSS_SLICES: u32 = 1 << 11;
/// HEVC picture flags: deblocking override.
pub(crate) const HEVC_PIC_DEBLOCK_OVERRIDE: u32 = 1 << 12;
/// HEVC picture flags: deblocking disabled.
pub(crate) const HEVC_PIC_DEBLOCK_DISABLED: u32 = 1 << 13;
/// HEVC picture flags: reference list modification.
pub(crate) const HEVC_PIC_LISTS_MOD: u32 = 1 << 14;
/// HEVC picture flags: slice extension present.
pub(crate) const HEVC_PIC_SLICE_EXT: u32 = 1 << 15;
/// HEVC picture flags: random access picture.
pub(crate) const HEVC_PIC_IRAP: u32 = 1 << 16;
/// HEVC picture flags: IDR picture.
pub(crate) const HEVC_PIC_IDR: u32 = 1 << 17;
/// HEVC picture flags: intra picture.
pub(crate) const HEVC_PIC_INTRA: u32 = 1 << 18;

/// HEVC picture parameters, 232 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsHevc {
    /// Picture width in minimum coding blocks.
    pub(crate) width_in_min_cbs: u16,
    /// Picture height in minimum coding blocks.
    pub(crate) height_in_min_cbs: u16,
    /// Packed format and sequence flags.
    pub(crate) format_flags: u16,
    /// Current surface.
    pub(crate) curr_pic: PicEntryHevc,
    /// Maximum buffered pictures minus one.
    pub(crate) max_dec_pic_buffering_minus1: u8,
    /// Minimum luma block size minus 3.
    pub(crate) log2_min_cb_size_minus3: u8,
    /// Luma block size range.
    pub(crate) log2_diff_max_min_cb_size: u8,
    /// Minimum transform size minus 2.
    pub(crate) log2_min_tb_size_minus2: u8,
    /// Transform size range.
    pub(crate) log2_diff_max_min_tb_size: u8,
    /// Maximum inter transform depth.
    pub(crate) max_transform_depth_inter: u8,
    /// Maximum intra transform depth.
    pub(crate) max_transform_depth_intra: u8,
    /// Short-term reference set count.
    pub(crate) num_short_term_rps: u8,
    /// Long-term picture count in the sequence set.
    pub(crate) num_long_term_sps: u8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_l0_default_minus1: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_l1_default_minus1: u8,
    /// Luma QP init delta.
    pub(crate) init_qp_minus26: i8,
    /// Delta POC count of the reference set.
    pub(crate) num_delta_pocs_of_ref_rps: u8,
    /// Reference set bit length in the slice.
    pub(crate) num_bits_for_rps_in_slice: u16,
    /// Reserved.
    pub(crate) reserved_2: u16,
    /// Packed coding tool flags.
    pub(crate) tool_flags: u32,
    /// Packed picture property flags.
    pub(crate) pic_flags: u32,
    /// Cb QP offset.
    pub(crate) pps_cb_qp_offset: i8,
    /// Cr QP offset.
    pub(crate) pps_cr_qp_offset: i8,
    /// Tile column count minus one.
    pub(crate) num_tile_columns_minus1: u8,
    /// Tile row count minus one.
    pub(crate) num_tile_rows_minus1: u8,
    /// Tile column widths minus one.
    pub(crate) column_width_minus1: [u16; 19],
    /// Tile row heights minus one.
    pub(crate) row_height_minus1: [u16; 21],
    /// CU QP delta depth.
    pub(crate) diff_cu_qp_delta_depth: u8,
    /// Deblocking beta offset over two.
    pub(crate) pps_beta_offset_div2: i8,
    /// Deblocking tc offset over two.
    pub(crate) pps_tc_offset_div2: i8,
    /// Parallel merge level minus 2.
    pub(crate) log2_parallel_merge_minus2: u8,
    /// Current picture order count.
    pub(crate) curr_poc: i32,
    /// Reference surfaces; flag is the long-term flag.
    pub(crate) ref_pic_list: [PicEntryHevc; 15],
    /// Reserved.
    pub(crate) reserved_5: u8,
    /// Reference picture order counts.
    pub(crate) poc_list: [i32; 15],
    /// Past short-term references by parameter index.
    pub(crate) ref_set_st_curr_before: [u8; 8],
    /// Future short-term references by parameter index.
    pub(crate) ref_set_st_curr_after: [u8; 8],
    /// Long-term references by parameter index.
    pub(crate) ref_set_lt_curr: [u8; 8],
    /// Reserved.
    pub(crate) reserved_6: u16,
    /// Reserved.
    pub(crate) reserved_7: u16,
    /// Driver-echoed picture serial.
    pub(crate) feedback: u32,
}

impl PicParamsHevc {
    /// Parameters with invalid references and sets.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            width_in_min_cbs: 0,
            height_in_min_cbs: 0,
            format_flags: 0,
            curr_pic: PicEntryHevc::INVALID,
            max_dec_pic_buffering_minus1: 0,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 0,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 0,
            max_transform_depth_inter: 0,
            max_transform_depth_intra: 0,
            num_short_term_rps: 0,
            num_long_term_sps: 0,
            num_ref_l0_default_minus1: 0,
            num_ref_l1_default_minus1: 0,
            init_qp_minus26: 0,
            num_delta_pocs_of_ref_rps: 0,
            num_bits_for_rps_in_slice: 0,
            reserved_2: 0,
            tool_flags: 0,
            pic_flags: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            diff_cu_qp_delta_depth: 0,
            pps_beta_offset_div2: 0,
            pps_tc_offset_div2: 0,
            log2_parallel_merge_minus2: 0,
            curr_poc: 0,
            ref_pic_list: [PicEntryHevc::INVALID; 15],
            reserved_5: 0,
            poc_list: [0; 15],
            ref_set_st_curr_before: [0xFF; 8],
            ref_set_st_curr_after: [0xFF; 8],
            ref_set_lt_curr: [0xFF; 8],
            reserved_6: 0,
            reserved_7: 0,
            feedback: 0,
        }
    }
}

/// HEVC quantization matrices, 1000 bytes in raster order.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QmatrixHevc {
    /// Six 4x4 scaling lists.
    pub(crate) lists_0: [[u8; 16]; 6],
    /// Six 8x8 scaling lists.
    pub(crate) lists_1: [[u8; 64]; 6],
    /// Six 16x16 scaling lists.
    pub(crate) lists_2: [[u8; 64]; 6],
    /// Two 32x32 scaling lists.
    pub(crate) lists_3: [[u8; 64]; 2],
    /// 16x16 DC coefficients.
    pub(crate) dc_size2: [u8; 6],
    /// 32x32 DC coefficients.
    pub(crate) dc_size3: [u8; 2],
}

/// HEVC short slice control entry, 12 bytes with trailing pad.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SliceShortHevc {
    /// NAL offset in the bitstream buffer.
    pub(crate) location: u32,
    /// NAL length in bytes.
    pub(crate) bytes: u32,
    /// Bad slice chopping flag.
    pub(crate) chopping: u16,
}

/// AV1 reference motion flags: invalid bit.
pub(crate) const AV1_MOTION_INVALID: u8 = 1;
/// AV1 reference motion flags: type shift.
pub(crate) const AV1_MOTION_TYPE_SHIFT: u8 = 1;

/// AV1 reference entry: extent, global motion, slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicEntryAv1 {
    /// Reference width in luma samples.
    pub(crate) width: u32,
    /// Reference height in luma samples.
    pub(crate) height: u32,
    /// Global motion matrix.
    pub(crate) wmmat: [i32; 6],
    /// Packed motion flags.
    pub(crate) motion_flags: u8,
    /// Reference slot, 0xFF when absent.
    pub(crate) index: u8,
    /// Reserved.
    pub(crate) reserved: u16,
}

impl PicEntryAv1 {
    /// Entry naming no reference.
    pub(crate) const INVALID: Self = Self {
        width: 0,
        height: 0,
        wmmat: [0; 6],
        motion_flags: AV1_MOTION_INVALID,
        index: 0xFF,
        reserved: 0,
    };
}

/// AV1 tile grid: column count, row count, context tile, superblock extents.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TilesAv1 {
    /// Tile column count.
    pub(crate) cols: u8,
    /// Tile row count.
    pub(crate) rows: u8,
    /// Context update tile id.
    pub(crate) context_update_id: u16,
    /// Tile column widths in superblocks.
    pub(crate) widths: [u16; 64],
    /// Tile row heights in superblocks.
    pub(crate) heights: [u16; 64],
}

/// AV1 loop filter control: delta enabled bit.
pub(crate) const AV1_LF_DELTA_ENABLED: u8 = 1;
/// AV1 loop filter control: delta update bit.
pub(crate) const AV1_LF_DELTA_UPDATE: u8 = 1 << 1;
/// AV1 loop filter control: multi-delta bit.
pub(crate) const AV1_LF_DELTA_MULTI: u8 = 1 << 2;
/// AV1 loop filter control: delta present bit.
pub(crate) const AV1_LF_DELTA_PRESENT: u8 = 1 << 3;

/// AV1 loop filter parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LoopFilterAv1 {
    /// Luma filter levels.
    pub(crate) filter_level: [u8; 2],
    /// U filter level.
    pub(crate) filter_level_u: u8,
    /// V filter level.
    pub(crate) filter_level_v: u8,
    /// Sharpness level.
    pub(crate) sharpness: u8,
    /// Packed delta control flags.
    pub(crate) control: u8,
    /// Reference deltas.
    pub(crate) ref_deltas: [i8; 8],
    /// Mode deltas.
    pub(crate) mode_deltas: [i8; 2],
    /// Delta resolution.
    pub(crate) delta_lf_res: u8,
    /// Restoration types per plane.
    pub(crate) restoration_type: [u8; 3],
    /// Restoration unit sizes per plane.
    pub(crate) log2_restoration_unit_size: [u16; 3],
    /// Reserved.
    pub(crate) reserved: u16,
}

/// AV1 quantization control: delta present bit.
pub(crate) const AV1_Q_DELTA_PRESENT: u8 = 1;
/// AV1 quantization control: delta resolution shift.
pub(crate) const AV1_Q_DELTA_RES_SHIFT: u8 = 1;

/// AV1 quantization parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QuantAv1 {
    /// Packed delta control flags.
    pub(crate) control: u8,
    /// Base quantizer index.
    pub(crate) base_qindex: u8,
    /// Y DC delta.
    pub(crate) y_dc_delta: i8,
    /// U DC delta.
    pub(crate) u_dc_delta: i8,
    /// V DC delta.
    pub(crate) v_dc_delta: i8,
    /// U AC delta.
    pub(crate) u_ac_delta: i8,
    /// V AC delta.
    pub(crate) v_ac_delta: i8,
    /// Y matrix id, 0xFF when unused.
    pub(crate) qm_y: u8,
    /// U matrix id, 0xFF when unused.
    pub(crate) qm_u: u8,
    /// V matrix id, 0xFF when unused.
    pub(crate) qm_v: u8,
    /// Reserved.
    pub(crate) reserved: u16,
}

/// AV1 CDEF control: damping shift.
pub(crate) const AV1_CDEF_DAMPING_SHIFT: u8 = 0;
/// AV1 CDEF control: strength bits shift.
pub(crate) const AV1_CDEF_BITS_SHIFT: u8 = 2;
/// AV1 CDEF strength: secondary shift.
pub(crate) const AV1_CDEF_SECONDARY_SHIFT: u8 = 6;

/// AV1 CDEF parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CdefAv1 {
    /// Packed damping and strength bits.
    pub(crate) control: u8,
    /// Y strengths: primary in bits 0-5, secondary in 6-7.
    pub(crate) y_strengths: [u8; 8],
    /// UV strengths: primary in bits 0-5, secondary in 6-7.
    pub(crate) uv_strengths: [u8; 8],
}

/// AV1 segmentation control: enabled bit.
pub(crate) const AV1_SEG_ENABLED: u8 = 1;
/// AV1 segmentation control: map update bit.
pub(crate) const AV1_SEG_UPDATE_MAP: u8 = 1 << 1;
/// AV1 segmentation control: data update bit.
pub(crate) const AV1_SEG_UPDATE_DATA: u8 = 1 << 2;
/// AV1 segmentation control: temporal update bit.
pub(crate) const AV1_SEG_TEMPORAL_UPDATE: u8 = 1 << 3;

/// AV1 segmentation parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SegmentationAv1 {
    /// Packed update control flags.
    pub(crate) control: u8,
    /// Reserved.
    pub(crate) reserved24: [u8; 3],
    /// Feature enable masks per segment.
    pub(crate) feature_mask: [u8; 8],
    /// Feature values per segment and level.
    pub(crate) feature_data: [[i16; 8]; 8],
}

/// AV1 film grain control: grain applied bit.
pub(crate) const AV1_GRAIN_APPLY: u16 = 1;
/// AV1 film grain control: scaling shift.
pub(crate) const AV1_GRAIN_SCALING_SHIFT: u16 = 1;
/// AV1 film grain control: chroma from luma bit.
pub(crate) const AV1_GRAIN_CHROMA_FROM_LUMA: u16 = 1 << 3;
/// AV1 film grain control: coefficient lag shift.
pub(crate) const AV1_GRAIN_COEFF_LAG_SHIFT: u16 = 4;
/// AV1 film grain control: coefficient shift.
pub(crate) const AV1_GRAIN_COEFF_SHIFT: u16 = 6;
/// AV1 film grain control: scale shift.
pub(crate) const AV1_GRAIN_SCALE_SHIFT: u16 = 8;
/// AV1 film grain control: overlap bit.
pub(crate) const AV1_GRAIN_OVERLAP: u16 = 1 << 10;
/// AV1 film grain control: restricted range bit.
pub(crate) const AV1_GRAIN_RESTRICTED_RANGE: u16 = 1 << 11;
/// AV1 film grain control: identity matrix bit.
pub(crate) const AV1_GRAIN_IDENTITY_MATRIX: u16 = 1 << 12;

/// AV1 film grain parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FilmGrainAv1 {
    /// Packed grain control flags.
    pub(crate) control: u16,
    /// Grain seed.
    pub(crate) grain_seed: u16,
    /// Y scaling points.
    pub(crate) scaling_points_y: [[u8; 2]; 14],
    /// Y scaling point count.
    pub(crate) num_y_points: u8,
    /// Cb scaling points.
    pub(crate) scaling_points_cb: [[u8; 2]; 10],
    /// Cb scaling point count.
    pub(crate) num_cb_points: u8,
    /// Cr scaling points.
    pub(crate) scaling_points_cr: [[u8; 2]; 10],
    /// Cr scaling point count.
    pub(crate) num_cr_points: u8,
    /// Y autoregressive coefficients.
    pub(crate) ar_coeffs_y: [u8; 24],
    /// Cb autoregressive coefficients.
    pub(crate) ar_coeffs_cb: [u8; 25],
    /// Cr autoregressive coefficients.
    pub(crate) ar_coeffs_cr: [u8; 25],
    /// Cb multiplier.
    pub(crate) cb_mult: u8,
    /// Cb luma multiplier.
    pub(crate) cb_luma_mult: u8,
    /// Cr multiplier.
    pub(crate) cr_mult: u8,
    /// Cr luma multiplier.
    pub(crate) cr_luma_mult: u8,
    /// Reserved.
    pub(crate) reserved: u8,
    /// Cb offset.
    pub(crate) cb_offset: i16,
    /// Cr offset.
    pub(crate) cr_offset: i16,
}

/// AV1 coding flags: 128x128 superblocks.
pub(crate) const AV1_CODING_SUPERBLOCK_128: u32 = 1;
/// AV1 coding flags: intra edge filter.
pub(crate) const AV1_CODING_INTRA_EDGE: u32 = 1 << 1;
/// AV1 coding flags: interintra compound.
pub(crate) const AV1_CODING_INTERINTRA: u32 = 1 << 2;
/// AV1 coding flags: masked compound.
pub(crate) const AV1_CODING_MASKED_COMPOUND: u32 = 1 << 3;
/// AV1 coding flags: warped motion.
pub(crate) const AV1_CODING_WARPED: u32 = 1 << 4;
/// AV1 coding flags: dual filter.
pub(crate) const AV1_CODING_DUAL_FILTER: u32 = 1 << 5;
/// AV1 coding flags: joint compound.
pub(crate) const AV1_CODING_JNT_COMP: u32 = 1 << 6;
/// AV1 coding flags: screen content tools.
pub(crate) const AV1_CODING_SCREEN_CONTENT: u32 = 1 << 7;
/// AV1 coding flags: integer motion vectors.
pub(crate) const AV1_CODING_INTEGER_MV: u32 = 1 << 8;
/// AV1 coding flags: CDEF.
pub(crate) const AV1_CODING_CDEF: u32 = 1 << 9;
/// AV1 coding flags: restoration.
pub(crate) const AV1_CODING_RESTORATION: u32 = 1 << 10;
/// AV1 coding flags: film grain.
pub(crate) const AV1_CODING_FILM_GRAIN: u32 = 1 << 11;
/// AV1 coding flags: intra block copy.
pub(crate) const AV1_CODING_INTRABC: u32 = 1 << 12;
/// AV1 coding flags: high precision motion vectors.
pub(crate) const AV1_CODING_HIGH_PRECISION_MV: u32 = 1 << 13;
/// AV1 coding flags: switchable motion mode.
pub(crate) const AV1_CODING_SWITCHABLE_MOTION: u32 = 1 << 14;
/// AV1 coding flags: filter intra.
pub(crate) const AV1_CODING_FILTER_INTRA: u32 = 1 << 15;
/// AV1 coding flags: no frame-end CDF update.
pub(crate) const AV1_CODING_NO_FRAME_END_CDF: u32 = 1 << 16;
/// AV1 coding flags: no CDF update.
pub(crate) const AV1_CODING_NO_CDF_UPDATE: u32 = 1 << 17;
/// AV1 coding flags: compound reference mode.
pub(crate) const AV1_CODING_REFERENCE_MODE: u32 = 1 << 18;
/// AV1 coding flags: skip mode present.
pub(crate) const AV1_CODING_SKIP_MODE: u32 = 1 << 19;
/// AV1 coding flags: reduced transform set.
pub(crate) const AV1_CODING_REDUCED_TX: u32 = 1 << 20;
/// AV1 coding flags: superres.
pub(crate) const AV1_CODING_SUPERRES: u32 = 1 << 21;
/// AV1 coding flags: transform mode shift.
pub(crate) const AV1_CODING_TX_MODE_SHIFT: u32 = 22;
/// AV1 coding flags: use reference motion vectors.
pub(crate) const AV1_CODING_USE_REF_MVS: u32 = 1 << 24;
/// AV1 coding flags: reference motion vectors enabled.
pub(crate) const AV1_CODING_REF_MVS_ENABLED: u32 = 1 << 25;
/// AV1 coding flags: reference frame update.
pub(crate) const AV1_CODING_REF_UPDATE: u32 = 1 << 26;

/// AV1 format flags: frame type shift.
pub(crate) const AV1_FORMAT_TYPE_SHIFT: u8 = 0;
/// AV1 format flags: show frame bit.
pub(crate) const AV1_FORMAT_SHOW_FRAME: u8 = 1 << 2;
/// AV1 format flags: showable frame bit.
pub(crate) const AV1_FORMAT_SHOWABLE_FRAME: u8 = 1 << 3;
/// AV1 format flags: horizontal subsampling bit.
pub(crate) const AV1_FORMAT_SUBSAMPLING_X: u8 = 1 << 4;
/// AV1 format flags: vertical subsampling bit.
pub(crate) const AV1_FORMAT_SUBSAMPLING_Y: u8 = 1 << 5;
/// AV1 format flags: monochrome bit.
pub(crate) const AV1_FORMAT_MONO_CHROME: u8 = 1 << 6;

/// AV1 picture parameters, 912 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsAv1 {
    /// Frame width in luma samples.
    pub(crate) width: u32,
    /// Frame height in luma samples.
    pub(crate) height: u32,
    /// Maximum frame width in luma samples.
    pub(crate) max_width: u32,
    /// Maximum frame height in luma samples.
    pub(crate) max_height: u32,
    /// Current texture index.
    pub(crate) curr_pic_index: u8,
    /// Superres denominator.
    pub(crate) superres_denom: u8,
    /// Bit depth.
    pub(crate) bitdepth: u8,
    /// Sequence profile.
    pub(crate) seq_profile: u8,
    /// Tile grid.
    pub(crate) tiles: TilesAv1,
    /// Packed coding tool flags.
    pub(crate) coding_flags: u32,
    /// Packed format flags.
    pub(crate) format_flags: u8,
    /// Primary reference frame.
    pub(crate) primary_ref_frame: u8,
    /// Frame order hint.
    pub(crate) order_hint: u8,
    /// Order hint bit count.
    pub(crate) order_hint_bits: u8,
    /// Reference entries per slot.
    pub(crate) frame_refs: [PicEntryAv1; 7],
    /// Texture index per reference slot.
    pub(crate) ref_map: [u8; 8],
    /// Loop filter parameters.
    pub(crate) loop_filter: LoopFilterAv1,
    /// Quantization parameters.
    pub(crate) quantization: QuantAv1,
    /// CDEF parameters.
    pub(crate) cdef: CdefAv1,
    /// Interpolation filter.
    pub(crate) interp_filter: u8,
    /// Segmentation parameters.
    pub(crate) segmentation: SegmentationAv1,
    /// Film grain parameters.
    pub(crate) film_grain: FilmGrainAv1,
    /// Reserved.
    pub(crate) reserved32: u32,
    /// Driver-echoed picture serial.
    pub(crate) feedback: u32,
}

impl PicParamsAv1 {
    /// Parameters with invalid references and map.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            max_width: 0,
            max_height: 0,
            curr_pic_index: 0xFF,
            superres_denom: 8,
            bitdepth: 8,
            seq_profile: 0,
            tiles: TilesAv1 {
                cols: 0,
                rows: 0,
                context_update_id: 0,
                widths: [0; 64],
                heights: [0; 64],
            },
            coding_flags: 0,
            format_flags: 0,
            primary_ref_frame: 7,
            order_hint: 0,
            order_hint_bits: 0,
            frame_refs: [PicEntryAv1::INVALID; 7],
            ref_map: [0xFF; 8],
            loop_filter: LoopFilterAv1 {
                filter_level: [0; 2],
                filter_level_u: 0,
                filter_level_v: 0,
                sharpness: 0,
                control: 0,
                ref_deltas: [0; 8],
                mode_deltas: [0; 2],
                delta_lf_res: 0,
                restoration_type: [0; 3],
                log2_restoration_unit_size: [8; 3],
                reserved: 0,
            },
            quantization: QuantAv1 {
                control: 0,
                base_qindex: 0,
                y_dc_delta: 0,
                u_dc_delta: 0,
                v_dc_delta: 0,
                u_ac_delta: 0,
                v_ac_delta: 0,
                qm_y: 0xFF,
                qm_u: 0xFF,
                qm_v: 0xFF,
                reserved: 0,
            },
            cdef: CdefAv1 {
                control: 0,
                y_strengths: [0; 8],
                uv_strengths: [0; 8],
            },
            interp_filter: 0,
            segmentation: SegmentationAv1 {
                control: 0,
                reserved24: [0; 3],
                feature_mask: [0; 8],
                feature_data: [[0; 8]; 8],
            },
            film_grain: FilmGrainAv1 {
                control: 0,
                grain_seed: 0,
                scaling_points_y: [[0; 2]; 14],
                num_y_points: 0,
                scaling_points_cb: [[0; 2]; 10],
                num_cb_points: 0,
                scaling_points_cr: [[0; 2]; 10],
                num_cr_points: 0,
                ar_coeffs_y: [0; 24],
                ar_coeffs_cb: [0; 25],
                ar_coeffs_cr: [0; 25],
                cb_mult: 0,
                cb_luma_mult: 0,
                cr_mult: 0,
                cr_luma_mult: 0,
                reserved: 0,
                cb_offset: 0,
                cr_offset: 0,
            },
            reserved32: 0,
            feedback: 0,
        }
    }
}

/// AV1 tile entry, 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TileAv1 {
    /// Tile offset in the bitstream buffer.
    pub(crate) data_offset: u32,
    /// Tile length in bytes.
    pub(crate) data_size: u32,
    /// Tile row.
    pub(crate) row: u16,
    /// Tile column.
    pub(crate) column: u16,
    /// Reserved.
    pub(crate) reserved: u16,
    /// Anchor frame.
    pub(crate) anchor_frame: u8,
    /// Reserved.
    pub(crate) reserved8: u8,
}

/// Sizes and offsets against dxva.h; the driver reads these bytes verbatim.
const _: () = {
    assert!(size_of::<PicEntryH264>() == 1);
    assert!(size_of::<PicParamsH264>() == 1040);
    assert!(size_of::<QmatrixH264>() == 224);
    assert!(size_of::<SliceLongH264>() == 864);
    assert!(offset_of!(PicParamsH264, curr_pic) == 4);
    assert!(offset_of!(PicParamsH264, ref_frame_list) == 16);
    assert!(offset_of!(PicParamsH264, field_order_cnt_list) == 40);
    assert!(offset_of!(PicParamsH264, frame_num_list) == 176);
    assert!(offset_of!(PicParamsH264, used_for_reference_flags) == 208);
    assert!(offset_of!(PicParamsH264, frame_num) == 214);
    assert!(offset_of!(PicParamsH264, slice_group_map) == 230);
    assert!(offset_of!(SliceLongH264, first_mb) == 10);
    assert!(offset_of!(SliceLongH264, ref_lists) == 24);
    assert!(offset_of!(SliceLongH264, weights) == 88);
    assert!(offset_of!(SliceLongH264, qs_delta) == 856);
    assert!(offset_of!(SliceLongH264, slice_id) == 862);
    assert!(size_of::<PicEntryHevc>() == 1);
    assert!(size_of::<PicParamsHevc>() == 232);
    assert!(size_of::<QmatrixHevc>() == 1000);
    assert!(size_of::<SliceShortHevc>() == 12);
    assert!(offset_of!(PicParamsHevc, format_flags) == 4);
    assert!(offset_of!(PicParamsHevc, curr_pic) == 6);
    assert!(offset_of!(PicParamsHevc, tool_flags) == 24);
    assert!(offset_of!(PicParamsHevc, pic_flags) == 28);
    assert!(offset_of!(PicParamsHevc, column_width_minus1) == 36);
    assert!(offset_of!(PicParamsHevc, row_height_minus1) == 74);
    assert!(offset_of!(PicParamsHevc, curr_poc) == 120);
    assert!(offset_of!(PicParamsHevc, ref_pic_list) == 124);
    assert!(offset_of!(PicParamsHevc, poc_list) == 140);
    assert!(offset_of!(PicParamsHevc, ref_set_st_curr_before) == 200);
    assert!(offset_of!(PicParamsHevc, ref_set_lt_curr) == 216);
    assert!(offset_of!(PicParamsHevc, feedback) == 228);
    assert!(size_of::<PicEntryAv1>() == 36);
    assert!(size_of::<TilesAv1>() == 260);
    assert!(size_of::<LoopFilterAv1>() == 28);
    assert!(size_of::<QuantAv1>() == 12);
    assert!(size_of::<CdefAv1>() == 17);
    assert!(size_of::<SegmentationAv1>() == 140);
    assert!(size_of::<FilmGrainAv1>() == 158);
    assert!(size_of::<PicParamsAv1>() == 912);
    assert!(size_of::<TileAv1>() == 16);
    assert!(offset_of!(PicParamsAv1, tiles) == 20);
    assert!(offset_of!(PicParamsAv1, coding_flags) == 280);
    assert!(offset_of!(PicParamsAv1, format_flags) == 284);
    assert!(offset_of!(PicParamsAv1, frame_refs) == 288);
    assert!(offset_of!(PicParamsAv1, ref_map) == 540);
    assert!(offset_of!(PicParamsAv1, loop_filter) == 548);
    assert!(offset_of!(PicParamsAv1, quantization) == 576);
    assert!(offset_of!(PicParamsAv1, cdef) == 588);
    assert!(offset_of!(PicParamsAv1, interp_filter) == 605);
    assert!(offset_of!(PicParamsAv1, segmentation) == 606);
    assert!(offset_of!(PicParamsAv1, film_grain) == 746);
    assert!(offset_of!(PicParamsAv1, feedback) == 908);
    assert!(offset_of!(TileAv1, anchor_frame) == 14);
};

#[cfg(test)]
mod tests {
    use super::{
        PicEntryAv1, PicEntryH264, PicEntryHevc, PicParamsAv1, PicParamsH264, PicParamsHevc,
    };

    #[test]
    fn entry_packs_index_and_flag() {
        assert_eq!(PicEntryH264::frame(3).entry, 3);
        assert_eq!(PicEntryH264::new(3, true).entry, 0x83);
        assert_eq!(PicEntryH264::INVALID.entry, 0xFF);
    }

    #[test]
    fn empty_clears_references() {
        let params = PicParamsH264::empty();
        assert!(params
            .ref_frame_list
            .iter()
            .all(|entry| entry.entry == 0xFF));
        assert_eq!(params.used_for_reference_flags, 0);
    }

    #[test]
    fn hevc_entry_packs_index_and_flag() {
        assert_eq!(PicEntryHevc::frame(3).entry, 3);
        assert_eq!(PicEntryHevc::new(3, true).entry, 0x83);
        assert_eq!(PicEntryHevc::INVALID.entry, 0xFF);
    }

    #[test]
    fn hevc_empty_invalidates_sets() {
        let params = PicParamsHevc::empty();
        assert!(params.ref_pic_list.iter().all(|entry| entry.entry == 0xFF));
        assert!(params
            .ref_set_st_curr_before
            .iter()
            .all(|index| *index == 0xFF));
        assert!(params
            .ref_set_st_curr_after
            .iter()
            .all(|index| *index == 0xFF));
        assert!(params.ref_set_lt_curr.iter().all(|index| *index == 0xFF));
        assert_eq!(params.feedback, 0);
    }

    #[test]
    fn av1_empty_invalidates_map() {
        let params = PicParamsAv1::empty();
        assert!(params.ref_map.iter().all(|index| *index == 0xFF));
        assert!(params.frame_refs.iter().all(|entry| entry.index == 0xFF));
        assert_eq!(params.superres_denom, 8);
        assert_eq!(params.primary_ref_frame, 7);
        assert_eq!(params.quantization.qm_y, 0xFF);
        assert_eq!(PicEntryAv1::INVALID.index, 0xFF);
    }
}
