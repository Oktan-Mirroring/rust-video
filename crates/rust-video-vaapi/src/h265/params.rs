//! H.265 VA-API parameter buffers from parsed syntax.
//!
//! Portable: pure mapping from sequence, picture, and slice syntax onto
//! the `ffi` layouts; no display handle needed.

use std::os::raw::c_int;

use crate::error::VaApiError;
use crate::ffi::{
    VaIqMatrixBufferHevc, VaPictureHevc, VaPictureParameterBufferHevc, VaSliceParameterBufferHevc,
    VaSurfaceId,
};
use crate::frontend::SurfaceSlot;
use crate::h265::dpb::Dpb;
use crate::h265::frontend::Rps;
use crate::h265::parser::{
    NaluType, Pps, Profile, ScalingLists, SliceHeader, SliceType, Sps, DEFAULT_SCALING_LIST_0,
    DEFAULT_SCALING_LIST_1, DEFAULT_SCALING_LIST_2,
};
use crate::h265::picture::{PictureData, Reference};

/// VA-API parameter builder over the DPB and the surface pool.
pub struct Params<'a> {
    /// Decoded-picture buffer backing reference pictures.
    dpb: &'a Dpb<SurfaceSlot>,
    /// Pool surfaces by slot index.
    surfaces: &'a [VaSurfaceId],
}

impl<'a> Params<'a> {
    /// Up-right-diagonal scan of a 4x4 block, scan index to raster index.
    const SCAN_4X4: [usize; 16] = [0, 4, 1, 8, 5, 2, 12, 9, 6, 3, 13, 10, 7, 14, 11, 15];
    /// Up-right-diagonal scan of an 8x8 block, scan index to raster index.
    const SCAN_8X8: [usize; 64] = [
        0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 32, 25, 18, 11, 4, 40, 33, 26, 19, 12, 5, 48, 41, 34, 27,
        20, 13, 6, 56, 49, 42, 35, 28, 21, 14, 7, 57, 50, 43, 36, 29, 22, 15, 58, 51, 44, 37, 30,
        23, 59, 52, 45, 38, 31, 60, 53, 46, 39, 61, 54, 47, 62, 55, 63,
    ];

    /// Builder over `dpb` with pool `surfaces`.
    #[must_use]
    pub fn new(dpb: &'a Dpb<SurfaceSlot>, surfaces: &'a [VaSurfaceId]) -> Self {
        Self { dpb, surfaces }
    }

    /// VA-API profile for `profile_idc`.
    ///
    /// Main Still Picture decodes under Main: the VA-API HEVC Main
    /// profile covers both still and moving pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` for profiles outside Main and Main Still Picture.
    pub fn profile(profile_idc: u8) -> Result<c_int, VaApiError> {
        let unsupported = || VaApiError::Unsupported(format!("h265 profile {profile_idc}"));
        match Profile::try_from(profile_idc).map_err(|_| unsupported())? {
            Profile::Main | Profile::MainStill => Ok(crate::ffi::VA_PROFILE_HEVC_MAIN),
            _ => Err(unsupported()),
        }
    }

    /// Picture-parameter buffer for `pic` on `surface`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a reference picture lost its surface, when
    /// the DPB holds more references than the buffer fits, or when a
    /// corrupt size falls outside the parameter range.
    pub fn picture(
        &self,
        sps: &Sps,
        pps: &Pps,
        pic: &PictureData,
        rps: &Rps,
        surface: VaSurfaceId,
    ) -> Result<VaPictureParameterBufferHevc, VaApiError> {
        let layer = usize::from(sps.max_sub_layers_minus1);
        let buffering = sps
            .max_dec_pic_buffering_minus1
            .get(layer)
            .copied()
            .ok_or_else(|| VaApiError::Parse("sublayer range".to_owned()))?;
        let range = |what: &str| VaApiError::Parse(format!("picture {what} range"));
        let mut columns = [0u16; 19];
        for (out, width) in columns.iter_mut().zip(pps.column_width_minus1.iter()) {
            *out = u16::try_from(*width).map_err(|_| range("column width"))?;
        }
        let mut rows = [0u16; 21];
        for (out, height) in rows.iter_mut().zip(pps.row_height_minus1.iter()) {
            *out = u16::try_from(*height).map_err(|_| range("row height"))?;
        }
        Ok(VaPictureParameterBufferHevc {
            curr_pic: VaPictureHevc {
                picture_id: surface,
                pic_order_cnt: pic.pic_order_cnt_val,
                flags: 0,
                reserved: [0; 4],
            },
            reference_frames: self.reference_frames(rps)?,
            pic_width_in_luma_samples: sps.pic_width_in_luma_samples,
            pic_height_in_luma_samples: sps.pic_height_in_luma_samples,
            pic_fields: Self::pic_fields(sps, pps),
            sps_max_dec_pic_buffering_minus1: buffering,
            bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
            bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
            pcm_sample_bit_depth_luma_minus1: sps.pcm_sample_bit_depth_luma_minus1,
            pcm_sample_bit_depth_chroma_minus1: sps.pcm_sample_bit_depth_chroma_minus1,
            log2_min_luma_coding_block_size_minus3: sps.log2_min_luma_coding_block_size_minus3,
            log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_luma_coding_block_size,
            log2_min_transform_block_size_minus2: sps.log2_min_luma_transform_block_size_minus2,
            log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_luma_transform_block_size,
            log2_min_pcm_luma_coding_block_size_minus3: sps
                .log2_min_pcm_luma_coding_block_size_minus3,
            log2_diff_max_min_pcm_luma_coding_block_size: sps
                .log2_diff_max_min_pcm_luma_coding_block_size,
            max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
            max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
            init_qp_minus26: pps.init_qp_minus26,
            diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
            pps_cb_qp_offset: pps.cb_qp_offset,
            pps_cr_qp_offset: pps.cr_qp_offset,
            log2_parallel_merge_level_minus2: pps.log2_parallel_merge_level_minus2,
            num_tile_columns_minus1: pps.num_tile_columns_minus1,
            num_tile_rows_minus1: pps.num_tile_rows_minus1,
            column_width_minus1: columns,
            row_height_minus1: rows,
            slice_parsing_fields: Self::slice_parsing_fields(sps, pps, pic),
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
            num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets,
            num_long_term_ref_pic_sps: sps.num_long_term_ref_pics,
            num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
            pps_beta_offset_div2: pps.beta_offset_div2,
            pps_tc_offset_div2: pps.tc_offset_div2,
            num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
            st_rps_bits: pic.short_term_ref_pic_set_size_bits,
            reserved: [0; 8],
        })
    }

