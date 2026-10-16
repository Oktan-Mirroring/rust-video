//! H.264 VA-API parameter buffers from parsed syntax.
//!
//! Portable: pure mapping from sequence, picture, and slice syntax onto
//! the `ffi` layouts; no display handle needed.

use std::os::raw::c_int;

use crate::error::VaApiError;
use crate::ffi::{
    VaIqMatrixBufferH264, VaPictureH264, VaPictureParameterBufferH264, VaSliceParameterBufferH264,
    VaSurfaceId,
};
use crate::frontend::SurfaceSlot;
use crate::h264::dpb::Dpb;
use crate::h264::parser::{Level, Pps, SliceHeader, Sps};
use crate::h264::picture::{Field, PictureData, Reference};

/// VA-API parameter builder over the DPB and the surface pool.
pub struct Params<'a> {
    /// Decoded-picture buffer backing reference pictures.
    dpb: &'a Dpb<SurfaceSlot>,
    /// Pool surfaces by slot index.
    surfaces: &'a [VaSurfaceId],
}

impl<'a> Params<'a> {
    /// Zig-zag scan of a 4x4 block, zig-zag index to raster index.
    const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
    /// Zig-zag scan of an 8x8 block, zig-zag index to raster index.
    const ZIGZAG_8X8: [usize; 64] = [
        0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
        20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
        58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
    ];

    /// Builder over `dpb` with pool `surfaces`.
    #[must_use]
    pub fn new(dpb: &'a Dpb<SurfaceSlot>, surfaces: &'a [VaSurfaceId]) -> Self {
        Self { dpb, surfaces }
    }

    /// VA-API profile for H.264 `profile_idc`.
    ///
    /// Baseline decodes under Constrained Baseline: the VA-API
    /// Baseline profile is deprecated, and the frontend already
    /// declines the Baseline-only syntax outside it.
    ///
    /// # Errors
    ///
    /// Returns `Err` for profiles outside Baseline, Main, and High.
    pub fn profile(profile_idc: u8) -> Result<c_int, VaApiError> {
        match profile_idc {
            66 => Ok(crate::ffi::VA_PROFILE_H264_CONSTRAINED_BASELINE),
            77 => Ok(crate::ffi::VA_PROFILE_H264_MAIN),
            100 => Ok(crate::ffi::VA_PROFILE_H264_HIGH),
            other => Err(VaApiError::Unsupported(format!("h264 profile {other}"))),
        }
    }

    /// Picture-parameter buffer for `pic` on `surface`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a reference picture lost its surface.
    pub fn picture(
        &self,
        sps: &Sps,
        pps: &Pps,
        header: &SliceHeader,
        pic: &PictureData,
        surface: VaSurfaceId,
    ) -> Result<VaPictureParameterBufferH264, VaApiError> {
        let interlaced = u32::from(!sps.frame_field.frame_mbs_only);
        let height_in_mbs = (u32::from(sps.pic_height_in_map_units_minus1) + 1) << interlaced;
        let height_in_mbs = u16::try_from(height_in_mbs.saturating_sub(1))
            .map_err(|_| VaApiError::Parse("coded height range".to_owned()))?;
        Ok(VaPictureParameterBufferH264 {
            curr_pic: Self::picture_descriptor(pic, surface),
            reference_frames: self.reference_frames()?,
            picture_width_in_mbs_minus1: sps.pic_width_in_mbs_minus1,
            picture_height_in_mbs_minus1: height_in_mbs,
            bit_depth_luma_minus8: sps.bit_depth.luma_minus8,
            bit_depth_chroma_minus8: sps.bit_depth.chroma_minus8,
            num_ref_frames: sps.max_num_ref_frames,
            seq_fields: Self::seq_fields(sps),
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: pps.pic_init_qp_minus26,
            pic_init_qs_minus26: pps.pic_init_qs_minus26,
            chroma_qp_index_offset: pps.chroma_qp_index_offset,
            second_chroma_qp_index_offset: pps.second_chroma_qp_index_offset,
            pic_fields: Self::pic_fields(pps, header, pic),
            frame_num: header.frame_num,
            reserved: [0; 8],
        })
    }

