//! AV1 VA-API parameter buffers from parsed syntax.
//!
//! Portable: pure mapping from sequence, frame, and tile syntax onto
//! the `ffi` layouts; no display handle needed.

use std::os::raw::c_int;

use crate::av1::frontend::NUM_SLOTS;
use crate::av1::parser::{
    CdefParams, FilmGrainParams, FrameHeaderObu, FrameRestorationType, GlobalMotionParams,
    LoopFilterParams, LoopRestorationParams, Profile, QuantizationParams, SegmentationParams,
    SequenceHeaderObu, Tile, TileInfo, WarpModelType, MAX_SEGMENTS, NUM_REF_FRAMES, SEG_LVL_MAX,
};
use crate::error::VaApiError;
use crate::ffi::{
    VaDecPictureParameterBufferAv1, VaFilmGrainAv1, VaSegmentationAv1, VaSliceParameterBufferAv1,
    VaSurfaceId, VaWarpedMotionAv1,
};
use crate::frontend::SurfaceSlot;

/// VA-API parameter builder over the surface pool.
pub struct Params<'a> {
    /// Pool surfaces by slot index.
    surfaces: &'a [VaSurfaceId],
}

impl<'a> Params<'a> {
    /// Builder over pool `surfaces`.
    #[must_use]
    pub fn new(surfaces: &'a [VaSurfaceId]) -> Self {
        Self { surfaces }
    }

    /// VA-API profile for `profile`.
    ///
    /// # Errors
    ///
    /// Returns `Err` for profiles outside Profile 0.
    pub fn profile(profile: Profile) -> Result<c_int, VaApiError> {
        match profile {
            Profile::Profile0 => Ok(crate::ffi::VA_PROFILE_AV1_PROFILE0),
            other => Err(VaApiError::Unsupported(format!(
                "av1 profile {}",
                other as u8
            ))),
        }
    }