    /// Inverse-quantization matrix for `pps`, absent without scaling lists.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt DC coefficient falls outside the
    /// parameter range.
    pub fn matrix(sps: &Sps, pps: &Pps) -> Result<Option<VaIqMatrixBufferHevc>, VaApiError> {
        if !sps.scaling.scaling_list_enabled_flag {
            return Ok(None);
        }
        let mut lists = ScalingLists::default();
        if pps.headers.scaling_list_data_present_flag {
            lists = pps.scaling_list.clone();
        } else if sps.scaling.scaling_list_data_present_flag {
            lists = sps.scaling_list.clone();
        } else {
            lists.list_4x4.fill(DEFAULT_SCALING_LIST_0);
            for (index, list) in lists
                .list_8x8
                .iter_mut()
                .chain(lists.list_16x16.iter_mut())
                .chain(lists.list_32x32.iter_mut())
                .enumerate()
            {
                *list = if index % 6 < 3 {
                    DEFAULT_SCALING_LIST_1
                } else {
                    DEFAULT_SCALING_LIST_2
                };
            }
            lists.dc_coef_minus8_16x16 = [8; 6];
            lists.dc_coef_minus8_32x32 = [8; 6];
        }
        let mut scaling_4x4 = [[0u8; 16]; 6];
        let mut scaling_8x8 = [[0u8; 64]; 6];
        let mut scaling_16x16 = [[0u8; 64]; 6];
        let mut scaling_32x32 = [[0u8; 64]; 2];
        for (raster, scanned) in scaling_4x4.iter_mut().zip(lists.list_4x4.iter()) {
            for (index, value) in scanned.iter().enumerate() {
                raster[Self::SCAN_4X4[index]] = *value;
            }
        }
        for (raster, scanned) in scaling_8x8
            .iter_mut()
            .zip(lists.list_8x8.iter())
            .chain(scaling_16x16.iter_mut().zip(lists.list_16x16.iter()))
        {
            for (index, value) in scanned.iter().enumerate() {
                raster[Self::SCAN_8X8[index]] = *value;
            }
        }
        for (matrix, raster) in scaling_32x32.iter_mut().enumerate() {
            for (index, value) in lists.list_32x32[matrix * 3].iter().enumerate() {
                raster[Self::SCAN_8X8[index]] = *value;
            }
        }
        let mut dc_16x16 = [0u8; 6];
        for (out, dc) in dc_16x16.iter_mut().zip(lists.dc_coef_minus8_16x16.iter()) {
            *out = u8::try_from(dc + 8)
                .map_err(|_| VaApiError::Parse("scaling dc range".to_owned()))?;
        }
        let mut dc_32x32 = [0u8; 2];
        for (matrix, out) in dc_32x32.iter_mut().enumerate() {
            *out = u8::try_from(lists.dc_coef_minus8_32x32[matrix * 3] + 8)
                .map_err(|_| VaApiError::Parse("scaling dc range".to_owned()))?;
        }
        Ok(Some(VaIqMatrixBufferHevc {
            scaling_list_4x4: scaling_4x4,
            scaling_list_8x8: scaling_8x8,
            scaling_list_16x16: scaling_16x16,
            scaling_list_32x32: scaling_32x32,
            scaling_list_dc_16x16: dc_16x16,
            scaling_list_dc_32x32: dc_32x32,
            reserved: [0; 4],
        }))
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
        list0: &[Option<usize>],
        list1: &[Option<usize>],
        sps: &Sps,
        last: bool,
    ) -> Result<VaSliceParameterBufferHevc, VaApiError> {
        let weights = Weights::new(header, sps)?;
        let range = |what: &str| VaApiError::Parse(format!("slice {what} range"));
        Ok(VaSliceParameterBufferHevc {
            slice_data_size: u32::try_from(data_len).map_err(|_| range("data size"))?,
            slice_data_offset: 0,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            slice_data_byte_offset: header.header_bit_size / 8,
            slice_segment_address: header.segment_address,
            ref_pic_list: [self.ref_list(list0)?, self.ref_list(list1)?],
            long_slice_flags: Self::slice_flags(header, last),
            collocated_ref_idx: if header.motion.temporal_mvp_enabled_flag {
                header.collocated_ref_idx
            } else {
                0xff
            },
            num_ref_idx_l0_active_minus1: header.num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1: header.num_ref_idx_l1_active_minus1,
            slice_qp_delta: header.qp_delta,
            slice_cb_qp_offset: header.cb_qp_offset,
            slice_cr_qp_offset: header.cr_qp_offset,
            slice_beta_offset_div2: header.beta_offset_div2,
            slice_tc_offset_div2: header.tc_offset_div2,
            luma_log2_weight_denom: weights.luma_log2_denom,
            delta_chroma_log2_weight_denom: weights.delta_chroma_log2_denom,
            delta_luma_weight_l0: weights.delta_luma_l0,
            luma_offset_l0: weights.luma_offset_l0,
            delta_chroma_weight_l0: weights.delta_chroma_l0,
            chroma_offset_l0: weights.chroma_offset_l0,
            delta_luma_weight_l1: weights.delta_luma_l1,
            luma_offset_l1: weights.luma_offset_l1,
            delta_chroma_weight_l1: weights.delta_chroma_l1,
            chroma_offset_l1: weights.chroma_offset_l1,
            five_minus_max_num_merge_cand: header.five_minus_max_num_merge_cand,
            num_entry_point_offsets: u16::try_from(header.num_entry_point_offsets)
                .map_err(|_| range("entry points"))?,
            entry_offset_to_subset_array: 0,
            slice_data_num_emu_prevn_bytes: u16::try_from(header.n_emulation_prevention_bytes)
                .map_err(|_| range("emulation prevention"))?,
            reserved: [0; 2],
        })
    }

