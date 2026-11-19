//! H.265 DXVA picture structures behind D3D12 video decode.
//!
//! Field order matches dxva.h in the Windows SDK.

use std::mem::{offset_of, size_of};

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

    /// Entry for surface `index` without the flag.
    #[must_use]
    pub(crate) const fn frame(index: u8) -> Self {
        Self {
            entry: index & 0x7F,
        }
    }
}

/// Chroma 4:2:0 in the HEVC format flags.
pub(crate) const HEVC_CHROMA_420: u16 = 1;
/// POC LSB width shift in the HEVC format flags.
pub(crate) const HEVC_POC_LSB_SHIFT: u16 = 9;
/// Asymmetric motion partitions in the HEVC tool flags.
pub(crate) const HEVC_TOOL_AMP: u32 = 1 << 1;
/// Sample adaptive offset in the HEVC tool flags.
pub(crate) const HEVC_TOOL_SAO: u32 = 1 << 2;
/// Temporal motion vectors in the HEVC tool flags.
pub(crate) const HEVC_TOOL_TEMPORAL_MVP: u32 = 1 << 18;
/// Strong intra smoothing in the HEVC tool flags.
pub(crate) const HEVC_TOOL_STRONG_INTRA: u32 = 1 << 19;
/// Dependent slices in the HEVC tool flags.
pub(crate) const HEVC_TOOL_DEPENDENT_SLICES: u32 = 1 << 20;
/// Output flag in the HEVC tool flags.
pub(crate) const HEVC_TOOL_OUTPUT_FLAG: u32 = 1 << 21;
/// Reserved header bit shift in the HEVC tool flags.
pub(crate) const HEVC_TOOL_EXTRA_BITS_SHIFT: u32 = 22;
/// Sign data hiding in the HEVC tool flags.
pub(crate) const HEVC_TOOL_SIGN_HIDING: u32 = 1 << 25;
/// CABAC init in the HEVC tool flags.
pub(crate) const HEVC_TOOL_CABAC_INIT: u32 = 1 << 26;
/// Constrained intra prediction in the HEVC property flags.
pub(crate) const HEVC_PROP_CONSTRAINED_INTRA: u32 = 1;
/// Transform skip in the HEVC property flags.
pub(crate) const HEVC_PROP_TRANSFORM_SKIP: u32 = 1 << 1;
/// CU QP deltas in the HEVC property flags.
pub(crate) const HEVC_PROP_CU_QP_DELTA: u32 = 1 << 2;
/// Slice chroma QP offsets in the HEVC property flags.
pub(crate) const HEVC_PROP_CHROMA_QP_OFFSETS: u32 = 1 << 3;
/// Weighted prediction in the HEVC property flags.
pub(crate) const HEVC_PROP_WEIGHTED_PRED: u32 = 1 << 4;
/// Weighted biprediction in the HEVC property flags.
pub(crate) const HEVC_PROP_WEIGHTED_BIPRED: u32 = 1 << 5;
/// Transform bypass in the HEVC property flags.
pub(crate) const HEVC_PROP_TRANSQUANT_BYPASS: u32 = 1 << 6;
/// Tiles in the HEVC property flags.
pub(crate) const HEVC_PROP_TILES: u32 = 1 << 7;
/// Entropy sync in the HEVC property flags.
pub(crate) const HEVC_PROP_ENTROPY_SYNC: u32 = 1 << 8;
/// Uniform tile spacing in the HEVC property flags.
pub(crate) const HEVC_PROP_UNIFORM_SPACING: u32 = 1 << 9;
/// Loop filter across tiles in the HEVC property flags.
pub(crate) const HEVC_PROP_LOOP_TILES: u32 = 1 << 10;
/// Loop filter across slices in the HEVC property flags.
pub(crate) const HEVC_PROP_LOOP_SLICES: u32 = 1 << 11;
/// Deblocking override in the HEVC property flags.
pub(crate) const HEVC_PROP_DEBLOCK_OVERRIDE: u32 = 1 << 12;
/// Disabled deblocking in the HEVC property flags.
pub(crate) const HEVC_PROP_DEBLOCK_DISABLED: u32 = 1 << 13;
/// Reference list modification in the HEVC property flags.
pub(crate) const HEVC_PROP_LISTS_MOD: u32 = 1 << 14;
/// Slice header extension in the HEVC property flags.
pub(crate) const HEVC_PROP_SLICE_EXT: u32 = 1 << 15;
/// IRAP picture in the HEVC property flags.
pub(crate) const HEVC_IRAP_PIC: u32 = 1 << 16;
/// IDR picture in the HEVC property flags.
pub(crate) const HEVC_IDR_PIC: u32 = 1 << 17;
/// Intra-only picture in the HEVC property flags.
pub(crate) const HEVC_INTRA_PIC: u32 = 1 << 18;

