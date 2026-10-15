//! DXVA picture structures behind D3D11 video decode.
//!
//! Field order matches dxva.h in the Windows SDK.

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

#[cfg(test)]
mod tests {
    use super::{PicEntryH264, PicParamsH264, QmatrixH264, SliceLongH264};
    use std::mem::{offset_of, size_of};

    #[test]
    fn entry_packs_index_and_flag() {
        assert_eq!(PicEntryH264::frame(3).entry, 3);
        assert_eq!(PicEntryH264::new(3, true).entry, 0x83);
        assert_eq!(PicEntryH264::INVALID.entry, 0xFF);
    }

    #[test]
    fn params_match_dxva_sizes() {
        assert_eq!(size_of::<PicEntryH264>(), 1);
        assert_eq!(size_of::<PicParamsH264>(), 1040);
        assert_eq!(size_of::<QmatrixH264>(), 224);
        assert_eq!(size_of::<SliceLongH264>(), 864);
    }

    #[test]
    fn params_match_dxva_offsets() {
        assert_eq!(offset_of!(PicParamsH264, curr_pic), 4);
        assert_eq!(offset_of!(PicParamsH264, ref_frame_list), 16);
        assert_eq!(offset_of!(PicParamsH264, field_order_cnt_list), 40);
        assert_eq!(offset_of!(PicParamsH264, frame_num_list), 176);
        assert_eq!(offset_of!(PicParamsH264, used_for_reference_flags), 208);
        assert_eq!(offset_of!(PicParamsH264, frame_num), 214);
        assert_eq!(offset_of!(PicParamsH264, slice_group_map), 230);
        assert_eq!(offset_of!(SliceLongH264, first_mb), 10);
        assert_eq!(offset_of!(SliceLongH264, ref_lists), 24);
        assert_eq!(offset_of!(SliceLongH264, weights), 88);
        assert_eq!(offset_of!(SliceLongH264, qs_delta), 856);
        assert_eq!(offset_of!(SliceLongH264, slice_id), 862);
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
}