    /// Inverse-quantization matrix from the picture set.
    #[must_use]
    pub fn matrix(pps: &Pps) -> VaIqMatrixBufferH264 {
        let mut scaling_4x4 = [[0u8; 16]; 6];
        let mut scaling_8x8 = [[0u8; 64]; 2];
        for (raster, zigzag) in scaling_4x4.iter_mut().zip(pps.scaling_lists_4x4.iter()) {
            for (index, value) in zigzag.iter().enumerate() {
                raster[Self::ZIGZAG_4X4[index]] = *value;
            }
        }
        for (raster, zigzag) in scaling_8x8.iter_mut().zip(pps.scaling_lists_8x8.iter()) {
            for (index, value) in zigzag.iter().enumerate() {
                raster[Self::ZIGZAG_8X8[index]] = *value;
            }
        }
        VaIqMatrixBufferH264 {
            scaling_list_4x4: scaling_4x4,
            scaling_list_8x8: scaling_8x8,
            reserved: [0; 4],
        }
    }

    /// Slice-parameter buffer for one slice.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a listed reference lost its surface, or when a
    /// corrupt slice size falls outside the parameter range.
    pub fn slice(
        &self,
        header: &SliceHeader,
        data_len: usize,
        list0: &[usize],
        list1: &[usize],
        sps: &Sps,
        pps: &Pps,
    ) -> Result<VaSliceParameterBufferH264, VaApiError> {
        let weights = Weights::new(header, sps, pps);
        let range = |what: &str| VaApiError::Parse(format!("slice {what} range"));
        Ok(VaSliceParameterBufferH264 {
            slice_data_size: u32::try_from(data_len).map_err(|_| range("data size"))?,
            slice_data_offset: 0,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            slice_data_bit_offset: u16::try_from(header.header_bit_size)
                .map_err(|_| range("bit offset"))?,
            first_mb_in_slice: u16::try_from(header.first_mb_in_slice)
                .map_err(|_| range("first macroblock"))?,
            slice_type: header.slice_type as u8,
            direct_spatial_mv_pred_flag: u8::from(header.direct_spatial_mv_pred_flag),
            num_ref_idx_l0_active_minus1: header.num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1: header.num_ref_idx_l1_active_minus1,
            cabac_init_idc: header.cabac_init_idc,
            slice_qp_delta: header.slice_qp_delta,
            disable_deblocking_filter_idc: header.disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2: header.slice_alpha_c0_offset_div2,
            slice_beta_offset_div2: header.slice_beta_offset_div2,
            ref_pic_list0: self.ref_list(list0)?,
            ref_pic_list1: self.ref_list(list1)?,
            luma_log2_weight_denom: weights.luma_log2_denom,
            chroma_log2_weight_denom: weights.chroma_log2_denom,
            luma_weight_l0_flag: weights.luma_l0_flag,
            luma_weight_l0: weights.luma_weight_l0,
            luma_offset_l0: weights.luma_offset_l0,
            chroma_weight_l0_flag: weights.chroma_l0_flag,
            chroma_weight_l0: weights.chroma_weight_l0,
            chroma_offset_l0: weights.chroma_offset_l0,
            luma_weight_l1_flag: weights.luma_l1_flag,
            luma_weight_l1: weights.luma_weight_l1,
            luma_offset_l1: weights.luma_offset_l1,
            chroma_weight_l1_flag: weights.chroma_l1_flag,
            chroma_weight_l1: weights.chroma_weight_l1,
            chroma_offset_l1: weights.chroma_offset_l1,
            reserved: [0; 4],
        })
    }

    /// DPB contents as VA-API reference frames, padded to sixteen.
    fn reference_frames(&self) -> Result<[VaPictureH264; 16], VaApiError> {
        let mut frames = [Self::invalid(); 16];
        let mut count = 0;
        for entry in self
            .dpb
            .short_term_refs_iter()
            .chain(self.dpb.long_term_refs_iter())
        {
            if count == frames.len() {
                break;
            }
            let picture = entry.pic.borrow();
            if picture.nonexisting {
                continue;
            }
            let Some(slot) = entry.reference.or(entry.decoded_frame) else {
                continue;
            };
            frames[count] = self.describe(&picture, slot)?;
            count += 1;
        }
        Ok(frames)
    }