    /// Picture-parameter buffer for `header` on `current`.
    ///
    /// `display` carries film grain when the frame applies it and
    /// matches `current` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a reference slot lost its surface or a
    /// corrupt size falls outside the parameter range.
    pub fn picture(
        &self,
        seq: &SequenceHeaderObu,
        header: &FrameHeaderObu,
        refs: &[Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
        current: VaSurfaceId,
        display: VaSurfaceId,
    ) -> Result<VaDecPictureParameterBufferAv1, VaApiError> {
        let range = |what: &str| VaApiError::Parse(format!("picture {what} range"));
        let mut ref_frame_map = [crate::ffi::VA_INVALID_ID; NUM_REF_FRAMES];
        for (surface, entry) in ref_frame_map.iter_mut().zip(refs.iter()) {
            let Some((slot, _)) = entry else {
                continue;
            };
            *surface = self
                .surfaces
                .get(slot.index() as usize)
                .copied()
                .ok_or_else(|| {
                    VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
                })?;
        }
        Ok(VaDecPictureParameterBufferAv1 {
            profile: seq.seq_profile as u8,
            order_hint_bits_minus_1: u8::try_from(seq.order_hint_bits_minus_1)
                .map_err(|_| range("order hint bits"))?,
            bit_depth_idx: 0,
            matrix_coefficients: seq.color_config.matrix_coefficients as u8,
            seq_info_fields: Self::seq_fields(seq),
            current_frame: current,
            current_display_picture: display,
            anchor_frames_num: 0,
            anchor_frames_list: std::ptr::null(),
            frame_width_minus1: u16::try_from(
                header
                    .upscaled_width
                    .checked_sub(1)
                    .ok_or_else(|| range("frame width"))?,
            )
            .map_err(|_| range("frame width"))?,
            frame_height_minus1: u16::try_from(
                header
                    .frame_height
                    .checked_sub(1)
                    .ok_or_else(|| range("frame height"))?,
            )
            .map_err(|_| range("frame height"))?,
            output_frame_width_in_tiles_minus_1: 0,
            output_frame_height_in_tiles_minus_1: 0,
            ref_frame_map,
            ref_frame_idx: header.ref_frame_idx,
            primary_ref_frame: u8::try_from(header.primary_ref_frame)
                .map_err(|_| range("primary ref"))?,
            order_hint: u8::try_from(header.order_hint).map_err(|_| range("order hint"))?,
            seg_info: Self::segmentation(&header.segmentation_params),
            film_grain_info: Self::film_grain(&header.film_grain_params)?,
            tile_cols: u8::try_from(header.tile_info.tile_cols).map_err(|_| range("tile cols"))?,
            tile_rows: u8::try_from(header.tile_info.tile_rows).map_err(|_| range("tile rows"))?,
            width_in_sbs_minus_1: Self::tile_widths(&header.tile_info)?,
            height_in_sbs_minus_1: Self::tile_heights(&header.tile_info)?,
            tile_count_minus_1: 0,
            context_update_tile_id: u16::try_from(header.tile_info.context_update_tile_id)
                .map_err(|_| range("context tile"))?,
            pic_info_fields: Self::pic_fields(header),
            superres_scale_denominator: u8::try_from(header.superres_denom)
                .map_err(|_| range("superres denom"))?,
            interp_filter: header.interpolation_filter as u8,
            filter_level: [
                header.loop_filter_params.loop_filter_level[0],
                header.loop_filter_params.loop_filter_level[1],
            ],
            filter_level_u: header.loop_filter_params.loop_filter_level[2],
            filter_level_v: header.loop_filter_params.loop_filter_level[3],
            loop_filter_info_fields: Self::loop_filter_fields(&header.loop_filter_params),
            ref_deltas: header.loop_filter_params.loop_filter_ref_deltas,
            mode_deltas: header.loop_filter_params.loop_filter_mode_deltas,
            base_qindex: u8::try_from(header.quantization_params.base_q_idx)
                .map_err(|_| range("base qindex"))?,
            y_dc_delta_q: i8::try_from(header.quantization_params.delta_q_y_dc)
                .map_err(|_| range("quant delta"))?,
            u_dc_delta_q: i8::try_from(header.quantization_params.delta_q_u_dc)
                .map_err(|_| range("quant delta"))?,
            u_ac_delta_q: i8::try_from(header.quantization_params.delta_q_u_ac)
                .map_err(|_| range("quant delta"))?,
            v_dc_delta_q: i8::try_from(header.quantization_params.delta_q_v_dc)
                .map_err(|_| range("quant delta"))?,
            v_ac_delta_q: i8::try_from(header.quantization_params.delta_q_v_ac)
                .map_err(|_| range("quant delta"))?,
            qmatrix_fields: Self::qmatrix_fields(&header.quantization_params)?,
            mode_control_fields: Self::mode_control_fields(header),
            cdef_damping_minus_3: u8::try_from(header.cdef_params.damping.saturating_sub(3))
                .map_err(|_| range("cdef damping"))?,
            cdef_bits: u8::try_from(header.cdef_params.bits).map_err(|_| range("cdef bits"))?,
            cdef_y_strengths: Self::cdef_strengths(&header.cdef_params, true)?,
            cdef_uv_strengths: Self::cdef_strengths(&header.cdef_params, false)?,
            loop_restoration_fields: Self::restoration_fields(&header.loop_restoration_params),
            wm: Self::warped_motion(&header.global_motion_params),
            va_reserved: [0; 8],
        })
    }

    /// Slice-parameter buffer for `tile` at `offset` of `size` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt tile size falls outside the
    /// parameter range.
    pub fn slice(
        tile: &Tile,
        offset: u32,
        size: u32,
    ) -> Result<VaSliceParameterBufferAv1, VaApiError> {
        let range = |what: &str| VaApiError::Parse(format!("tile {what} range"));
        Ok(VaSliceParameterBufferAv1 {
            slice_data_size: size,
            slice_data_offset: offset,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            tile_row: u16::try_from(tile.row).map_err(|_| range("row"))?,
            tile_column: u16::try_from(tile.col).map_err(|_| range("column"))?,
            tg_start: 0,
            tg_end: 0,
            anchor_frame_idx: 0,
            tile_idx_in_tile_list: 0,
            va_reserved: [0; 4],
        })
    }

    /// Packed sequence flags.
    fn seq_fields(seq: &SequenceHeaderObu) -> u32 {
        use crate::ffi::{
            AV1_COLOR_RANGE, AV1_ENABLE_CDEF, AV1_ENABLE_DUAL_FILTER, AV1_ENABLE_FILTER_INTRA,
            AV1_ENABLE_INTERINTRA_COMPOUND, AV1_ENABLE_INTRA_EDGE_FILTER, AV1_ENABLE_JNT_COMP,
            AV1_ENABLE_MASKED_COMPOUND, AV1_ENABLE_ORDER_HINT, AV1_FILM_GRAIN_PARAMS_PRESENT,
            AV1_MONO_CHROME, AV1_STILL_PICTURE, AV1_SUBSAMPLING_X, AV1_SUBSAMPLING_Y,
            AV1_USE_128X128_SUPERBLOCK,
        };
        let color = &seq.color_config;
        (u32::from(seq.sequence.still_picture) << AV1_STILL_PICTURE)
            | (u32::from(seq.superblock.use_128x128_superblock) << AV1_USE_128X128_SUPERBLOCK)
            | (u32::from(seq.intra.enable_filter_intra) << AV1_ENABLE_FILTER_INTRA)
            | (u32::from(seq.intra.enable_intra_edge_filter) << AV1_ENABLE_INTRA_EDGE_FILTER)
            | (u32::from(seq.compound.enable_interintra_compound) << AV1_ENABLE_INTERINTRA_COMPOUND)
            | (u32::from(seq.compound.enable_masked_compound) << AV1_ENABLE_MASKED_COMPOUND)
            | (u32::from(seq.inter.enable_dual_filter) << AV1_ENABLE_DUAL_FILTER)
            | (u32::from(seq.inter.enable_order_hint) << AV1_ENABLE_ORDER_HINT)
            | (u32::from(seq.compound.enable_jnt_comp) << AV1_ENABLE_JNT_COMP)
            | (u32::from(seq.tools.enable_cdef) << AV1_ENABLE_CDEF)
            | (u32::from(color.format.mono_chrome) << AV1_MONO_CHROME)
            | (u32::from(color.description.color_range) << AV1_COLOR_RANGE)
            | (u32::from(color.format.subsampling_x) << AV1_SUBSAMPLING_X)
            | (u32::from(color.format.subsampling_y) << AV1_SUBSAMPLING_Y)
            | (u32::from(seq.post.film_grain_params_present) << AV1_FILM_GRAIN_PARAMS_PRESENT)
    }

    /// Packed picture flags.
    fn pic_fields(header: &FrameHeaderObu) -> u32 {
        use crate::ffi::{
            AV1_ALLOW_HIGH_PRECISION_MV, AV1_ALLOW_INTRABC, AV1_ALLOW_SCREEN_CONTENT_TOOLS,
            AV1_ALLOW_WARPED_MOTION, AV1_DISABLE_CDF_UPDATE, AV1_DISABLE_FRAME_END_UPDATE_CDF,
            AV1_ERROR_RESILIENT_MODE, AV1_FORCE_INTEGER_MV, AV1_FRAME_TYPE,
            AV1_IS_MOTION_MODE_SWITCHABLE, AV1_SHOWABLE_FRAME, AV1_SHOW_FRAME,
            AV1_UNIFORM_TILE_SPACING, AV1_USE_REF_FRAME_MVS, AV1_USE_SUPERRES,
        };
        ((header.frame_type as u32) << AV1_FRAME_TYPE)
            | (u32::from(header.show.show_frame) << AV1_SHOW_FRAME)
            | (u32::from(header.show.showable_frame) << AV1_SHOWABLE_FRAME)
            | (u32::from(header.resilience.error_resilient_mode) << AV1_ERROR_RESILIENT_MODE)
            | (u32::from(header.resilience.disable_cdf_update) << AV1_DISABLE_CDF_UPDATE)
            | (u32::from(header.allow_screen_content_tools > 0) << AV1_ALLOW_SCREEN_CONTENT_TOOLS)
            | (u32::from(header.force_integer_mv > 0) << AV1_FORCE_INTEGER_MV)
            | (u32::from(header.tools.allow_intrabc) << AV1_ALLOW_INTRABC)
            | (u32::from(header.size.use_superres) << AV1_USE_SUPERRES)
            | (u32::from(header.motion.allow_high_precision_mv) << AV1_ALLOW_HIGH_PRECISION_MV)
            | (u32::from(header.motion.is_motion_mode_switchable) << AV1_IS_MOTION_MODE_SWITCHABLE)
            | (u32::from(header.motion.use_ref_frame_mvs) << AV1_USE_REF_FRAME_MVS)
            | (u32::from(header.resilience.disable_frame_end_update_cdf)
                << AV1_DISABLE_FRAME_END_UPDATE_CDF)
            | (u32::from(header.tile_info.uniform_tile_spacing_flag) << AV1_UNIFORM_TILE_SPACING)
            | (u32::from(header.tools.allow_warped_motion) << AV1_ALLOW_WARPED_MOTION)
    }

    /// Segmentation parameters.
    fn segmentation(seg: &SegmentationParams) -> VaSegmentationAv1 {
        use crate::ffi::{
            AV1_SEG_ENABLED, AV1_SEG_TEMPORAL_UPDATE, AV1_SEG_UPDATE_DATA, AV1_SEG_UPDATE_MAP,
        };
        let mut feature_mask = [0u8; MAX_SEGMENTS];
        for (mask, enabled) in feature_mask.iter_mut().zip(seg.feature_enabled.iter()) {
            for (bit, on) in enabled.iter().enumerate().take(SEG_LVL_MAX) {
                if *on {
                    *mask |= 1 << bit;
                }
            }
        }
        VaSegmentationAv1 {
            segment_info_fields: (u32::from(seg.map.segmentation_enabled) << AV1_SEG_ENABLED)
                | (u32::from(seg.map.segmentation_update_map) << AV1_SEG_UPDATE_MAP)
                | (u32::from(seg.map.segmentation_temporal_update) << AV1_SEG_TEMPORAL_UPDATE)
                | (u32::from(seg.state.segmentation_update_data) << AV1_SEG_UPDATE_DATA),
            feature_data: seg.feature_data,
            feature_mask,
            va_reserved: [0; 4],
        }
    }

    /// Film-grain parameters.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt coefficient falls outside the
    /// parameter range.
    fn film_grain(fg: &FilmGrainParams) -> Result<VaFilmGrainAv1, VaApiError> {
        use crate::ffi::{
            AV1_APPLY_GRAIN, AV1_AR_COEFF_LAG, AV1_AR_COEFF_SHIFT_MINUS_6,
            AV1_CHROMA_SCALING_FROM_LUMA, AV1_CLIP_TO_RESTRICTED_RANGE, AV1_GRAIN_SCALE_SHIFT,
            AV1_GRAIN_SCALING_MINUS_8, AV1_OVERLAP_FLAG,
        };
        Ok(VaFilmGrainAv1 {
            film_grain_info_fields: (u32::from(fg.apply.apply_grain) << AV1_APPLY_GRAIN)
                | (u32::from(fg.synthesis.chroma_scaling_from_luma)
                    << AV1_CHROMA_SCALING_FROM_LUMA)
                | (u32::from(fg.grain_scaling_minus_8) << AV1_GRAIN_SCALING_MINUS_8)
                | (fg.ar_coeff_lag << AV1_AR_COEFF_LAG)
                | (u32::from(fg.ar_coeff_shift_minus_6) << AV1_AR_COEFF_SHIFT_MINUS_6)
                | (u32::from(fg.grain_scale_shift) << AV1_GRAIN_SCALE_SHIFT)
                | (u32::from(fg.synthesis.overlap_flag) << AV1_OVERLAP_FLAG)
                | (u32::from(fg.synthesis.clip_to_restricted_range)
                    << AV1_CLIP_TO_RESTRICTED_RANGE),
            grain_seed: fg.grain_seed,
            num_y_points: fg.num_y_points,
            point_y_value: Self::points::<14>(&fg.point_y_value),
            point_y_scaling: Self::points::<14>(&fg.point_y_scaling),
            num_cb_points: fg.num_cb_points,
            point_cb_value: Self::points::<10>(&fg.point_cb_value),
            point_cb_scaling: Self::points::<10>(&fg.point_cb_scaling),
            num_cr_points: fg.num_cr_points,
            point_cr_value: Self::points::<10>(&fg.point_cr_value),
            point_cr_scaling: Self::points::<10>(&fg.point_cr_scaling),
            ar_coeffs_y: Self::ar_coeffs::<24>(&fg.ar_coeffs_y_plus_128)?,
            ar_coeffs_cb: Self::ar_coeffs::<25>(&fg.ar_coeffs_cb_plus_128)?,
            ar_coeffs_cr: Self::ar_coeffs::<25>(&fg.ar_coeffs_cr_plus_128)?,
            cb_mult: fg.cb_mult,
            cb_luma_mult: fg.cb_luma_mult,
            cb_offset: fg.cb_offset,
            cr_mult: fg.cr_mult,
            cr_luma_mult: fg.cr_luma_mult,
            cr_offset: fg.cr_offset,
            va_reserved: [0; 4],
        })
    }

    /// Scaling points truncated to the parameter width.
    fn points<const N: usize>(values: &[u8]) -> [u8; N] {
        let mut out = [0u8; N];
        for (slot, value) in out.iter_mut().zip(values.iter()) {
            *slot = *value;
        }
        out
    }

    /// Autoregressive coefficients recentered around zero.
    fn ar_coeffs<const N: usize>(plus_128: &[u8]) -> Result<[i8; N], VaApiError> {
        let mut out = [0i8; N];
        for (slot, value) in out.iter_mut().zip(plus_128.iter()) {
            *slot = i8::try_from(i16::from(*value) - 128)
                .map_err(|_| VaApiError::Parse("grain coeff range".to_owned()))?;
        }
        Ok(out)
    }

    /// Tile widths in superblocks, minus one.
    fn tile_widths(tiles: &TileInfo) -> Result<[u16; 63], VaApiError> {
        let count = usize::try_from(tiles.tile_cols)
            .map_err(|_| VaApiError::Parse("tile columns range".to_owned()))?;
        if count > 63 {
            return Err(VaApiError::Parse("tile columns range".to_owned()));
        }
        let mut widths = [0u16; 63];
        for (out, width) in widths
            .iter_mut()
            .zip(tiles.width_in_sbs_minus_1.iter())
            .take(count)
        {
            *out = u16::try_from(*width)
                .map_err(|_| VaApiError::Parse("tile width range".to_owned()))?;
        }
        Ok(widths)
    }

    /// Tile heights in superblocks, minus one.
    fn tile_heights(tiles: &TileInfo) -> Result<[u16; 63], VaApiError> {
        let count = usize::try_from(tiles.tile_rows)
            .map_err(|_| VaApiError::Parse("tile rows range".to_owned()))?;
        if count > 63 {
            return Err(VaApiError::Parse("tile rows range".to_owned()));
        }
        let mut heights = [0u16; 63];
        for (out, height) in heights
            .iter_mut()
            .zip(tiles.height_in_sbs_minus_1.iter())
            .take(count)
        {
            *out = u16::try_from(*height)
                .map_err(|_| VaApiError::Parse("tile height range".to_owned()))?;
        }
        Ok(heights)
    }

    /// Packed loop-filter flags.
    fn loop_filter_fields(lf: &LoopFilterParams) -> u8 {
        use crate::ffi::{
            AV1_MODE_REF_DELTA_ENABLED, AV1_MODE_REF_DELTA_UPDATE, AV1_SHARPNESS_LEVEL,
        };
        let sharpness = (lf.loop_filter_sharpness << AV1_SHARPNESS_LEVEL) & 0x7;
        let enabled = u8::from(lf.delta.loop_filter_delta_enabled) << AV1_MODE_REF_DELTA_ENABLED;
        let update = u8::from(lf.delta.loop_filter_delta_update) << AV1_MODE_REF_DELTA_UPDATE;
        sharpness | enabled | update
    }

    /// Packed quantizer-matrix flags.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt matrix level falls outside the
    /// parameter range.
    fn qmatrix_fields(quant: &QuantizationParams) -> Result<u16, VaApiError> {
        use crate::ffi::{AV1_QM_U, AV1_QM_V, AV1_QM_Y, AV1_USING_QMATRIX};
        let range = |what: &str| VaApiError::Parse(format!("quant {what} range"));
        let qm_y = u16::try_from(quant.qm_y).map_err(|_| range("matrix"))?;
        let qm_u = u16::try_from(quant.qm_u).map_err(|_| range("matrix"))?;
        let qm_v = u16::try_from(quant.qm_v).map_err(|_| range("matrix"))?;
        if qm_y > 15 || qm_u > 15 || qm_v > 15 {
            return Err(range("matrix"));
        }
        Ok((u16::from(quant.using_qmatrix) << AV1_USING_QMATRIX)
            | (qm_y << AV1_QM_Y)
            | (qm_u << AV1_QM_U)
            | (qm_v << AV1_QM_V))
    }

    /// Packed mode-control flags.
    fn mode_control_fields(header: &FrameHeaderObu) -> u32 {
        use crate::ffi::{
            AV1_DELTA_LF_MULTI, AV1_DELTA_LF_PRESENT, AV1_DELTA_Q_PRESENT, AV1_LOG2_DELTA_LF_RES,
            AV1_LOG2_DELTA_Q_RES, AV1_REDUCED_TX_SET_USED, AV1_REFERENCE_SELECT,
            AV1_SKIP_MODE_PRESENT, AV1_TX_MODE,
        };
        let quant = &header.quantization_params;
        let lf = &header.loop_filter_params;
        (u32::from(quant.delta_q_present) << AV1_DELTA_Q_PRESENT)
            | (quant.delta_q_res << AV1_LOG2_DELTA_Q_RES)
            | (u32::from(lf.multi.delta_lf_present) << AV1_DELTA_LF_PRESENT)
            | (u32::from(lf.delta_lf_res) << AV1_LOG2_DELTA_LF_RES)
            | (u32::from(lf.multi.delta_lf_multi) << AV1_DELTA_LF_MULTI)
            | ((header.tx_mode as u32) << AV1_TX_MODE)
            | (u32::from(header.mode.reference_select) << AV1_REFERENCE_SELECT)
            | (u32::from(header.mode.reduced_tx_set) << AV1_REDUCED_TX_SET_USED)
            | (u32::from(header.mode.skip_mode_present) << AV1_SKIP_MODE_PRESENT)
    }

    /// Packed CDEF strengths.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt strength falls outside the
    /// parameter range.
    fn cdef_strengths(cdef: &CdefParams, luma: bool) -> Result<[u8; 8], VaApiError> {
        let mut strengths = [0u8; 8];
        let count = (1usize << cdef.bits.min(3)).min(8);
        for (index, out) in strengths.iter_mut().enumerate().take(count) {
            let (pri, sec) = if luma {
                (
                    cdef.y_pri_strength[index] & 0xf,
                    Self::cdef_sec(cdef.y_sec_strength[index]),
                )
            } else {
                (
                    cdef.uv_pri_strength[index] & 0xf,
                    Self::cdef_sec(cdef.uv_sec_strength[index]),
                )
            };
            *out = u8::try_from((pri << 2) | sec)
                .map_err(|_| VaApiError::Parse("cdef strength range".to_owned()))?;
        }
        Ok(strengths)
    }

    /// Secondary CDEF strength clipped to two bits.
    fn cdef_sec(raw: u32) -> u32 {
        raw.min(3)
    }

    /// Packed restoration flags.
    fn restoration_fields(lr: &LoopRestorationParams) -> u16 {
        use crate::ffi::{
            AV1_CBFRAME_RESTORATION_TYPE, AV1_CRFRAME_RESTORATION_TYPE, AV1_LR_UNIT_SHIFT,
            AV1_LR_UV_SHIFT, AV1_YFRAME_RESTORATION_TYPE,
        };
        (Self::restoration_type(lr.frame_restoration_type[0]) << AV1_YFRAME_RESTORATION_TYPE)
            | (Self::restoration_type(lr.frame_restoration_type[1]) << AV1_CBFRAME_RESTORATION_TYPE)
            | (Self::restoration_type(lr.frame_restoration_type[2]) << AV1_CRFRAME_RESTORATION_TYPE)
            | (u16::from(lr.lr_unit_shift) << AV1_LR_UNIT_SHIFT)
            | (u16::from(lr.lr_uv_shift) << AV1_LR_UV_SHIFT)
    }

    /// Restoration type remapped onto the VA-API order.
    fn restoration_type(kind: FrameRestorationType) -> u16 {
        match kind {
            FrameRestorationType::None => 0,
            FrameRestorationType::Switchable => 1,
            FrameRestorationType::Wiener => 2,
            FrameRestorationType::Sgrproj => 3,
        }
    }

    /// Warped-motion parameters by `LAST` to `ALTREF` order.
    fn warped_motion(gm: &GlobalMotionParams) -> [VaWarpedMotionAv1; 7] {
        let mut wm = [VaWarpedMotionAv1 {
            wmtype: 0,
            wmmat: [0; 8],
            invalid: 0,
            va_reserved: [0; 4],
        }; 7];
        for (slot, index) in wm.iter_mut().zip(1..=7usize) {
            slot.wmtype = match gm.gm_type[index] {
                WarpModelType::Identity => crate::ffi::AV1_WARP_IDENTITY,
                WarpModelType::Translation => crate::ffi::AV1_WARP_TRANSLATION,
                WarpModelType::RotZoom => crate::ffi::AV1_WARP_ROTZOOM,
                WarpModelType::Affine => crate::ffi::AV1_WARP_AFFINE,
            };
            slot.wmmat[..6].copy_from_slice(&gm.gm_params[index][..6]);
            slot.invalid = u8::from(!gm.warp_valid[index]);
        }
        wm
    }
}

#[cfg(test)]
mod tests {
    use super::Params;
    use crate::av1::frontend::NUM_SLOTS;
    use crate::av1::parser::{
        FrameHeaderObu, FrameRestorationType, FrameType, Profile, SequenceHeaderObu, Tile,
        WarpModelType,
    };
    use crate::error::VaApiError;
    use crate::frontend::SurfaceSlot;

