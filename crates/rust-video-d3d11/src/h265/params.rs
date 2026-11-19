//! H.265 DXVA picture structures behind D3D11 video decode.
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

/// Sizes and offsets against dxva.h; the driver reads these bytes verbatim.
const _: () = {
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
};

#[cfg(test)]
mod tests {
    use super::{PicEntryHevc, PicParamsHevc};

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
}