    /// DPB references as VA-API pictures, padded to fifteen.
    fn reference_frames(&self, rps: &Rps) -> Result<[VaPictureHevc; 15], VaApiError> {
        let mut frames = [Self::invalid(); 15];
        let mut count = 0;
        for (index, entry) in self.dpb.entries().iter().enumerate() {
            let picture = entry.0.borrow();
            if !picture.is_ref() {
                continue;
            }
            if count == frames.len() {
                return Err(VaApiError::BrokenReference(
                    "reference frames overflow".to_owned(),
                ));
            }
            frames[count] = self.describe(&picture, entry.1, rps_flags(index, rps))?;
            count += 1;
        }
        Ok(frames)
    }

    /// Reference list as `ReferenceFrames` indices, padded with `0xFF`.
    fn ref_list(&self, indices: &[Option<usize>]) -> Result<[u8; 15], VaApiError> {
        if indices.len() > 15 {
            return Err(VaApiError::BrokenReference(
                "reference list overflow".to_owned(),
            ));
        }
        let mut list = [0xff; 15];
        for (slot, index) in list.iter_mut().zip(indices.iter()) {
            let Some(index) = index else {
                continue;
            };
            *slot = self.ref_position(*index).ok_or_else(|| {
                VaApiError::BrokenReference(format!("dpb index {index} unreferenced"))
            })?;
        }
        Ok(list)
    }

    /// Position of DPB entry `index` within the reference frames.
    fn ref_position(&self, index: usize) -> Option<u8> {
        let mut position = 0;
        for (at, entry) in self.dpb.entries().iter().enumerate() {
            if !entry.0.borrow().is_ref() {
                continue;
            }
            if at == index {
                return u8::try_from(position).ok();
            }
            position += 1;
        }
        None
    }

    /// VA-API descriptor for a DPB picture on `slot` with `rps` flags.
    fn describe(
        &self,
        picture: &PictureData,
        slot: SurfaceSlot,
        rps: u32,
    ) -> Result<VaPictureHevc, VaApiError> {
        let surface = self
            .surfaces
            .get(slot.index() as usize)
            .copied()
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        let mut flags = rps;
        if matches!(picture.reference(), Reference::LongTerm) {
            flags |= crate::ffi::VA_PICTURE_HEVC_LONG_TERM_REFERENCE;
        }
        Ok(VaPictureHevc {
            picture_id: surface,
            pic_order_cnt: picture.pic_order_cnt_val,
            flags,
            reserved: [0; 4],
        })
    }

    /// Invalid VA-API picture marker.
    fn invalid() -> VaPictureHevc {
        VaPictureHevc {
            picture_id: crate::ffi::VA_INVALID_ID,
            pic_order_cnt: 0,
            flags: crate::ffi::VA_PICTURE_HEVC_INVALID,
            reserved: [0; 4],
        }
    }