    /// Reference list as VA-API pictures, padded to thirty-two.
    fn ref_list(&self, indices: &[usize]) -> Result<[VaPictureH264; 32], VaApiError> {
        let mut list = [Self::invalid(); 32];
        for (slot, index) in list.iter_mut().zip(indices.iter()) {
            let entry =
                self.dpb.entries().get(*index).ok_or_else(|| {
                    VaApiError::BrokenReference(format!("dpb index {index} absent"))
                })?;
            let picture = entry.pic.borrow();
            let Some(backing) = entry.reference.or(entry.decoded_frame) else {
                return Err(VaApiError::BrokenReference(
                    "reference without slot".to_owned(),
                ));
            };
            *slot = self.describe(&picture, backing)?;
        }
        Ok(list)
    }

    /// VA-API descriptor for a DPB picture on `slot`.
    fn describe(
        &self,
        picture: &PictureData,
        slot: SurfaceSlot,
    ) -> Result<VaPictureH264, VaApiError> {
        let surface = self
            .surfaces
            .get(slot.index() as usize)
            .copied()
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        Ok(Self::picture_descriptor(picture, surface))
    }

    /// VA-API descriptor for `picture` on `surface`.
    fn picture_descriptor(picture: &PictureData, surface: VaSurfaceId) -> VaPictureH264 {
        let mut flags = 0;
        let frame_idx = match picture.reference() {
            Reference::LongTerm => {
                flags |= crate::ffi::VA_PICTURE_H264_LONG_TERM_REFERENCE;
                picture.long_term_frame_idx
            }
            Reference::ShortTerm => {
                flags |= crate::ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE;
                picture.frame_num
            }
            Reference::None => picture.frame_num,
        };
        VaPictureH264 {
            picture_id: surface,
            frame_idx,
            flags,
            top_field_order_cnt: picture.top_field_order_cnt,
            bottom_field_order_cnt: picture.bottom_field_order_cnt,
            reserved: [0; 4],
        }
    }

    /// Invalid VA-API picture marker.
    fn invalid() -> VaPictureH264 {
        VaPictureH264 {
            picture_id: crate::ffi::VA_INVALID_ID,
            frame_idx: 0,
            flags: crate::ffi::VA_PICTURE_H264_INVALID,
            top_field_order_cnt: 0,
            bottom_field_order_cnt: 0,
            reserved: [0; 4],
        }
    }

    /// Packed sequence flags.
    fn seq_fields(sps: &Sps) -> u32 {
        use crate::ffi::{
            SEQ_CHROMA_FORMAT_IDC, SEQ_DELTA_PIC_ORDER_ALWAYS_ZERO, SEQ_DIRECT_8X8_INFERENCE,
            SEQ_FRAME_MBS_ONLY, SEQ_GAPS_IN_FRAME_NUM, SEQ_LOG2_MAX_FRAME_NUM_MINUS4,
            SEQ_LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4, SEQ_MBAFF, SEQ_MIN_LUMA_BI_PRED_SIZE_8X8,
            SEQ_PIC_ORDER_CNT_TYPE, SEQ_SEPARATE_COLOUR_PLANE,
        };
        (u32::from(sps.chroma.idc) << SEQ_CHROMA_FORMAT_IDC)
            | (u32::from(sps.chroma.separate_plane) << SEQ_SEPARATE_COLOUR_PLANE)
            | (u32::from(sps.gaps_in_frame_num_value_allowed_flag) << SEQ_GAPS_IN_FRAME_NUM)
            | (u32::from(sps.frame_field.frame_mbs_only) << SEQ_FRAME_MBS_ONLY)
            | (u32::from(sps.frame_field.mb_adaptive) << SEQ_MBAFF)
            | (u32::from(sps.direct_8x8_inference_flag) << SEQ_DIRECT_8X8_INFERENCE)
            | (u32::from(sps.level_idc >= Level::L3_1) << SEQ_MIN_LUMA_BI_PRED_SIZE_8X8)
            | (u32::from(sps.log2_max_frame_num_minus4) << SEQ_LOG2_MAX_FRAME_NUM_MINUS4)
            | (u32::from(sps.pic_order_cnt_type) << SEQ_PIC_ORDER_CNT_TYPE)
            | (u32::from(sps.log2_max_pic_order_cnt_lsb_minus4)
                << SEQ_LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4)
            | (u32::from(sps.delta_pic_order_always_zero_flag) << SEQ_DELTA_PIC_ORDER_ALWAYS_ZERO)
    }

