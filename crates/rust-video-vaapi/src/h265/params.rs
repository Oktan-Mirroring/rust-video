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
use crate::h264::frontend::SurfaceSlot;
use crate::h265::dpb::Dpb;
use crate::h265::frontend::{PendingSlice, Rps};
use crate::h265::picture::{PictureData, Reference};
use crate::h265::{is_idr, is_irap, Pps, SliceHeader, SliceTail, SliceType, Sps};

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
        match profile_idc {
            1 | 3 => Ok(crate::ffi::VA_PROFILE_HEVC_MAIN),
            _other => Err(VaApiError::Unsupported(CapsUnsupported::ProfileOutsideMain)),
        }
    }

    /// VA-API profile for `sps`, honoring Main compatibility flags.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `sps` neither names nor declares compatibility with Main.
    pub fn sps_profile(sps: &Sps) -> Result<c_int, VaApiError> {
        // Compatibility flags pack into bits 31..0 for profiles 0..32.
        if sps.ptl.compat & (1u32 << 30) != 0 || sps.ptl.compat & (1u32 << 28) != 0 {
            return Ok(crate::ffi::VA_PROFILE_HEVC_MAIN);
        }
        Self::profile(sps.ptl.profile_idc)
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
        let buffering = sps.ordering_buffering.last().copied().unwrap_or(0);
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        let (columns, rows) = Self::tile_grid(sps, pps, &range)?;
        Ok(VaPictureParameterBufferHevc {
            curr_pic: VaPictureHevc {
                picture_id: surface,
                pic_order_cnt: pic.pic_order_cnt_val,
                flags: 0,
                reserved: [0; 4],
            },
            reference_frames: self.reference_frames(rps)?,
            pic_width_in_luma_samples: u16::try_from(sps.width)
                .map_err(|_| range("coded width"))?,
            pic_height_in_luma_samples: u16::try_from(sps.height)
                .map_err(|_| range("coded height"))?,
            pic_fields: Self::pic_fields(sps, pps),
            sps_max_dec_pic_buffering_minus1: buffering,
            bit_depth_luma_minus8: sps.bit_depth_luma,
            bit_depth_chroma_minus8: sps.bit_depth_chroma,
            pcm_sample_bit_depth_luma_minus1: sps.pcm_depth_luma,
            pcm_sample_bit_depth_chroma_minus1: sps.pcm_depth_chroma,
            log2_min_luma_coding_block_size_minus3: sps.log2_min_cb_size_minus3,
            log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_cb_size,
            log2_min_transform_block_size_minus2: sps.log2_min_tb_size_minus2,
            log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_tb_size,
            log2_min_pcm_luma_coding_block_size_minus3: sps.pcm_log2_min_minus3,
            log2_diff_max_min_pcm_luma_coding_block_size: sps.pcm_log2_diff,
            max_transform_hierarchy_depth_intra: sps.max_transform_depth_intra,
            max_transform_hierarchy_depth_inter: sps.max_transform_depth_inter,
            init_qp_minus26: pps.pic_init_qp_minus26,
            diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
            pps_cb_qp_offset: pps.cb_qp_offset,
            pps_cr_qp_offset: pps.cr_qp_offset,
            log2_parallel_merge_level_minus2: pps.log2_parallel_merge_minus2,
            num_tile_columns_minus1: pps.num_tile_columns.saturating_sub(1),
            num_tile_rows_minus1: pps.num_tile_rows.saturating_sub(1),
            column_width_minus1: columns,
            row_height_minus1: rows,
            slice_parsing_fields: Self::slice_parsing_fields(sps, pps, pic),
            log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_poc_lsb_minus4,
            num_short_term_ref_pic_sets: u8::try_from(sps.rps.len())
                .map_err(|_| range("rps count"))?,
            num_long_term_ref_pic_sps: u8::try_from(sps.lt_poc_lsb.len())
                .map_err(|_| range("long-term count"))?,
            num_ref_idx_l0_default_active_minus1: pps.num_ref_l0,
            num_ref_idx_l1_default_active_minus1: pps.num_ref_l1,
            pps_beta_offset_div2: pps.beta_offset_div2,
            pps_tc_offset_div2: pps.tc_offset_div2,
            num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
            st_rps_bits: pic.short_term_ref_pic_set_size_bits,
            reserved: [0; 8],
        })
    }

    /// Tile extents as minus-one widths, uniform-derived or explicit.
    fn tile_grid(
        sps: &Sps,
        pps: &Pps,
        range: &impl Fn(&'static str) -> VaApiError,
    ) -> Result<([u16; 19], [u16; 21]), VaApiError> {
        let mut columns = [0u16; 19];
        let mut rows = [0u16; 21];
        if !pps.tiles_enabled() {
            return Ok((columns, rows));
        }
        if pps.uniform_spacing() {
            let ncols = u32::from(pps.num_tile_columns);
            let nrows = u32::from(pps.num_tile_rows);
            let width_ctbs = sps.width_in_ctbs();
            let height_ctbs = sps.height_in_ctbs();
            for (index, out) in columns
                .iter_mut()
                .enumerate()
                .take(usize::from(pps.num_tile_columns))
            {
                let j = u32::try_from(index).map_err(|_| range("column width"))?;
                let minus1 = ((j + 1) * width_ctbs) / ncols - j * width_ctbs / ncols - 1;
                *out = u16::try_from(minus1).map_err(|_| range("column width"))?;
            }
            for (index, out) in rows
                .iter_mut()
                .enumerate()
                .take(usize::from(pps.num_tile_rows))
            {
                let j = u32::try_from(index).map_err(|_| range("row height"))?;
                let minus1 = ((j + 1) * height_ctbs) / nrows - j * height_ctbs / nrows - 1;
                *out = u16::try_from(minus1).map_err(|_| range("row height"))?;
            }
            return Ok((columns, rows));
        }
        for (out, width) in columns.iter_mut().zip(pps.column_width.iter()) {
            *out = width.saturating_sub(1);
        }
        for (out, height) in rows.iter_mut().zip(pps.row_height.iter()) {
            *out = height.saturating_sub(1);
        }
        Ok((columns, rows))
    }

    /// Inverse-quantization matrix for `pps`, absent without scaling lists.
    #[must_use]
    pub fn matrix(sps: &Sps, pps: &Pps) -> Option<VaIqMatrixBufferHevc> {
        if !sps.scaling_enabled() {
            return None;
        }
        let lists = &pps.scaling;
        let mut scaling_4x4 = [[0u8; 16]; 6];
        let mut scaling_8x8 = [[0u8; 64]; 6];
        let mut scaling_16x16 = [[0u8; 64]; 6];
        let mut scaling_32x32 = [[0u8; 64]; 2];
        for (raster, scanned) in scaling_4x4.iter_mut().zip(lists.size0.iter()) {
            for (index, value) in scanned.iter().enumerate() {
                raster[Self::SCAN_4X4[index]] = *value;
            }
        }
        for (raster, scanned) in scaling_8x8
            .iter_mut()
            .zip(lists.size1.iter())
            .chain(scaling_16x16.iter_mut().zip(lists.size2.iter()))
        {
            for (index, value) in scanned.iter().enumerate() {
                raster[Self::SCAN_8X8[index]] = *value;
            }
        }
        for (raster, scanned) in scaling_32x32.iter_mut().zip(lists.size3.iter()) {
            for (index, value) in scanned.iter().enumerate() {
                raster[Self::SCAN_8X8[index]] = *value;
            }
        }
        Some(VaIqMatrixBufferHevc {
            scaling_list_4x4: scaling_4x4,
            scaling_list_8x8: scaling_8x8,
            scaling_list_16x16: scaling_16x16,
            scaling_list_32x32: scaling_32x32,
            scaling_list_dc_16x16: lists.dc2,
            scaling_list_dc_32x32: lists.dc3,
            reserved: [0; 4],
        })
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
        list0: &[Option<usize>],
        list1: &[Option<usize>],
        last: bool,
    ) -> Result<VaSliceParameterBufferHevc, VaApiError> {
        let header = slice.header();
        let tail = slice.tail();
        let weights = Weights::try_new(tail, header.slice_type)?;
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        Ok(VaSliceParameterBufferHevc {
            slice_data_size: u32::try_from(slice.data().len()).map_err(|_| range("data size"))?,
            slice_data_offset: 0,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            slice_data_byte_offset: u32::try_from(slice.bit_size() / 8)
                .map_err(|_| range("byte offset"))?,
            slice_segment_address: header.address,
            ref_pic_list: [self.ref_list(list0)?, self.ref_list(list1)?],
            long_slice_flags: Self::slice_flags(header, tail, last),
            collocated_ref_idx: if tail.temporal_mvp {
                tail.collocated_ref_idx
            } else {
                0xff
            },
            num_ref_idx_l0_active_minus1: tail.num_ref_l0,
            num_ref_idx_l1_active_minus1: tail.num_ref_l1,
            slice_qp_delta: i8::try_from(tail.qp_delta).map_err(|_| range("qp delta"))?,
            slice_cb_qp_offset: i8::try_from(tail.cb_qp_offset)
                .map_err(|_| range("cb qp offset"))?,
            slice_cr_qp_offset: i8::try_from(tail.cr_qp_offset)
                .map_err(|_| range("cr qp offset"))?,
            slice_beta_offset_div2: i8::try_from(tail.beta_offset)
                .map_err(|_| range("beta offset"))?,
            slice_tc_offset_div2: i8::try_from(tail.tc_offset).map_err(|_| range("tc offset"))?,
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
            five_minus_max_num_merge_cand: tail.merge_cand_minus5,
            num_entry_point_offsets: u16::try_from(header.entry_count)
                .map_err(|_| range("entry points"))?,
            entry_offset_to_subset_array: 0,
            slice_data_num_emu_prevn_bytes: u16::try_from(slice.epb())
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
                return Err(VaApiError::BrokenReference(CapsBroken::SlotOutsidePool));
            }
            frames[count] = self.describe(&picture, entry.1, rps_flags(index, rps))?;
            count += 1;
        }
        Ok(frames)
    }

    /// Reference list as `ReferenceFrames` indices, padded with `0xFF`.
    fn ref_list(&self, indices: &[Option<usize>]) -> Result<[u8; 15], VaApiError> {
        if indices.len() > 15 {
            return Err(VaApiError::BrokenReference(CapsBroken::SlotOutsidePool));
        }
        let mut list = [0xff; 15];
        for (slot, index) in list.iter_mut().zip(indices.iter()) {
            let Some(index) = index else {
                continue;
            };
            *slot = self
                .ref_position(*index)
                .ok_or(VaApiError::BrokenReference(CapsBroken::MarkingTargetAbsent))?;
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
            .ok_or(VaApiError::BrokenReference(CapsBroken::SlotOutsidePool))?;
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
        let loop_across_tiles = if pps.tiles_enabled() {
            pps.loop_across_tiles()
        } else {
            true
        };
        (u32::from(sps.chroma_format) << HEVC_CHROMA_FORMAT_IDC)
            | (u32::from(sps.separate_colour_plane) << HEVC_SEPARATE_COLOUR_PLANE)
            | (u32::from(sps.pcm()) << HEVC_PCM_ENABLED)
            | (u32::from(sps.scaling_enabled()) << HEVC_SCALING_LIST_ENABLED)
            | (u32::from(pps.transform_skip()) << HEVC_TRANSFORM_SKIP_ENABLED)
            | (u32::from(sps.amp()) << HEVC_AMP_ENABLED)
            | (u32::from(sps.smoothing()) << HEVC_STRONG_INTRA_SMOOTHING_ENABLED)
            | (u32::from(pps.sign_hiding()) << HEVC_SIGN_DATA_HIDING_ENABLED)
            | (u32::from(pps.constrained_intra()) << HEVC_CONSTRAINED_INTRA_PRED)
            | (u32::from(pps.cu_qp_delta()) << HEVC_CU_QP_DELTA_ENABLED)
            | (u32::from(pps.weighted_pred()) << HEVC_WEIGHTED_PRED)
            | (u32::from(pps.weighted_bipred()) << HEVC_WEIGHTED_BIPRED)
            | (u32::from(pps.transquant_bypass()) << HEVC_TRANSQUANT_BYPASS_ENABLED)
            | (u32::from(pps.tiles_enabled()) << HEVC_TILES_ENABLED)
            | (u32::from(pps.entropy_sync()) << HEVC_ENTROPY_CODING_SYNC_ENABLED)
            | (u32::from(pps.loop_across_slices()) << HEVC_PPS_LOOP_FILTER_ACROSS_SLICES)
            | (u32::from(loop_across_tiles) << HEVC_LOOP_FILTER_ACROSS_TILES)
            | (u32::from(sps.pcm_loop_disabled()) << HEVC_PCM_LOOP_FILTER_DISABLED)
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
        let rap = u32::from((16..=21).contains(&pic.nalu_type));
        (u32::from(pps.lists_modification_present()) << HEVC_LISTS_MODIFICATION_PRESENT)
            | (u32::from(sps.long_term_present()) << HEVC_LONG_TERM_REF_PICS_PRESENT)
            | (u32::from(sps.temporal_mvp()) << HEVC_SPS_TEMPORAL_MVP_ENABLED)
            | (u32::from(pps.cabac_init_present()) << HEVC_CABAC_INIT_PRESENT)
            | (u32::from(pps.output_flag_present()) << HEVC_OUTPUT_FLAG_PRESENT)
            | (u32::from(pps.dependent_slices()) << HEVC_DEPENDENT_SLICE_SEGMENTS_ENABLED)
            | (u32::from(pps.chroma_offsets_present()) << HEVC_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT)
            | (u32::from(sps.sao()) << HEVC_SAMPLE_ADAPTIVE_OFFSET_ENABLED)
            | (u32::from(pps.deblocking_override()) << HEVC_DEBLOCKING_FILTER_OVERRIDE_ENABLED)
            | (u32::from(pps.deblocking_disabled()) << HEVC_PPS_DISABLE_DEBLOCKING_FILTER)
            | (u32::from(pps.slice_ext_present()) << HEVC_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT)
            | (rap << HEVC_RAP_PIC)
            | (u32::from(is_idr(pic.nalu_type)) << HEVC_IDR_PIC)
            | (u32::from(is_irap(pic.nalu_type)) << HEVC_INTRA_PIC)
    }

    /// Packed slice flags.
    ///
    /// Dependent segments decline at ingest, so the dependent bit stays clear.
    fn slice_flags(header: &SliceHeader, tail: &SliceTail, last: bool) -> u32 {
        use crate::ffi::{
            HEVC_CABAC_INIT, HEVC_COLLOCATED_FROM_L0, HEVC_COLOR_PLANE_ID, HEVC_LAST_SLICE_OF_PIC,
            HEVC_MVD_L1_ZERO, HEVC_SLICE_DEBLOCKING_FILTER_DISABLED,
            HEVC_SLICE_LOOP_FILTER_ACROSS_SLICES, HEVC_SLICE_SAO_CHROMA, HEVC_SLICE_SAO_LUMA,
            HEVC_SLICE_TEMPORAL_MVP_ENABLED, HEVC_SLICE_TYPE,
        };
        let slice_type = match header.slice_type {
            SliceType::B => 0,
            SliceType::P => 1,
            SliceType::I => 2,
        };
        (u32::from(last) << HEVC_LAST_SLICE_OF_PIC)
            | (slice_type << HEVC_SLICE_TYPE)
            | (u32::from(tail.colour_plane) << HEVC_COLOR_PLANE_ID)
            | (u32::from(tail.sao_luma) << HEVC_SLICE_SAO_LUMA)
            | (u32::from(tail.sao_chroma) << HEVC_SLICE_SAO_CHROMA)
            | (u32::from(tail.mvd_l1_zero) << HEVC_MVD_L1_ZERO)
            | (u32::from(tail.cabac_init) << HEVC_CABAC_INIT)
            | (u32::from(tail.temporal_mvp) << HEVC_SLICE_TEMPORAL_MVP_ENABLED)
            | (u32::from(tail.deblocking_disabled) << HEVC_SLICE_DEBLOCKING_FILTER_DISABLED)
            | (u32::from(tail.collocated_from_l0) << HEVC_COLLOCATED_FROM_L0)
            | (u32::from(tail.loop_filter_across) << HEVC_SLICE_LOOP_FILTER_ACROSS_SLICES)
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
    /// Weights for `tail` on a `slice_type` slice.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt denominator or delta falls outside
    /// the parameter range.
    fn try_new(tail: &SliceTail, slice_type: SliceType) -> Result<Self, VaApiError> {
        let range = |what: &'static str| VaApiError::Parse(CapsParse::Range { syntax: what });
        let chroma_denom = i32::from(tail.weights.luma_denom) + tail.weights.delta_chroma_denom;
        if !(0..=7).contains(&chroma_denom) {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "weight denom",
            }));
        }
        let chroma_denom = u8::try_from(chroma_denom).map_err(|_| {
            VaApiError::Parse(CapsParse::Range {
                syntax: "weight denom",
            })
        })?;
        let mut weights = Self {
            luma_log2_denom: tail.weights.luma_denom,
            delta_chroma_log2_denom: i8::try_from(tail.weights.delta_chroma_denom)
                .map_err(|_| range("chroma denom"))?,
            delta_luma_l0: [0; 15],
            luma_offset_l0: [0; 15],
            delta_chroma_l0: [[0; 2]; 15],
            chroma_offset_l0: [[0; 2]; 15],
            delta_luma_l1: [0; 15],
            luma_offset_l1: [0; 15],
            delta_chroma_l1: [[0; 2]; 15],
            chroma_offset_l1: [[0; 2]; 15],
        };
        Self::fill_list(
            &mut weights.delta_luma_l0,
            &mut weights.luma_offset_l0,
            &mut weights.delta_chroma_l0,
            &mut weights.chroma_offset_l0,
            &tail.weights.l0,
            chroma_denom,
            &range,
        )?;
        if matches!(slice_type, SliceType::B) {
            Self::fill_list(
                &mut weights.delta_luma_l1,
                &mut weights.luma_offset_l1,
                &mut weights.delta_chroma_l1,
                &mut weights.chroma_offset_l1,
                &tail.weights.l1,
                chroma_denom,
                &range,
            )?;
        }
        Ok(weights)
    }

    /// One weight list into the `delta`/`offset` arrays.
    fn fill_list(
        delta_luma: &mut [i8; 15],
        luma_offset: &mut [i8; 15],
        delta_chroma: &mut [[i8; 2]; 15],
        chroma_offset: &mut [[i8; 2]; 15],
        entries: &[crate::h265::WeightEntry],
        chroma_denom: u8,
        range: &impl Fn(&'static str) -> VaApiError,
    ) -> Result<(), VaApiError> {
        for index in 0..15 {
            let entry = entries.get(index);
            let (weight, offset) = entry.map_or((0, 0), |entry| entry.luma.unwrap_or((0, 0)));
            delta_luma[index] = i8::try_from(weight).map_err(|_| range("luma weight"))?;
            luma_offset[index] = i8::try_from(offset).map_err(|_| range("luma offset"))?;
            let pairs = entry.map_or([(0, 0); 2], |entry| entry.chroma.unwrap_or([(0, 0); 2]));
            for (component, (weight, offset)) in pairs.iter().enumerate() {
                delta_chroma[index][component] =
                    i8::try_from(*weight).map_err(|_| range("chroma weight"))?;
                chroma_offset[index][component] =
                    Self::chroma_offset(chroma_denom, *weight, *offset)?;
            }
        }
        Ok(())
    }

    /// Derived chroma offset from the weight delta and offset delta.
    ///
    /// Half range is 128: range extensions decline, so the high
    /// precision path never engages.
    fn chroma_offset(denom: u8, delta_weight: i32, delta_offset: i32) -> Result<i8, VaApiError> {
        let half = 128;
        let weight = (1 << denom) + delta_weight;
        let offset = half + delta_offset - ((half * weight) >> denom);
        i8::try_from(offset.clamp(-half, half - 1)).map_err(|_| {
            VaApiError::Parse(CapsParse::Range {
                syntax: "weight offset",
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use rust_video_h265::SPS_SCALING;

    use super::Params;
    use crate::error::VaApiError;
    use crate::fixtures::{pps_payload, sps_payload};
    use crate::h264::frontend::SurfaceSlot;
    use crate::h265::dpb::Dpb;
    use crate::h265::frontend::{PendingSlice, Rps};
    use crate::h265::picture::{PictureData, Reference};
    use crate::h265::{
        Gate, Pps, SliceHeader, SliceTail, SliceType, SliceWeights, Sps, WeightEntry,
    };

    use std::cell::RefCell;
    use std::rc::Rc;

    /// Parsed fixture sequence and picture sets.
    fn sets() -> (Sps, Pps) {
        let sps = crate::h265::parse_sps::<VaApiError, Gate>(&crate::h265::rbsp(&sps_payload()))
            .expect("sps parses");
        let pps = crate::h265::parse_pps::<VaApiError, Gate>(
            &crate::h265::rbsp(&pps_payload()),
            Some(&sps),
        )
        .expect("pps parses");
        (sps, pps)
    }

    /// P slice header over `tail` defaults.
    fn header() -> SliceHeader {
        SliceHeader {
            first: true,
            address: 0,
            slice_type: SliceType::P,
            pic_output: true,
            no_output_prior: false,
            poc_lsb: Some(0),
            rps: None,
            rps_bits: None,
            lt: Vec::new(),
            entry_count: 0,
            extension_length: 0,
        }
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
        assert!(Params::matrix(&sps, &pps).is_none());
    }

    #[test]
    fn matrix_defaults_match_spec_tables() {
        let (sps, pps) = sets();
        let mut enabled = sps;
        enabled.flags |= SPS_SCALING;
        let matrix = Params::matrix(&enabled, &pps).expect("matrix present");
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
        let mut enabled = sps;
        enabled.flags |= SPS_SCALING;
        enabled.scaling.size0[0][1] = 9;
        let mut pps = pps;
        pps.scaling.size0[0][1] = 7;
        let matrix = Params::matrix(&enabled, &pps).expect("matrix present");
        assert_eq!(matrix.scaling_list_4x4[0][Params::SCAN_4X4[1]], 7);
    }

    #[test]
    fn matrix_maps_custom_dc_coefficients() {
        let (sps, pps) = sets();
        let mut enabled = sps;
        enabled.flags |= SPS_SCALING;
        let mut pps = pps;
        pps.scaling.dc2[0] = 20;
        pps.scaling.dc3[1] = 24;
        let matrix = Params::matrix(&enabled, &pps).expect("matrix present");
        assert_eq!(matrix.scaling_list_dc_16x16[0], 20);
        assert_eq!(matrix.scaling_list_dc_32x32[1], 24);
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
        let dpb = dpb_with(&[0, 2]);
        let surfaces = [50, 51];
        let params = Params::new(&dpb, &surfaces);
        let tail = SliceTail {
            bit_size: 80,
            weights: SliceWeights {
                luma_denom: 0,
                delta_chroma_denom: 2,
                l0: vec![WeightEntry {
                    luma: None,
                    chroma: Some([(0, 10), (0, 200)]),
                }],
                l1: Vec::new(),
            },
            ..SliceTail::default()
        };
        let pending = PendingSlice::assembled(header(), tail, 64, 0);
        let slice = params
            .slice(&pending, &[Some(1), None], &[], true)
            .expect("slice maps");
        assert_eq!(slice.slice_data_size, 64);
        assert_eq!(slice.slice_data_byte_offset, 10);
        assert_eq!(slice.long_slice_flags & 0x1, 1);
        assert_eq!(slice.collocated_ref_idx, 0xff);
        assert_eq!(slice.ref_pic_list[0][0], 1);
        assert_eq!(slice.ref_pic_list[0][1], 0xff);
        assert_eq!(slice.ref_pic_list[1][0], 0xff);
        assert_eq!(slice.chroma_offset_l0[0], [10, 127]);
        let b_header = SliceHeader {
            slice_type: SliceType::B,
            ..header()
        };
        let b_tail = SliceTail {
            temporal_mvp: true,
            collocated_ref_idx: 1,
            ..SliceTail::default()
        };
        let pending = PendingSlice::assembled(b_header, b_tail, 64, 0);
        let slice = params
            .slice(&pending, &[], &[], false)
            .expect("b slice maps");
        assert_eq!(slice.collocated_ref_idx, 1);
        assert_eq!(slice.long_slice_flags & 0x1, 0);
    }

    #[test]
    fn slice_rejects_bad_denom_and_overflow() {
        let dpb = dpb_with(&[0]);
        let surfaces = [50];
        let params = Params::new(&dpb, &surfaces);
        let tail = SliceTail {
            weights: SliceWeights {
                luma_denom: 0,
                delta_chroma_denom: 8,
                l0: Vec::new(),
                l1: Vec::new(),
            },
            ..SliceTail::default()
        };
        let pending = PendingSlice::assembled(header(), tail, 64, 0);
        assert!(matches!(
            params.slice(&pending, &[], &[], true),
            Err(VaApiError::Parse(_))
        ));
        let tail = SliceTail::default();
        let pending = PendingSlice::assembled(header(), tail, 64, 0);
        let long = vec![Some(0); 16];
        assert!(matches!(
            params.slice(&pending, &long, &[], true),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn weights_derive_chroma_offsets() {
        let tail = SliceTail {
            weights: SliceWeights {
                luma_denom: 0,
                delta_chroma_denom: 2,
                l0: vec![WeightEntry {
                    luma: None,
                    chroma: Some([(1, 0), (-1, 0)]),
                }],
                l1: Vec::new(),
            },
            ..SliceTail::default()
        };
        let pending = PendingSlice::assembled(header(), tail, 8, 0);
        let slice = Params::new(&dpb_with(&[]), &[])
            .slice(&pending, &[], &[], true)
            .expect("weights derive");
        assert_eq!(slice.delta_chroma_weight_l0[0], [1, -1]);
        assert_eq!(slice.chroma_offset_l0[0], [-32, 32]);
    }
}