    /// Packed picture flags.
    fn pic_fields(sps: &Sps, pps: &Pps) -> u32 {
        use crate::ffi::{
            HEVC_AMP_ENABLED, HEVC_CHROMA_FORMAT_IDC, HEVC_CONSTRAINED_INTRA_PRED,
            HEVC_CU_QP_DELTA_ENABLED, HEVC_ENTROPY_CODING_SYNC_ENABLED,
            HEVC_LOOP_FILTER_ACROSS_TILES, HEVC_PCM_ENABLED, HEVC_PCM_LOOP_FILTER_DISABLED,
            HEVC_PPS_LOOP_FILTER_ACROSS_SLICES, HEVC_SCALING_LIST_ENABLED,
            HEVC_SEPARATE_COLOUR_PLANE, HEVC_SIGN_DATA_HIDING_ENABLED,
            HEVC_STRONG_INTRA_SMOOTHING_ENABLED, HEVC_TILES_ENABLED, HEVC_TRANSFORM_SKIP_ENABLED,
            HEVC_TRANSQUANT_BYPASS_ENABLED, HEVC_WEIGHTED_BIPRED, HEVC_WEIGHTED_PRED,
        };
        (u32::from(sps.chroma_format_idc) << HEVC_CHROMA_FORMAT_IDC)
            | (u32::from(sps.format.separate_colour_plane_flag) << HEVC_SEPARATE_COLOUR_PLANE)
            | (u32::from(sps.pcm.pcm_enabled_flag) << HEVC_PCM_ENABLED)
            | (u32::from(sps.scaling.scaling_list_enabled_flag) << HEVC_SCALING_LIST_ENABLED)
            | (u32::from(pps.transform.transform_skip_enabled_flag) << HEVC_TRANSFORM_SKIP_ENABLED)
            | (u32::from(sps.tools.amp_enabled_flag) << HEVC_AMP_ENABLED)
            | (u32::from(sps.tools.strong_intra_smoothing_enabled_flag)
                << HEVC_STRONG_INTRA_SMOOTHING_ENABLED)
            | (u32::from(pps.coding.sign_data_hiding_enabled_flag) << HEVC_SIGN_DATA_HIDING_ENABLED)
            | (u32::from(pps.coding.constrained_intra_pred_flag) << HEVC_CONSTRAINED_INTRA_PRED)
            | (u32::from(pps.transform.cu_qp_delta_enabled_flag) << HEVC_CU_QP_DELTA_ENABLED)
            | (u32::from(pps.prediction.weighted_pred_flag) << HEVC_WEIGHTED_PRED)
            | (u32::from(pps.prediction.weighted_bipred_flag) << HEVC_WEIGHTED_BIPRED)
            | (u32::from(pps.transform.transquant_bypass_enabled_flag)
                << HEVC_TRANSQUANT_BYPASS_ENABLED)
            | (u32::from(pps.tiles.tiles_enabled_flag) << HEVC_TILES_ENABLED)
            | (u32::from(pps.tiles.entropy_coding_sync_enabled_flag)
                << HEVC_ENTROPY_CODING_SYNC_ENABLED)
            | (u32::from(pps.loop_filter.loop_filter_across_slices_enabled_flag)
                << HEVC_PPS_LOOP_FILTER_ACROSS_SLICES)
            | (u32::from(pps.loop_filter.loop_filter_across_tiles_enabled_flag)
                << HEVC_LOOP_FILTER_ACROSS_TILES)
            | (u32::from(sps.pcm.pcm_loop_filter_disabled_flag) << HEVC_PCM_LOOP_FILTER_DISABLED)
    }