    /// Packed picture flags.
    fn pic_fields(pps: &Pps, header: &SliceHeader, pic: &PictureData) -> u32 {
        use crate::ffi::{
            PIC_CONSTRAINED_INTRA_PRED, PIC_DEBLOCKING_FILTER_CONTROL_PRESENT,
            PIC_ENTROPY_CODING_MODE, PIC_FIELD_PIC, PIC_ORDER_PRESENT,
            PIC_REDUNDANT_PIC_CNT_PRESENT, PIC_REFERENCE_PIC, PIC_TRANSFORM_8X8_MODE,
            PIC_WEIGHTED_BIPRED_IDC, PIC_WEIGHTED_PRED,
        };
        (u32::from(pps.coding.entropy_coding_mode_flag) << PIC_ENTROPY_CODING_MODE)
            | (u32::from(pps.prediction.weighted_pred_flag) << PIC_WEIGHTED_PRED)
            | (u32::from(pps.weighted_bipred_idc) << PIC_WEIGHTED_BIPRED_IDC)
            | (u32::from(pps.coding.transform_8x8_mode_flag) << PIC_TRANSFORM_8X8_MODE)
            | (u32::from(header.field != Field::Frame) << PIC_FIELD_PIC)
            | (u32::from(pps.prediction.constrained_intra_pred_flag) << PIC_CONSTRAINED_INTRA_PRED)
            | (u32::from(pps.presence.bottom_field_pic_order_in_frame_present_flag)
                << PIC_ORDER_PRESENT)
            | (u32::from(pps.presence.deblocking_filter_control_present_flag)
                << PIC_DEBLOCKING_FILTER_CONTROL_PRESENT)
            | (u32::from(pps.presence.redundant_pic_cnt_present_flag)
                << PIC_REDUNDANT_PIC_CNT_PRESENT)
            | (u32::from(pic.nal_ref_idc != 0) << PIC_REFERENCE_PIC)
    }
}

/// Prediction weights for one slice.
struct Weights {
    /// Luma denominator.
    luma_log2_denom: u8,
    /// Chroma denominator.
    chroma_log2_denom: u8,
    /// List-0 luma weights present.
    luma_l0_flag: u8,
    /// List-0 luma weights.
    luma_weight_l0: [i16; 32],
    /// List-0 luma offsets.
    luma_offset_l0: [i16; 32],
    /// List-0 chroma weights present.
    chroma_l0_flag: u8,
    /// List-0 chroma weights.
    chroma_weight_l0: [[i16; 2]; 32],
    /// List-0 chroma offsets.
    chroma_offset_l0: [[i16; 2]; 32],
    /// List-1 luma weights present.
    luma_l1_flag: u8,
    /// List-1 luma weights.
    luma_weight_l1: [i16; 32],
    /// List-1 luma offsets.
    luma_offset_l1: [i16; 32],
    /// List-1 chroma weights present.
    chroma_l1_flag: u8,
    /// List-1 chroma weights.
    chroma_weight_l1: [[i16; 2]; 32],
    /// List-1 chroma offsets.
    chroma_offset_l1: [[i16; 2]; 32],
}