/// HEVC picture parameters, 232 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsHevc {
    /// Coded width in minimum coding blocks.
    pub(crate) pic_width_in_min_cbs_y: u16,
    /// Coded height in minimum coding blocks.
    pub(crate) pic_height_in_min_cbs_y: u16,
    /// Format and sequence flags.
    pub(crate) format_flags: u16,
    /// Current surface.
    pub(crate) curr_pic: PicEntryHevc,
    /// Maximum buffering minus one.
    pub(crate) sps_max_dec_pic_buffering_minus1: u8,
    /// Minimum luma block width minus 3.
    pub(crate) log2_min_luma_coding_block_size_minus3: u8,
    /// Luma block size range.
    pub(crate) log2_diff_max_min_luma_coding_block_size: u8,
    /// Minimum transform width minus 2.
    pub(crate) log2_min_transform_block_size_minus2: u8,
    /// Transform size range.
    pub(crate) log2_diff_max_min_transform_block_size: u8,
    /// Inter transform depth.
    pub(crate) max_transform_hierarchy_depth_inter: u8,
    /// Intra transform depth.
    pub(crate) max_transform_hierarchy_depth_intra: u8,
    /// Short-term reference set count.
    pub(crate) num_short_term_ref_pic_sets: u8,
    /// Long-term reference count.
    pub(crate) num_long_term_ref_pics_sps: u8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_idx_l0_default_active_minus1: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_idx_l1_default_active_minus1: u8,
    /// Luma QP init delta.
    pub(crate) init_qp_minus26: i8,
    /// Reference RPS delta POC count.
    pub(crate) num_delta_pocs_of_ref_rps_idx: u8,
    /// Short-term RPS bit count in slices.
    pub(crate) num_bits_for_short_term_rps_in_slice: u16,
    /// Reserved.
    pub(crate) reserved_16: u16,
    /// Coding tool flags.
    pub(crate) tool_flags: u32,
    /// Picture property flags.
    pub(crate) property_flags: u32,
    /// Cb QP offset.
    pub(crate) pps_cb_qp_offset: i8,
    /// Cr QP offset.
    pub(crate) pps_cr_qp_offset: i8,
    /// Tile columns minus one.
    pub(crate) num_tile_columns_minus1: u8,
    /// Tile rows minus one.
    pub(crate) num_tile_rows_minus1: u8,
    /// Tile column widths minus one.
    pub(crate) column_width_minus1: [u16; 19],
    /// Tile row heights minus one.
    pub(crate) row_height_minus1: [u16; 21],
    /// CU QP delta depth.
    pub(crate) diff_cu_qp_delta_depth: u8,
    /// Beta deblocking offset.
    pub(crate) pps_beta_offset_div2: i8,
    /// Tc deblocking offset.
    pub(crate) pps_tc_offset_div2: i8,
    /// Parallel merge level minus 2.
    pub(crate) log2_parallel_merge_level_minus2: u8,
    /// Current picture order count.
    pub(crate) curr_pic_order_cnt_val: i32,
    /// Reference surfaces.
    pub(crate) ref_pic_list: [PicEntryHevc; 15],
    /// Reserved.
    pub(crate) reserved_8: u8,
    /// Reference picture order counts.
    pub(crate) pic_order_cnt_val_list: [i32; 15],
    /// Short-term before references.
    pub(crate) ref_pic_set_st_curr_before: [u8; 8],
    /// Short-term after references.
    pub(crate) ref_pic_set_st_curr_after: [u8; 8],
    /// Long-term current references.
    pub(crate) ref_pic_set_lt_curr: [u8; 8],
    /// Reserved.
    pub(crate) reserved_16_b: u16,
    /// Reserved.
    pub(crate) reserved_16_c: u16,
    /// Driver-echoed picture serial.
    pub(crate) status_report_feedback_number: u32,
}