    /// Packed slice-parsing flags.
    fn slice_parsing_fields(sps: &Sps, pps: &Pps, pic: &PictureData) -> u32 {
        use crate::ffi::{
            HEVC_CABAC_INIT_PRESENT, HEVC_DEBLOCKING_FILTER_OVERRIDE_ENABLED,
            HEVC_DEPENDENT_SLICE_SEGMENTS_ENABLED, HEVC_IDR_PIC, HEVC_INTRA_PIC,
            HEVC_LISTS_MODIFICATION_PRESENT, HEVC_LONG_TERM_REF_PICS_PRESENT,
            HEVC_OUTPUT_FLAG_PRESENT, HEVC_PPS_DISABLE_DEBLOCKING_FILTER,
            HEVC_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT, HEVC_RAP_PIC,
            HEVC_SAMPLE_ADAPTIVE_OFFSET_ENABLED, HEVC_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT,
            HEVC_SPS_TEMPORAL_MVP_ENABLED,
        };
        let rap = u32::from(
            (NaluType::BlaWLp as u32..=NaluType::CraNut as u32).contains(&(pic.nalu_type as u32)),
        );
        (u32::from(pps.prediction.lists_modification_present_flag)
            << HEVC_LISTS_MODIFICATION_PRESENT)
            | (u32::from(sps.features.long_term_ref_pics_present_flag)
                << HEVC_LONG_TERM_REF_PICS_PRESENT)
            | (u32::from(sps.tools.temporal_mvp_enabled_flag) << HEVC_SPS_TEMPORAL_MVP_ENABLED)
            | (u32::from(pps.coding.cabac_init_present_flag) << HEVC_CABAC_INIT_PRESENT)
            | (u32::from(pps.presence.output_flag_present_flag) << HEVC_OUTPUT_FLAG_PRESENT)
            | (u32::from(pps.presence.dependent_slice_segments_enabled_flag)
                << HEVC_DEPENDENT_SLICE_SEGMENTS_ENABLED)
            | (u32::from(pps.presence.slice_chroma_qp_offsets_present_flag)
                << HEVC_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT)
            | (u32::from(sps.features.sample_adaptive_offset_enabled_flag)
                << HEVC_SAMPLE_ADAPTIVE_OFFSET_ENABLED)
            | (u32::from(pps.deblocking.deblocking_filter_override_enabled_flag)
                << HEVC_DEBLOCKING_FILTER_OVERRIDE_ENABLED)
            | (u32::from(pps.deblocking.deblocking_filter_disabled_flag)
                << HEVC_PPS_DISABLE_DEBLOCKING_FILTER)
            | (u32::from(pps.headers.slice_segment_header_extension_present_flag)
                << HEVC_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT)
            | (rap << HEVC_RAP_PIC)
            | (u32::from(pic.nalu_type.is_idr()) << HEVC_IDR_PIC)
            | (u32::from(pic.nalu_type.is_irap()) << HEVC_INTRA_PIC)
    }

    /// Packed slice flags.
    fn slice_flags(header: &SliceHeader, last: bool) -> u32 {
        use crate::ffi::{
            HEVC_CABAC_INIT, HEVC_COLLOCATED_FROM_L0, HEVC_COLOR_PLANE_ID,
            HEVC_DEPENDENT_SLICE_SEGMENT, HEVC_LAST_SLICE_OF_PIC, HEVC_MVD_L1_ZERO,
            HEVC_SLICE_DEBLOCKING_FILTER_DISABLED, HEVC_SLICE_LOOP_FILTER_ACROSS_SLICES,
            HEVC_SLICE_SAO_CHROMA, HEVC_SLICE_SAO_LUMA, HEVC_SLICE_TEMPORAL_MVP_ENABLED,
            HEVC_SLICE_TYPE,
        };
        (u32::from(last) << HEVC_LAST_SLICE_OF_PIC)
            | (u32::from(header.position.dependent_slice_segment_flag)
                << HEVC_DEPENDENT_SLICE_SEGMENT)
            | ((header.type_ as u32) << HEVC_SLICE_TYPE)
            | (u32::from(header.colour_plane_id) << HEVC_COLOR_PLANE_ID)
            | (u32::from(header.sao.sao_luma_flag) << HEVC_SLICE_SAO_LUMA)
            | (u32::from(header.sao.sao_chroma_flag) << HEVC_SLICE_SAO_CHROMA)
            | (u32::from(header.motion.mvd_l1_zero_flag) << HEVC_MVD_L1_ZERO)
            | (u32::from(header.coding.cabac_init_flag) << HEVC_CABAC_INIT)
            | (u32::from(header.motion.temporal_mvp_enabled_flag)
                << HEVC_SLICE_TEMPORAL_MVP_ENABLED)
            | (u32::from(header.deblocking.deblocking_filter_disabled_flag)
                << HEVC_SLICE_DEBLOCKING_FILTER_DISABLED)
            | (u32::from(header.motion.collocated_from_l0_flag) << HEVC_COLLOCATED_FROM_L0)
            | (u32::from(header.deblocking.loop_filter_across_slices_enabled_flag)
                << HEVC_SLICE_LOOP_FILTER_ACROSS_SLICES)
    }
}

/// RPS flags of DPB entry `index` within `rps`.
fn rps_flags(index: usize, rps: &Rps) -> u32 {
    if rps.st_curr_before().contains(&Some(index)) {
        crate::ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE
    } else if rps.st_curr_after().contains(&Some(index)) {
        crate::ffi::VA_PICTURE_HEVC_RPS_ST_CURR_AFTER
    } else if rps.lt_curr().contains(&Some(index)) {
        crate::ffi::VA_PICTURE_HEVC_RPS_LT_CURR
    } else {
        0
    }
}

/// Prediction weights for one slice.
struct Weights {
    /// Luma denominator.
    luma_log2_denom: u8,
    /// Chroma denominator delta.
    delta_chroma_log2_denom: i8,
    /// List-0 luma weight deltas.
    delta_luma_l0: [i8; 15],
    /// List-0 luma offsets.
    luma_offset_l0: [i8; 15],
    /// List-0 chroma weight deltas.
    delta_chroma_l0: [[i8; 2]; 15],
    /// List-0 chroma offsets.
    chroma_offset_l0: [[i8; 2]; 15],
    /// List-1 luma weight deltas.
    delta_luma_l1: [i8; 15],
    /// List-1 luma offsets.
    luma_offset_l1: [i8; 15],
    /// List-1 chroma weight deltas.
    delta_chroma_l1: [[i8; 2]; 15],
    /// List-1 chroma offsets.
    chroma_offset_l1: [[i8; 2]; 15],
}