impl Weights {
    /// Weights for `header` under `sps` and `pps`.
    fn new(header: &SliceHeader, sps: &Sps, pps: &Pps) -> Self {
        use crate::h264::parser::SliceType;

        let mut weights = Self {
            luma_log2_denom: header.pred_weight_table.luma_log2_weight_denom,
            chroma_log2_denom: header.pred_weight_table.chroma_log2_weight_denom,
            luma_l0_flag: 0,
            luma_weight_l0: [0; 32],
            luma_offset_l0: [0; 32],
            chroma_l0_flag: 0,
            chroma_weight_l0: [[0; 2]; 32],
            chroma_offset_l0: [[0; 2]; 32],
            luma_l1_flag: 0,
            luma_weight_l1: [0; 32],
            luma_offset_l1: [0; 32],
            chroma_l1_flag: 0,
            chroma_weight_l1: [[0; 2]; 32],
            chroma_offset_l1: [[0; 2]; 32],
        };
        let table = &header.pred_weight_table;
        let l0 = pps.prediction.weighted_pred_flag
            && matches!(header.slice_type, SliceType::P | SliceType::Sp);
        let l1 = pps.weighted_bipred_idc == 1 && matches!(header.slice_type, SliceType::B);
        if l0 || l1 {
            weights.luma_l0_flag = 1;
            let end = usize::from(header.num_ref_idx_l0_active_minus1) + 1;
            for (index, (weight, offset)) in weights
                .luma_weight_l0
                .iter_mut()
                .zip(weights.luma_offset_l0.iter_mut())
                .enumerate()
                .take(end)
            {
                *weight = table.luma_weight_l0[index];
                *offset = i16::from(table.luma_offset_l0[index]);
            }
            if sps.chroma_array_type() != 0 {
                weights.chroma_l0_flag = 1;
                for index in 0..end {
                    weights.chroma_weight_l0[index] = table.chroma_weight_l0[index];
                    for component in 0..2 {
                        weights.chroma_offset_l0[index][component] =
                            i16::from(table.chroma_offset_l0[index][component]);
                    }
                }
            }
        }
        if l1 {
            weights.luma_l1_flag = 1;
            let end = usize::from(header.num_ref_idx_l1_active_minus1) + 1;
            for (index, (weight, offset)) in weights
                .luma_weight_l1
                .iter_mut()
                .zip(weights.luma_offset_l1.iter_mut())
                .enumerate()
                .take(end)
            {
                *weight = table.luma_weight_l1[index];
                *offset = table.luma_offset_l1[index];
            }
            if sps.chroma_array_type() != 0 {
                weights.chroma_l1_flag = 1;
                for index in 0..end {
                    weights.chroma_weight_l1[index] = table.chroma_weight_l1[index];
                    for component in 0..2 {
                        weights.chroma_offset_l1[index][component] =
                            i16::from(table.chroma_offset_l1[index][component]);
                    }
                }
            }
        }
        weights
    }
}

#[cfg(test)]
mod tests {
    use super::Params;
    use crate::error::VaApiError;
    use crate::frontend::SurfaceSlot;
    use crate::h264::dpb::Dpb;
    use crate::h264::parser::{
        ChromaFormat, FrameFieldCoding, Level, PpsBuilder, SliceHeader, Sps,
    };
    use crate::h264::picture::{PictureData, Reference};

    /// Sequence set matching the word test below.
    fn sps() -> std::rc::Rc<Sps> {
        std::rc::Rc::new(Sps {
            profile_idc: 100,
            level_idc: Level::L4,
            chroma: ChromaFormat {
                idc: 1,
                ..Default::default()
            },
            pic_width_in_mbs_minus1: 119,
            pic_height_in_map_units_minus1: 67,
            frame_field: FrameFieldCoding {
                frame_mbs_only: true,
                ..Default::default()
            },
            direct_8x8_inference_flag: true,
            ..Sps::default()
        })
    }

    #[test]
    fn profile_maps_baseline_main_high() {
        assert_eq!(
            Params::profile(66).expect("baseline"),
            crate::ffi::VA_PROFILE_H264_CONSTRAINED_BASELINE
        );
        assert_eq!(
            Params::profile(77).expect("main"),
            crate::ffi::VA_PROFILE_H264_MAIN
        );
        assert_eq!(
            Params::profile(100).expect("high"),
            crate::ffi::VA_PROFILE_H264_HIGH
        );
        assert!(matches!(
            Params::profile(110),
            Err(VaApiError::Unsupported(_))
        ));
    }

    #[test]
    fn zigzag_tables_cover_every_raster_slot() {
        let mut seen_4x4 = [false; 16];
        for slot in Params::ZIGZAG_4X4 {
            assert!(!seen_4x4[slot], "duplicate {slot}");
            seen_4x4[slot] = true;
        }
        let mut seen_8x8 = [false; 64];
        for slot in Params::ZIGZAG_8X8 {
            assert!(!seen_8x8[slot], "duplicate {slot}");
            seen_8x8[slot] = true;
        }
        assert_eq!(Params::ZIGZAG_4X4[3], 8);
        assert_eq!(Params::ZIGZAG_8X8[5], 2);
    }