impl PicParamsHevc {
    /// Parameters with cleared references.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            pic_width_in_min_cbs_y: 0,
            pic_height_in_min_cbs_y: 0,
            format_flags: 0,
            curr_pic: PicEntryHevc::INVALID,
            sps_max_dec_pic_buffering_minus1: 0,
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 0,
            log2_min_transform_block_size_minus2: 0,
            log2_diff_max_min_transform_block_size: 0,
            max_transform_hierarchy_depth_inter: 0,
            max_transform_hierarchy_depth_intra: 0,
            num_short_term_ref_pic_sets: 0,
            num_long_term_ref_pics_sps: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            init_qp_minus26: 0,
            num_delta_pocs_of_ref_rps_idx: 0,
            num_bits_for_short_term_rps_in_slice: 0,
            reserved_16: 0,
            tool_flags: 0,
            property_flags: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            diff_cu_qp_delta_depth: 0,
            pps_beta_offset_div2: 0,
            pps_tc_offset_div2: 0,
            log2_parallel_merge_level_minus2: 0,
            curr_pic_order_cnt_val: 0,
            ref_pic_list: [PicEntryHevc::INVALID; 15],
            reserved_8: 0,
            pic_order_cnt_val_list: [0; 15],
            ref_pic_set_st_curr_before: [0xFF; 8],
            ref_pic_set_st_curr_after: [0xFF; 8],
            ref_pic_set_lt_curr: [0xFF; 8],
            reserved_16_b: 0,
            reserved_16_c: 0,
            status_report_feedback_number: 0,
        }
    }
}

/// HEVC inverse quantization matrices, 1000 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QmatrixHevc {
    /// Six 4x4 scaling lists.
    pub(crate) lists0: [[u8; 16]; 6],
    /// Six 8x8 scaling lists.
    pub(crate) lists1: [[u8; 64]; 6],
    /// Six 16x16 scaling lists.
    pub(crate) lists2: [[u8; 64]; 6],
    /// Two 32x32 scaling lists.
    pub(crate) lists3: [[u8; 64]; 2],
    /// DC coefficients for size id 2.
    pub(crate) dc2: [u8; 6],
    /// DC coefficients for size id 3.
    pub(crate) dc3: [u8; 2],
}

/// HEVC short slice control entry, 12 bytes with tail pad.
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

/// Sizes and offsets against dxva.h; the driver reads these bytes verbatim.
const _: () = {
    assert!(size_of::<PicEntryHevc>() == 1);
    assert!(size_of::<PicParamsHevc>() == 232);
    assert!(size_of::<QmatrixHevc>() == 1000);
    assert!(size_of::<SliceShortHevc>() == 12);
    assert!(offset_of!(PicParamsHevc, curr_pic) == 6);
    assert!(offset_of!(PicParamsHevc, tool_flags) == 24);
    assert!(offset_of!(PicParamsHevc, column_width_minus1) == 36);
    assert!(offset_of!(PicParamsHevc, curr_pic_order_cnt_val) == 120);
    assert!(offset_of!(PicParamsHevc, ref_pic_list) == 124);
    assert!(offset_of!(PicParamsHevc, pic_order_cnt_val_list) == 140);
    assert!(offset_of!(PicParamsHevc, status_report_feedback_number) == 228);
};

#[cfg(test)]
mod tests {
    use super::{PicEntryHevc, PicParamsHevc};

    #[test]
    fn entries_pack_index_and_flag() {
        assert_eq!(PicEntryHevc::frame(3).entry, 3);
        assert_eq!(PicEntryHevc::INVALID.entry, 0xFF);
    }

    #[test]
    fn empty_clears_references() {
        let hevc = PicParamsHevc::empty();
        assert!(hevc.ref_pic_list.iter().all(|entry| entry.entry == 0xFF));
    }
}