impl Weights {
    /// Weights for `header` under `sps`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt denominator falls outside the
    /// parameter range.
    fn new(header: &SliceHeader, sps: &Sps) -> Result<Self, VaApiError> {
        let table = &header.pred_weight_table;
        if table.chroma_log2_weight_denom > 7 {
            return Err(VaApiError::Parse("weight denom range".to_owned()));
        }
        let mut weights = Self {
            luma_log2_denom: table.luma_log2_weight_denom,
            delta_chroma_log2_denom: table.delta_chroma_log2_weight_denom,
            delta_luma_l0: table.delta_luma_weight_l0,
            luma_offset_l0: table.luma_offset_l0,
            delta_chroma_l0: table.delta_chroma_weight_l0,
            chroma_offset_l0: [[0; 2]; 15],
            delta_luma_l1: [0; 15],
            luma_offset_l1: [0; 15],
            delta_chroma_l1: [[0; 2]; 15],
            chroma_offset_l1: [[0; 2]; 15],
        };
        let half = i32::try_from(sps.wp_offset_half_range_c)
            .map_err(|_| VaApiError::Parse("weight offset range".to_owned()))?;
        for index in 0..15 {
            for component in 0..2 {
                weights.chroma_offset_l0[index][component] = Self::chroma_offset(
                    half,
                    table.chroma_log2_weight_denom,
                    table.delta_chroma_weight_l0[index][component],
                    table.delta_chroma_offset_l0[index][component],
                )?;
            }
        }
        if matches!(header.type_, SliceType::B) {
            weights.delta_luma_l1 = table.delta_luma_weight_l1;
            weights.luma_offset_l1 = table.luma_offset_l1;
            weights.delta_chroma_l1 = table.delta_chroma_weight_l1;
            for index in 0..15 {
                for component in 0..2 {
                    weights.chroma_offset_l1[index][component] = Self::chroma_offset(
                        half,
                        table.chroma_log2_weight_denom,
                        table.delta_chroma_weight_l1[index][component],
                        table.delta_chroma_offset_l1[index][component],
                    )?;
                }
            }
        }
        Ok(weights)
    }