    /// Surfaces backing pool slots.
    const SURFACES: [u32; NUM_SLOTS + 4] = [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21];

    /// Sequence header for Profile 0 8-bit 4:2:0.
    fn sequence() -> SequenceHeaderObu {
        let mut seq = SequenceHeaderObu::default();
        seq.color_config.format.subsampling_x = true;
        seq.color_config.format.subsampling_y = true;
        seq
    }

    /// Frame header for a shown 64x64 keyframe.
    fn keyframe() -> FrameHeaderObu {
        FrameHeaderObu {
            frame_type: FrameType::Key,
            show: crate::av1::parser::ShowFlags {
                show_frame: true,
                ..crate::av1::parser::ShowFlags::default()
            },
            frame_width: 64,
            frame_height: 64,
            upscaled_width: 64,
            render_width: 64,
            render_height: 64,
            superres_denom: 8,
            tile_info: crate::av1::parser::TileInfo {
                tile_cols: 1,
                tile_rows: 1,
                ..crate::av1::parser::TileInfo::default()
            },
            ..FrameHeaderObu::default()
        }
    }

    /// Reference slots with all entries unset.
    fn empty_refs() -> [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS] {
        [None; NUM_SLOTS]
    }

    #[test]
    fn profile_maps_profile_zero() {
        assert_eq!(
            Params::profile(Profile::Profile0).expect("profile 0"),
            crate::ffi::VA_PROFILE_AV1_PROFILE0
        );
        assert!(matches!(
            Params::profile(Profile::Profile1),
            Err(VaApiError::Unsupported(_))
        ));
        assert!(matches!(
            Params::profile(Profile::Profile2),
            Err(VaApiError::Unsupported(_))
        ));
    }