    #[test]
    fn matrix_converts_zigzag_to_raster() {
        let sps = sps();
        let mut pps = PpsBuilder::new(sps.clone()).build();
        let lists = std::rc::Rc::get_mut(&mut pps).expect("unique pps");
        for (index, slot) in lists.scaling_lists_4x4[0].iter_mut().enumerate() {
            *slot = u8::try_from(index).expect("small list");
        }
        let matrix = Params::matrix(&pps);
        assert_eq!(
            matrix.scaling_list_4x4[0],
            [0, 1, 5, 6, 2, 4, 7, 12, 3, 8, 11, 13, 9, 10, 14, 15]
        );
        assert_eq!(matrix.scaling_list_8x8[0], [0; 64]);
    }

    #[test]
    fn picture_packs_seq_and_pic_words() {
        let sps = sps();
        let pps = PpsBuilder::new(sps.clone()).build();
        let dpb: Dpb<SurfaceSlot> = Dpb::default();
        let params = Params::new(&dpb, &[7]);
        let mut pic = PictureData::default();
        pic.nal_ref_idc = 1;
        pic.set_reference(Reference::ShortTerm);
        let header = SliceHeader::default();
        let raw = params
            .picture(&sps, &pps, &header, &pic, 7)
            .expect("picture params");
        assert_eq!(raw.seq_fields, 209);
        assert_eq!(raw.pic_fields, 1024);
        assert_eq!(raw.curr_pic.picture_id, 7);
        assert_eq!(raw.reference_frames.len(), 16);
        assert_eq!(
            raw.reference_frames[0].flags,
            crate::ffi::VA_PICTURE_H264_INVALID
        );
    }

    #[test]
    fn slice_reports_reference_without_surface() {
        let sps = sps();
        let pps = PpsBuilder::new(sps.clone()).build();
        let mut dpb: Dpb<SurfaceSlot> = Dpb::default();
        dpb.set_limits(16, 16);
        let mut pic = PictureData::default();
        pic.set_reference(Reference::ShortTerm);
        dpb.store_picture(pic.into_rc(), Some(SurfaceSlot::new(3)))
            .expect("dpb room");
        let params = Params::new(&dpb, &[7]);
        let header = SliceHeader::default();
        assert!(matches!(
            params.slice(&header, 64, &[0], &[], &sps, &pps),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn ref_list_describes_surface_backed_entries() {
        let sps = sps();
        let pps = PpsBuilder::new(sps.clone()).build();
        let mut dpb: Dpb<SurfaceSlot> = Dpb::default();
        dpb.set_limits(16, 16);
        let mut pic = PictureData::default();
        pic.frame_num = 5;
        pic.top_field_order_cnt = 10;
        pic.bottom_field_order_cnt = 10;
        pic.set_reference(Reference::ShortTerm);
        dpb.store_picture(pic.into_rc(), Some(SurfaceSlot::new(0)))
            .expect("dpb room");
        let params = Params::new(&dpb, &[41]);
        let header = SliceHeader::default();
        let raw = params
            .slice(&header, 64, &[0], &[], &sps, &pps)
            .expect("slice params");
        assert_eq!(raw.ref_pic_list0[0].picture_id, 41);
        assert_eq!(raw.ref_pic_list0[0].frame_idx, 5);
        assert_eq!(
            raw.ref_pic_list0[0].flags,
            crate::ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE
        );
        assert_eq!(
            raw.ref_pic_list0[1].flags,
            crate::ffi::VA_PICTURE_H264_INVALID
        );
        assert_eq!(raw.slice_data_size, 64);
    }

    #[test]
    fn slice_rejects_corrupt_sizes() {
        let sps = sps();
        let pps = PpsBuilder::new(sps.clone()).build();
        let dpb: Dpb<SurfaceSlot> = Dpb::default();
        let params = Params::new(&dpb, &[7]);
        let header = SliceHeader {
            header_bit_size: usize::from(u16::MAX) + 1,
            ..SliceHeader::default()
        };
        assert!(matches!(
            params.slice(&header, 64, &[], &[], &sps, &pps),
            Err(VaApiError::Parse(_))
        ));
        let header = SliceHeader::default();
        let huge = usize::try_from(u64::from(u32::MAX) + 1).expect("test size");
        assert!(matches!(
            params.slice(&header, huge, &[], &[], &sps, &pps),
            Err(VaApiError::Parse(_))
        ));
    }
}
