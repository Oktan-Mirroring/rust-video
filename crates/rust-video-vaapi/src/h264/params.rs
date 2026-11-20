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
use crate::h264::dpb::Dpb;
use crate::h264::frontend::{PendingSlice, SurfaceSlot};
use crate::h264::picture::{PictureData, Reference};
use crate::h264::{Pps, SliceHeader, SliceType, Sps};

use rust_video_caps::reason::{
    BrokenReferenceReason as CapsBroken, ParseReason as CapsParse,
    UnsupportedReason as CapsUnsupported,
};

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
            _other => Err(VaApiError::Unsupported(CapsUnsupported::ProfileOutsideMain)),
        }
    }

    /// Picture-parameter buffer for `pic` on `surface`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a reference picture lost its surface, or the
    /// frame number falls outside the parameter range.
    pub fn picture(
        &self,
        sps: &Sps,
        pps: &Pps,
        header: &SliceHeader,
        pic: &PictureData,
        surface: VaSurfaceId,
    ) -> Result<VaPictureParameterBufferH264, VaApiError> {
        Ok(VaPictureParameterBufferH264 {
            curr_pic: Self::picture_descriptor(pic, surface),
            reference_frames: self.reference_frames()?,
            picture_width_in_mbs_minus1: sps.width_in_mbs_minus1,
            picture_height_in_mbs_minus1: sps.height_in_mbs_minus1,
            bit_depth_luma_minus8: sps.fidelity.bit_depth_luma_minus8,
            bit_depth_chroma_minus8: sps.fidelity.bit_depth_chroma_minus8,
            num_ref_frames: sps.max_num_ref_frames,
            seq_fields: Self::seq_fields(sps),
            num_slice_groups_minus1: 0,
            slice_group_map_type: 0,
            slice_group_change_rate_minus1: 0,
            pic_init_qp_minus26: pps.pic_init_qp,
            pic_init_qs_minus26: pps.pic_init_qs,
            chroma_qp_index_offset: pps.chroma_qp,
            second_chroma_qp_index_offset: pps.second_chroma_qp,
            pic_fields: Self::pic_fields(pps, pic),
            frame_num: u16::try_from(header.frame_num).map_err(|_| {
                VaApiError::Parse(CapsParse::Range {
                    syntax: "frame_num",
                })
            })?,
            reserved: [0; 8],
        })
    }

    /// Inverse-quantization matrix from the picture set.
    #[must_use]
    pub fn matrix(pps: &Pps) -> VaIqMatrixBufferH264 {
        let mut scaling_4x4 = [[0u8; 16]; 6];
        let mut scaling_8x8 = [[0u8; 64]; 2];
        for (raster, zigzag) in scaling_4x4.iter_mut().zip(pps.scaling.four.iter()) {
            for (index, value) in zigzag.iter().enumerate() {
                raster[Self::ZIGZAG_4X4[index]] = *value;
            }
        }
        for (raster, zigzag) in scaling_8x8.iter_mut().zip(pps.scaling.eight.iter()) {
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
    /// corrupt slice size or delta falls outside the parameter range.
    pub fn slice(
        &self,
        slice: &PendingSlice,
        list0: &[usize],
        list1: &[usize],
        sps: &Sps,
        pps: &Pps,
    ) -> Result<VaSliceParameterBufferH264, VaApiError> {
        let header = slice.header();
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        let weights = Weights::try_new(header, sps, pps)?;
        Ok(VaSliceParameterBufferH264 {
            slice_data_size: u32::try_from(slice.data().len()).map_err(|_| range("data size"))?,
            slice_data_offset: 0,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            slice_data_bit_offset: u16::try_from(slice.bit_size())
                .map_err(|_| range("bit offset"))?,
            first_mb_in_slice: u16::try_from(header.first_mb)
                .map_err(|_| range("first macroblock"))?,
            slice_type: header.slice_type.id(),
            direct_spatial_mv_pred_flag: u8::from(header.direct_spatial),
            num_ref_idx_l0_active_minus1: header.num_ref_l0(pps),
            num_ref_idx_l1_active_minus1: header.num_ref_l1(pps),
            cabac_init_idc: header.cabac_init_idc,
            slice_qp_delta: i8::try_from(header.qp_delta).map_err(|_| range("qp delta"))?,
            disable_deblocking_filter_idc: header.deblocking_idc,
            slice_alpha_c0_offset_div2: i8::try_from(header.deblocking_alpha)
                .map_err(|_| range("deblocking alpha"))?,
            slice_beta_offset_div2: i8::try_from(header.deblocking_beta)
                .map_err(|_| range("deblocking beta"))?,
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
            let entry = self
                .dpb
                .entries()
                .get(*index)
                .ok_or(VaApiError::BrokenReference(CapsBroken::MarkingTargetAbsent))?;
            let picture = entry.pic.borrow();
            let Some(backing) = entry.reference.or(entry.decoded_frame) else {
                return Err(VaApiError::BrokenReference(CapsBroken::NamesEmptySlot {
                    reference: "reference",
                }));
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
            .ok_or(VaApiError::BrokenReference(CapsBroken::SlotOutsidePool))?;
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
    ///
    /// Frame-only streams: the frame bit stays set, MBAFF stays clear.
    fn seq_fields(sps: &Sps) -> u32 {
        use crate::ffi::{
            SEQ_CHROMA_FORMAT_IDC, SEQ_DELTA_PIC_ORDER_ALWAYS_ZERO, SEQ_DIRECT_8X8_INFERENCE,
            SEQ_FRAME_MBS_ONLY, SEQ_GAPS_IN_FRAME_NUM, SEQ_LOG2_MAX_FRAME_NUM_MINUS4,
            SEQ_LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4, SEQ_MIN_LUMA_BI_PRED_SIZE_8X8,
            SEQ_PIC_ORDER_CNT_TYPE, SEQ_SEPARATE_COLOUR_PLANE,
        };
        (u32::from(sps.fidelity.chroma_format_idc) << SEQ_CHROMA_FORMAT_IDC)
            | (u32::from(sps.fidelity.separate_plane) << SEQ_SEPARATE_COLOUR_PLANE)
            | (u32::from(sps.gaps_allowed) << SEQ_GAPS_IN_FRAME_NUM)
            | (1u32 << SEQ_FRAME_MBS_ONLY)
            | (u32::from(sps.direct_8x8) << SEQ_DIRECT_8X8_INFERENCE)
            | (u32::from(sps.level_idc >= 31) << SEQ_MIN_LUMA_BI_PRED_SIZE_8X8)
            | (u32::from(sps.log2_max_frame_num_minus4) << SEQ_LOG2_MAX_FRAME_NUM_MINUS4)
            | (u32::from(sps.poc_type.id()) << SEQ_PIC_ORDER_CNT_TYPE)
            | (u32::from(sps.log2_max_poc_lsb_minus4) << SEQ_LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4)
            | (u32::from(sps.delta_zero) << SEQ_DELTA_PIC_ORDER_ALWAYS_ZERO)
    }

    /// Packed picture flags.
    ///
    /// Frame-only streams: the field bit stays clear.
    fn pic_fields(pps: &Pps, pic: &PictureData) -> u32 {
        use crate::ffi::{
            PIC_CONSTRAINED_INTRA_PRED, PIC_DEBLOCKING_FILTER_CONTROL_PRESENT,
            PIC_ENTROPY_CODING_MODE, PIC_ORDER_PRESENT, PIC_REDUNDANT_PIC_CNT_PRESENT,
            PIC_REFERENCE_PIC, PIC_TRANSFORM_8X8_MODE, PIC_WEIGHTED_BIPRED_IDC, PIC_WEIGHTED_PRED,
        };
        (u32::from(pps.entropy()) << PIC_ENTROPY_CODING_MODE)
            | (u32::from(pps.weighted_pred()) << PIC_WEIGHTED_PRED)
            | (u32::from(pps.weighted_bipred_idc) << PIC_WEIGHTED_BIPRED_IDC)
            | (u32::from(pps.transform_8x8()) << PIC_TRANSFORM_8X8_MODE)
            | (u32::from(pps.constrained_intra()) << PIC_CONSTRAINED_INTRA_PRED)
            | (u32::from(pps.pic_order_present()) << PIC_ORDER_PRESENT)
            | (u32::from(pps.deblocking_present()) << PIC_DEBLOCKING_FILTER_CONTROL_PRESENT)
            | (u32::from(pps.redundant_present()) << PIC_REDUNDANT_PIC_CNT_PRESENT)
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
    ///
    /// # Errors
    ///
    /// Returns `Err` when a weight or offset falls outside the
    /// parameter range.
    fn try_new(header: &SliceHeader, sps: &Sps, pps: &Pps) -> Result<Self, VaApiError> {
        let mut weights = Self {
            luma_log2_denom: header.weights.luma_denom,
            chroma_log2_denom: header.weights.chroma_denom,
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
        let luma_default = 1i32 << header.weights.luma_denom;
        let chroma_default = 1i32 << header.weights.chroma_denom;
        let chroma_samples = !sps.fidelity.separate_plane && sps.fidelity.chroma_format_idc != 0;
        let l0 = pps.weighted_pred() && matches!(header.slice_type, SliceType::P);
        let l1 = pps.weighted_bipred_idc == 1 && matches!(header.slice_type, SliceType::B);
        if l0 || l1 {
            weights.luma_l0_flag = 1;
            let mut slots = ListSlots {
                luma_weight: &mut weights.luma_weight_l0,
                luma_offset: &mut weights.luma_offset_l0,
                chroma_weight: &mut weights.chroma_weight_l0,
                chroma_offset: &mut weights.chroma_offset_l0,
            };
            Self::fill_list(
                &mut slots,
                &header.weights.l0,
                usize::from(header.num_ref_l0(pps)) + 1,
                luma_default,
                chroma_default,
                chroma_samples,
            )?;
            if chroma_samples {
                weights.chroma_l0_flag = 1;
            }
        }
        if l1 {
            weights.luma_l1_flag = 1;
            let mut slots = ListSlots {
                luma_weight: &mut weights.luma_weight_l1,
                luma_offset: &mut weights.luma_offset_l1,
                chroma_weight: &mut weights.chroma_weight_l1,
                chroma_offset: &mut weights.chroma_offset_l1,
            };
            Self::fill_list(
                &mut slots,
                &header.weights.l1,
                usize::from(header.num_ref_l1(pps)) + 1,
                luma_default,
                chroma_default,
                chroma_samples,
            )?;
            if chroma_samples {
                weights.chroma_l1_flag = 1;
            }
        }
        Ok(weights)
    }

    /// One weight list into `slots`, defaulting absent entries.
    fn fill_list(
        slots: &mut ListSlots,
        entries: &[rust_video_h264::WeightEntry],
        end: usize,
        luma_default: i32,
        chroma_default: i32,
        chroma_samples: bool,
    ) -> Result<(), VaApiError> {
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        for (index, (weight, offset)) in slots
            .luma_weight
            .iter_mut()
            .zip(slots.luma_offset.iter_mut())
            .enumerate()
            .take(end)
        {
            let (luma_w, luma_o) = entries.get(index).map_or((luma_default, 0), |entry| {
                entry.luma.unwrap_or((luma_default, 0))
            });
            *weight = i16::try_from(luma_w).map_err(|_| range("luma weight"))?;
            *offset = i16::try_from(luma_o).map_err(|_| range("luma offset"))?;
        }
        if !chroma_samples {
            return Ok(());
        }
        for (index, (slot_weights, slot_offsets)) in slots
            .chroma_weight
            .iter_mut()
            .zip(slots.chroma_offset.iter_mut())
            .enumerate()
            .take(end)
        {
            let pairs = entries
                .get(index)
                .map_or([(chroma_default, 0); 2], |entry| {
                    entry.chroma.unwrap_or([(chroma_default, 0); 2])
                });
            for (component, (weight, offset)) in pairs.iter().enumerate() {
                slot_weights[component] =
                    i16::try_from(*weight).map_err(|_| range("chroma weight"))?;
                slot_offsets[component] =
                    i16::try_from(*offset).map_err(|_| range("chroma offset"))?;
            }
        }
        Ok(())
    }
}

/// Mutable weight arrays of one reference list.
struct ListSlots<'a> {
    /// Luma weights by reference index.
    luma_weight: &'a mut [i16; 32],
    /// Luma offsets by reference index.
    luma_offset: &'a mut [i16; 32],
    /// Chroma weights by reference index.
    chroma_weight: &'a mut [[i16; 2]; 32],
    /// Chroma offsets by reference index.
    chroma_offset: &'a mut [[i16; 2]; 32],
}

#[cfg(test)]
mod tests {
    use rust_video_h264::PocType;

    use super::Params;
    use crate::error::VaApiError;
    use crate::fixtures::{pps, slice_header, sps};
    use crate::h264::dpb::Dpb;
    use crate::h264::frontend::{PendingSlice, SurfaceSlot};
    use crate::h264::picture::{PictureData, Reference};
    use crate::h264::SliceHeader;

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
        let mut pic = pps();
        for (index, slot) in pic.scaling.four[0].iter_mut().enumerate() {
            *slot = u8::try_from(index).expect("small list");
        }
        pic.scaling.eight[0] = [0; 64];
        let matrix = Params::matrix(&pic);
        assert_eq!(
            matrix.scaling_list_4x4[0],
            [0, 1, 5, 6, 2, 4, 7, 12, 3, 8, 11, 13, 9, 10, 14, 15]
        );
        assert_eq!(matrix.scaling_list_8x8[0], [0; 64]);
    }

    #[test]
    fn picture_packs_seq_and_pic_words() {
        let sps = sps(PocType::Lsb);
        let pps = pps();
        let dpb: Dpb<SurfaceSlot> = Dpb::default();
        let params = Params::new(&dpb, &[7]);
        let mut pic = PictureData::default();
        pic.nal_ref_idc = 1;
        pic.set_reference(Reference::ShortTerm);
        let header = slice_header();
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
        let sps = sps(PocType::Lsb);
        let pps = pps();
        let mut dpb: Dpb<SurfaceSlot> = Dpb::default();
        dpb.set_limits(16, 16);
        let mut pic = PictureData::default();
        pic.set_reference(Reference::ShortTerm);
        dpb.store_picture(pic.into_rc(), Some(SurfaceSlot::new(3)))
            .expect("dpb room");
        let params = Params::new(&dpb, &[7]);
        let header = slice_header();
        assert!(matches!(
            params.slice(
                &PendingSlice::assembled(header, 64, 0),
                &[0],
                &[],
                &sps,
                &pps
            ),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn ref_list_describes_surface_backed_entries() {
        let sps = sps(PocType::Lsb);
        let pps = pps();
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
        let header = slice_header();
        let raw = params
            .slice(
                &PendingSlice::assembled(header, 64, 0),
                &[0],
                &[],
                &sps,
                &pps,
            )
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
        let sps = sps(PocType::Lsb);
        let pps = pps();
        let dpb: Dpb<SurfaceSlot> = Dpb::default();
        let params = Params::new(&dpb, &[7]);
        let header = slice_header();
        assert!(matches!(
            params.slice(
                &PendingSlice::assembled(header.clone(), 64, usize::from(u16::MAX) + 1),
                &[],
                &[],
                &sps,
                &pps
            ),
            Err(VaApiError::Parse(_))
        ));
        let huge = usize::try_from(u64::from(u32::MAX) + 1).expect("test size");
        assert!(matches!(
            params.slice(
                &PendingSlice::assembled(header, huge, 0),
                &[],
                &[],
                &sps,
                &pps
            ),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn slice_maps_explicit_and_default_weights() {
        use rust_video_h264::{WeightEntry, Weights, PPS_WEIGHTED_PRED};

        let sps = sps(PocType::Lsb);
        let mut pic = pps();
        pic.flags |= PPS_WEIGHTED_PRED;
        let header = SliceHeader {
            weights: Weights {
                luma_denom: 5,
                chroma_denom: 5,
                l0: vec![WeightEntry {
                    luma: Some((40, 3)),
                    chroma: None,
                }],
                l1: Vec::new(),
            },
            ..slice_header()
        };
        let dpb: Dpb<SurfaceSlot> = Dpb::default();
        let params = Params::new(&dpb, &[7]);
        let raw = params
            .slice(
                &PendingSlice::assembled(header, 64, 0),
                &[],
                &[],
                &sps,
                &pic,
            )
            .expect("slice params");
        assert_eq!(raw.luma_weight_l0[0], 40);
        assert_eq!(raw.luma_offset_l0[0], 3);
        assert_eq!(raw.luma_weight_l0[1], 0);
        assert_eq!(raw.chroma_weight_l0[0], [32, 32]);
        assert_eq!(raw.chroma_offset_l0[0], [0, 0]);
    }
}