    #[test]
    fn picture_maps_references_and_sizes() {
        let seq = sequence();
        let header = keyframe();
        let mut refs = empty_refs();
        refs[0] = Some((SurfaceSlot::new(2), (64, 64)));
        refs[3] = Some((SurfaceSlot::new(5), (64, 64)));
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &refs, 18, 19)
            .expect("picture maps");
        assert_eq!(picture.profile, 0);
        assert_eq!(picture.current_frame, 18);
        assert_eq!(picture.current_display_picture, 19);
        assert_eq!(picture.frame_width_minus1, 63);
        assert_eq!(picture.frame_height_minus1, 63);
        assert_eq!(picture.ref_frame_map[0], 12);
        assert_eq!(picture.ref_frame_map[1], crate::ffi::VA_INVALID_ID);
        assert_eq!(picture.ref_frame_map[3], 15);
        assert_eq!(picture.superres_scale_denominator, 8);
        assert!(picture.anchor_frames_list.is_null());
    }

    #[test]
    fn picture_rejects_lost_surface_and_empty_frame() {
        let seq = sequence();
        let header = keyframe();
        let mut refs = empty_refs();
        refs[0] = Some((SurfaceSlot::new(99), (64, 64)));
        assert!(matches!(
            Params::new(&SURFACES).picture(&seq, &header, &refs, 18, 18),
            Err(VaApiError::BrokenReference(_))
        ));
        let mut header = keyframe();
        header.frame_width = 0;
        header.upscaled_width = 0;
        assert!(matches!(
            Params::new(&SURFACES).picture(&seq, &header, &empty_refs(), 18, 18),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn picture_packs_sequence_and_picture_flags() {
        let mut seq = sequence();
        seq.sequence.still_picture = true;
        seq.superblock.use_128x128_superblock = true;
        seq.inter.enable_order_hint = true;
        let mut header = keyframe();
        header.show.show_frame = true;
        header.show.showable_frame = true;
        header.resilience.error_resilient_mode = true;
        header.tile_info.uniform_tile_spacing_flag = true;
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &empty_refs(), 18, 18)
            .expect("picture maps");
        assert_eq!(picture.seq_info_fields & 0x1, 1);
        assert_eq!((picture.seq_info_fields >> 1) & 0x1, 1);
        assert_eq!((picture.seq_info_fields >> 7) & 0x1, 1);
        assert_eq!((picture.pic_info_fields >> 2) & 0x1, 1);
        assert_eq!((picture.pic_info_fields >> 3) & 0x1, 1);
        assert_eq!((picture.pic_info_fields >> 4) & 0x1, 1);
        assert_eq!((picture.pic_info_fields >> 14) & 0x1, 1);
        assert_eq!(picture.pic_info_fields & 0x3, FrameType::Key as u32);
    }

    #[test]
    fn picture_maps_segmentation_and_grain() {
        let seq = sequence();
        let mut header = keyframe();
        header.segmentation_params.map.segmentation_enabled = true;
        header.segmentation_params.map.segmentation_update_map = true;
        header.segmentation_params.feature_enabled[0][0] = true;
        header.segmentation_params.feature_data[0][0] = -4;
        header.film_grain_params.apply.apply_grain = true;
        header.film_grain_params.grain_seed = 7;
        header.film_grain_params.num_y_points = 2;
        header.film_grain_params.point_y_value[0] = 9;
        header.film_grain_params.ar_coeffs_y_plus_128[0] = 128 + 5;
        header.film_grain_params.ar_coeffs_cb_plus_128[0] = 128 - 3;
        header.film_grain_params.ar_coeffs_cr_plus_128[24] = 128;
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &empty_refs(), 18, 18)
            .expect("picture maps");
        assert_eq!(picture.seg_info.segment_info_fields & 0x3, 0x3);
        assert_eq!(picture.seg_info.feature_mask[0], 0x01);
        assert_eq!(picture.seg_info.feature_data[0][0], -4);
        assert_eq!(picture.film_grain_info.film_grain_info_fields & 0x1, 1);
        assert_eq!(picture.film_grain_info.grain_seed, 7);
        assert_eq!(picture.film_grain_info.num_y_points, 2);
        assert_eq!(picture.film_grain_info.point_y_value[0], 9);
        assert_eq!(picture.film_grain_info.ar_coeffs_y[0], 5);
        assert_eq!(picture.film_grain_info.ar_coeffs_cb[0], -3);
        assert_eq!(picture.film_grain_info.ar_coeffs_cr[24], 0);
    }

    #[test]
    fn picture_remaps_restoration_and_warp() {
        let seq = sequence();
        let mut header = keyframe();
        header.loop_restoration_params.frame_restoration_type[0] = FrameRestorationType::Wiener;
        header.loop_restoration_params.frame_restoration_type[1] = FrameRestorationType::Switchable;
        header.loop_restoration_params.frame_restoration_type[2] = FrameRestorationType::Sgrproj;
        header.global_motion_params.gm_type[1] = WarpModelType::Translation;
        header.global_motion_params.gm_params[1] = [3, 5, 0, 0, 0, 0];
        header.global_motion_params.warp_valid[1] = false;
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &empty_refs(), 18, 18)
            .expect("picture maps");
        assert_eq!(picture.loop_restoration_fields & 0x3f, 0b11_01_10);
        assert_eq!(picture.wm[0].wmtype, crate::ffi::AV1_WARP_TRANSLATION);
        assert_eq!(picture.wm[0].wmmat[0], 3);
        assert_eq!(picture.wm[0].wmmat[1], 5);
        assert_eq!(picture.wm[0].wmmat[6], 0);
        assert_eq!(picture.wm[0].invalid, 1);
        assert_eq!(picture.wm[1].wmtype, crate::ffi::AV1_WARP_IDENTITY);
    }