    /// Derived chroma offset from the weight delta and offset delta.
    fn chroma_offset(
        half: i32,
        denom: u8,
        delta_weight: i8,
        delta_offset: i16,
    ) -> Result<i8, VaApiError> {
        let weight = (1 << denom) + i32::from(delta_weight);
        let offset = half + i32::from(delta_offset) - ((half * weight) >> denom);
        i8::try_from(offset.clamp(-half, half - 1))
            .map_err(|_| VaApiError::Parse("weight offset range".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::Params;
    use crate::error::VaApiError;
    use crate::frontend::SurfaceSlot;
    use crate::h265::dpb::Dpb;
    use crate::h265::fixture::{nal, pps_payload, sps_payload};
    use crate::h265::frontend::Rps;
    use crate::h265::parser::{
        Nalu, NaluType, Parser, Pps, PredWeightTable, SliceHeader, SliceMotion, SliceType, Sps,
    };
    use crate::h265::picture::{PictureData, Reference};

    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    /// Parsed fixture sequence and picture sets.
    fn sets() -> (Rc<Sps>, Rc<Pps>) {
        let mut parser = Parser::default();
        let mut stream = nal(NaluType::SpsNut, &sps_payload());
        stream.extend(nal(NaluType::PpsNut, &pps_payload()));
        let mut cursor = Cursor::new(stream.as_slice());
        let sps = Nalu::next(&mut cursor).expect("sps nal");
        parser.parse_sps(&sps).expect("sps parses");
        let pps = Nalu::next(&mut cursor).expect("pps nal");
        parser.parse_pps(&pps).expect("pps parses");
        (
            parser.get_sps(0).expect("sps stored").clone(),
            parser.get_pps(0).expect("pps stored").clone(),
        )
    }

    /// DPB holding `pocs` as short-term references on matching slots.
    fn dpb_with(pocs: &[i32]) -> Dpb<SurfaceSlot> {
        let mut dpb = Dpb::default();
        dpb.set_max_num_pics(16);
        for (index, poc) in pocs.iter().enumerate() {
            let mut pic = PictureData::default();
            pic.pic_order_cnt_val = *poc;
            pic.output.pic_output_flag = true;
            dpb.store_picture(
                Rc::new(RefCell::new(pic)),
                SurfaceSlot::new(u32::try_from(index).expect("small dpb")),
            )
            .expect("dpb stores");
        }
        dpb
    }

    #[test]
    fn profile_maps_main_and_still() {
        assert_eq!(
            Params::profile(1).expect("main"),
            crate::ffi::VA_PROFILE_HEVC_MAIN
        );
        assert_eq!(
            Params::profile(3).expect("still"),
            crate::ffi::VA_PROFILE_HEVC_MAIN
        );
        assert!(matches!(
            Params::profile(2),
            Err(VaApiError::Unsupported(_))
        ));
    }

    #[test]
    fn scan_tables_cover_every_raster_slot() {
        let mut seen_4x4 = [false; 16];
        for slot in Params::SCAN_4X4 {
            assert!(!seen_4x4[slot], "duplicate {slot}");
            seen_4x4[slot] = true;
        }
        let mut seen_8x8 = [false; 64];
        for slot in Params::SCAN_8X8 {
            assert!(!seen_8x8[slot], "duplicate {slot}");
            seen_8x8[slot] = true;
        }
        assert_eq!(Params::SCAN_4X4[1], 4);
        assert_eq!(Params::SCAN_8X8[1], 8);
    }

    #[test]
    fn matrix_absent_without_scaling_lists() {
        let (sps, pps) = sets();
        assert!(Params::matrix(&sps, &pps).expect("no matrix").is_none());
    }

    #[test]
    fn matrix_defaults_match_spec_tables() {
        let (sps, pps) = sets();
        let mut enabled = (*sps).clone();
        enabled.scaling.scaling_list_enabled_flag = true;
        let matrix = Params::matrix(&enabled, &pps)
            .expect("default matrix")
            .expect("matrix present");
        assert_eq!(matrix.scaling_list_4x4[0], [16; 16]);
        assert_eq!(
            matrix.scaling_list_8x8[0],
            [
                16, 16, 16, 16, 17, 18, 21, 24, 16, 16, 16, 16, 17, 19, 22, 25, 16, 16, 17, 18, 20,
                22, 25, 29, 16, 16, 18, 21, 24, 27, 31, 36, 17, 17, 20, 24, 30, 35, 41, 47, 18, 19,
                22, 27, 35, 44, 54, 65, 21, 22, 25, 31, 41, 54, 70, 88, 24, 25, 29, 36, 47, 65, 88,
                115,
            ]
        );
        assert_eq!(
            matrix.scaling_list_8x8[3],
            [
                16, 16, 16, 16, 17, 18, 20, 24, 16, 16, 16, 17, 18, 20, 24, 25, 16, 16, 17, 18, 20,
                24, 25, 28, 16, 17, 18, 20, 24, 25, 28, 33, 17, 18, 20, 24, 25, 28, 33, 41, 18, 20,
                24, 25, 28, 33, 41, 54, 20, 24, 25, 28, 33, 41, 54, 71, 24, 25, 28, 33, 41, 54, 71,
                91,
            ]
        );
        assert_eq!(matrix.scaling_list_dc_16x16, [16; 6]);
        assert_eq!(matrix.scaling_list_dc_32x32, [16; 2]);
        assert_eq!(matrix.scaling_list_32x32[0], matrix.scaling_list_8x8[0]);
        assert_eq!(matrix.scaling_list_32x32[1], matrix.scaling_list_8x8[3]);
    }

    #[test]
    fn matrix_prefers_picture_set_and_unscans() {
        let (sps, pps) = sets();
        let mut enabled = (*sps).clone();
        enabled.scaling.scaling_list_enabled_flag = true;
        enabled.scaling.scaling_list_data_present_flag = true;
        enabled.scaling_list.list_4x4[0][1] = 9;
        let mut pps = (*pps).clone();
        pps.headers.scaling_list_data_present_flag = true;
        pps.scaling_list.list_4x4[0][1] = 7;
        let matrix = Params::matrix(&enabled, &pps)
            .expect("pps matrix")
            .expect("matrix present");
        assert_eq!(matrix.scaling_list_4x4[0][Params::SCAN_4X4[1]], 7);
    }

    #[test]
    fn matrix_rejects_dc_range() {
        let (sps, pps) = sets();
        let mut enabled = (*sps).clone();
        enabled.scaling.scaling_list_enabled_flag = true;
        let mut pps = (*pps).clone();
        pps.headers.scaling_list_data_present_flag = true;
        pps.scaling_list.dc_coef_minus8_16x16[0] = 248;
        assert!(matches!(
            Params::matrix(&enabled, &pps),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn picture_maps_references_and_flags() {
        let (sps, pps) = sets();
        let dpb = dpb_with(&[0, 2]);
        dpb.entries()[1]
            .0
            .borrow_mut()
            .set_reference(Reference::LongTerm);
        let surfaces = [100, 101, 102];
        let params = Params::new(&dpb, &surfaces);
        let rps = Rps::from_lists(vec![Some(0)], vec![], vec![Some(1)]);
        let mut pic = PictureData::default();
        pic.pic_order_cnt_val = 4;
        let picture = params
            .picture(&sps, &pps, &pic, &rps, 102)
            .expect("picture maps");
        assert_eq!(picture.curr_pic.picture_id, 102);
        assert_eq!(picture.curr_pic.pic_order_cnt, 4);
        assert_eq!(picture.curr_pic.flags, 0);
        assert_eq!(picture.reference_frames[0].picture_id, 100);
        assert_eq!(picture.reference_frames[0].pic_order_cnt, 0);
        assert_eq!(
            picture.reference_frames[0].flags,
            crate::ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE
        );
        assert_eq!(picture.reference_frames[1].picture_id, 101);
        assert_eq!(
            picture.reference_frames[1].flags,
            crate::ffi::VA_PICTURE_HEVC_LONG_TERM_REFERENCE
                | crate::ffi::VA_PICTURE_HEVC_RPS_LT_CURR
        );
        assert_eq!(
            picture.reference_frames[2].flags,
            crate::ffi::VA_PICTURE_HEVC_INVALID
        );
        assert_eq!(picture.pic_width_in_luma_samples, 64);
        assert_eq!(picture.pic_height_in_luma_samples, 64);
        assert_eq!(picture.pic_fields & 0x3, 1);
    }

    #[test]
    fn picture_rejects_reference_overflow() {
        let (sps, pps) = sets();
        let pocs: Vec<i32> = (0..16).collect();
        let dpb = dpb_with(&pocs);
        let surfaces: Vec<u32> = (0..20).collect();
        let params = Params::new(&dpb, &surfaces);
        let rps = Rps::from_lists(vec![], vec![], vec![]);
        let pic = PictureData::default();
        assert!(matches!(
            params.picture(&sps, &pps, &pic, &rps, 0),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn picture_rejects_lost_surface() {
        let (sps, pps) = sets();
        let dpb = dpb_with(&[0]);
        let params = Params::new(&dpb, &[]);
        let rps = Rps::from_lists(vec![Some(0)], vec![], vec![]);
        let pic = PictureData::default();
        assert!(matches!(
            params.picture(&sps, &pps, &pic, &rps, 0),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn slice_maps_weights_and_lists() {
        let (sps, _) = sets();
        let dpb = dpb_with(&[0, 2]);
        let surfaces = [50, 51];
        let params = Params::new(&dpb, &surfaces);
        let mut header = SliceHeader {
            type_: SliceType::P,
            header_bit_size: 80,
            pred_weight_table: PredWeightTable {
                chroma_log2_weight_denom: 2,
                ..PredWeightTable::default()
            },
            ..SliceHeader::default()
        };
        header.pred_weight_table.delta_chroma_offset_l0[0][0] = 10;
        header.pred_weight_table.delta_chroma_offset_l0[0][1] = 200;
        let slice = params
            .slice(&header, 64, &[Some(1), None], &[], &sps, true)
            .expect("slice maps");
        assert_eq!(slice.slice_data_size, 64);
        assert_eq!(slice.slice_data_byte_offset, 10);
        assert_eq!(slice.long_slice_flags & 0x1, 1);
        assert_eq!(slice.collocated_ref_idx, 0xff);
        assert_eq!(slice.ref_pic_list[0][0], 1);
        assert_eq!(slice.ref_pic_list[0][1], 0xff);
        assert_eq!(slice.ref_pic_list[1][0], 0xff);
        assert_eq!(slice.chroma_offset_l0[0], [10, 127]);
        let header = SliceHeader {
            type_: SliceType::B,
            motion: SliceMotion {
                temporal_mvp_enabled_flag: true,
                ..SliceMotion::default()
            },
            collocated_ref_idx: 1,
            ..SliceHeader::default()
        };
        let slice = params
            .slice(&header, 64, &[], &[], &sps, false)
            .expect("b slice maps");
        assert_eq!(slice.collocated_ref_idx, 1);
        assert_eq!(slice.long_slice_flags & 0x1, 0);
    }

    #[test]
    fn slice_rejects_bad_denom_and_overflow() {
        let (sps, _) = sets();
        let dpb = dpb_with(&[0]);
        let surfaces = [50];
        let params = Params::new(&dpb, &surfaces);
        let mut header = SliceHeader {
            type_: SliceType::P,
            pred_weight_table: PredWeightTable {
                chroma_log2_weight_denom: 8,
                ..PredWeightTable::default()
            },
            ..SliceHeader::default()
        };
        assert!(matches!(
            params.slice(&header, 64, &[], &[], &sps, true),
            Err(VaApiError::Parse(_))
        ));
        header.pred_weight_table.chroma_log2_weight_denom = 0;
        let long = vec![Some(0); 16];
        assert!(matches!(
            params.slice(&header, 64, &long, &[], &sps, true),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn weights_derive_chroma_offsets() {
        let (sps, _) = sets();
        let mut header = SliceHeader {
            type_: SliceType::P,
            pred_weight_table: PredWeightTable {
                chroma_log2_weight_denom: 2,
                ..PredWeightTable::default()
            },
            ..SliceHeader::default()
        };
        header.pred_weight_table.delta_chroma_weight_l0[0] = [1, -1];
        header.pred_weight_table.delta_chroma_offset_l0[0] = [0, 0];
        let slice = Params::new(&dpb_with(&[]), &[])
            .slice(&header, 8, &[], &[], &sps, true)
            .expect("weights derive");
        assert_eq!(slice.delta_chroma_weight_l0[0], [1, -1]);
        assert_eq!(slice.chroma_offset_l0[0], [-32, 32]);
    }
}