    #[test]
    fn picture_packs_cdef_and_quant() {
        let seq = sequence();
        let mut header = keyframe();
        header.cdef_params.damping = 5;
        header.cdef_params.bits = 2;
        header.cdef_params.y_pri_strength[0] = 9;
        header.cdef_params.y_sec_strength[0] = 4;
        header.quantization_params.base_q_idx = 128;
        header.quantization_params.delta_q_y_dc = -2;
        header.quantization_params.using_qmatrix = true;
        header.quantization_params.qm_y = 5;
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &empty_refs(), 18, 18)
            .expect("picture maps");
        assert_eq!(picture.cdef_damping_minus_3, 2);
        assert_eq!(picture.cdef_bits, 2);
        assert_eq!(picture.cdef_y_strengths[0], (9 << 2) | 3);
        assert_eq!(picture.base_qindex, 128);
        assert_eq!(picture.y_dc_delta_q, -2);
        assert_eq!(picture.qmatrix_fields & 0x1, 1);
        assert_eq!((picture.qmatrix_fields >> 1) & 0xf, 5);
        let mut header = keyframe();
        header.quantization_params.qm_u = 16;
        assert!(matches!(
            Params::new(&SURFACES).picture(&seq, &header, &empty_refs(), 18, 18),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn picture_maps_tiles_and_filters() {
        let seq = sequence();
        let mut header = keyframe();
        header.tile_info.tile_cols = 2;
        header.tile_info.width_in_sbs_minus_1[0] = 3;
        header.tile_info.width_in_sbs_minus_1[1] = 5;
        header.tile_info.context_update_tile_id = 1;
        header.loop_filter_params.loop_filter_level = [6, 6, 4, 4];
        header.loop_filter_params.loop_filter_sharpness = 3;
        header.loop_filter_params.delta.loop_filter_delta_enabled = true;
        header.loop_filter_params.loop_filter_ref_deltas[0] = 2;
        let picture = Params::new(&SURFACES)
            .picture(&seq, &header, &empty_refs(), 18, 18)
            .expect("picture maps");
        assert_eq!(picture.tile_cols, 2);
        assert_eq!(picture.width_in_sbs_minus_1[0], 3);
        assert_eq!(picture.width_in_sbs_minus_1[1], 5);
        assert_eq!(picture.context_update_tile_id, 1);
        assert_eq!(picture.filter_level, [6, 6]);
        assert_eq!(picture.filter_level_u, 4);
        assert_eq!(picture.loop_filter_info_fields & 0x7, 3);
        assert_eq!((picture.loop_filter_info_fields >> 3) & 0x1, 1);
        assert_eq!(picture.ref_deltas[0], 2);
        let mut header = keyframe();
        header.tile_info.tile_cols = 64;
        assert!(matches!(
            Params::new(&SURFACES).picture(&seq, &header, &empty_refs(), 18, 18),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn slice_maps_tile_geometry() {
        let tile = Tile {
            offset: 16,
            size: 32,
            row: 1,
            col: 2,
            ..Tile::default()
        };
        let slice = Params::slice(&tile, 16, 32).expect("slice maps");
        assert_eq!(slice.slice_data_size, 32);
        assert_eq!(slice.slice_data_offset, 16);
        assert_eq!(slice.slice_data_flag, crate::ffi::VA_SLICE_DATA_FLAG_ALL);
        assert_eq!(slice.tile_row, 1);
        assert_eq!(slice.tile_column, 2);
        assert_eq!(slice.tg_start, 0);
        let tile = Tile {
            row: u32::MAX,
            ..Tile::default()
        };
        assert!(matches!(
            Params::slice(&tile, 0, 0),
            Err(VaApiError::Parse(_))
        ));
    }
}
