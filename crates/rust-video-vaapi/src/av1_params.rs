//! AV1 VA-API parameter buffers from parsed syntax.
//!
//! Portable: pure mapping from sequence, frame, and tile syntax onto
//! the `ffi` layouts; no display handle needed.

use std::os::raw::c_int;

use crate::av1::{
    FilmGrain, FrameHeader, GlobalMotion, SequenceHeader, MAX_SEGMENTS, NUM_REF_FRAMES,
    REFS_PER_FRAME, SEG_LEVELS, SEQ_COLOR_RANGE, SUPERRES_NUM,
};
use crate::av1_frontend::{Tile, NUM_SLOTS};
use crate::error::VaApiError;
use crate::ffi::{
    VaDecPictureParameterBufferAv1, VaFilmGrainAv1, VaSegmentationAv1, VaSliceParameterBufferAv1,
    VaSurfaceId, VaWarpedMotionAv1,
};
use crate::h264_frontend::SurfaceSlot;

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
    pub fn profile(profile: u8) -> Result<c_int, VaApiError> {
        match profile {
            0 => Ok(crate::ffi::VA_PROFILE_AV1_PROFILE0),
            other => Err(VaApiError::Unsupported(format!("av1 profile {other}"))),
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
        seq: &SequenceHeader,
        header: &FrameHeader,
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
            profile: seq.profile,
            order_hint_bits_minus_1: seq.order_hint_bits_minus1,
            bit_depth_idx: 0,
            matrix_coefficients: seq.matrix_coefficients,
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
            primary_ref_frame: header.primary_ref_frame,
            order_hint: header.order_hint,
            seg_info: Self::segmentation(header),
            film_grain_info: Self::film_grain(&header.film_grain)?,
            tile_cols: header.tile_cols,
            tile_rows: header.tile_rows,
            width_in_sbs_minus_1: Self::tile_widths(header)?,
            height_in_sbs_minus_1: Self::tile_heights(header)?,
            tile_count_minus_1: 0,
            context_update_tile_id: header.context_update_tile_id,
            pic_info_fields: Self::pic_fields(header),
            superres_scale_denominator: header.superres_denom,
            interp_filter: header.interp_filter,
            filter_level: [header.loop_filter_level[0], header.loop_filter_level[1]],
            filter_level_u: header.loop_filter_level[2],
            filter_level_v: header.loop_filter_level[3],
            loop_filter_info_fields: Self::loop_filter_fields(header),
            ref_deltas: header.loop_ref_deltas,
            mode_deltas: header.loop_mode_deltas,
            base_qindex: header.base_q_idx,
            y_dc_delta_q: header.delta_q_y_dc,
            u_dc_delta_q: header.delta_q_u_dc,
            u_ac_delta_q: header.delta_q_u_ac,
            v_dc_delta_q: header.delta_q_v_dc,
            v_ac_delta_q: header.delta_q_v_ac,
            qmatrix_fields: Self::qmatrix_fields(header)?,
            mode_control_fields: Self::mode_control_fields(header),
            cdef_damping_minus_3: header.cdef_damping_minus_3,
            cdef_bits: header.cdef_bits,
            cdef_y_strengths: Self::cdef_strengths(
                header.cdef_y_pri,
                header.cdef_y_sec,
                header.cdef_bits,
            )?,
            cdef_uv_strengths: Self::cdef_strengths(
                header.cdef_uv_pri,
                header.cdef_uv_sec,
                header.cdef_bits,
            )?,
            loop_restoration_fields: Self::restoration_fields(header),
            wm: Self::warped_motion(&header.global_motion),
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
    fn seq_fields(seq: &SequenceHeader) -> u32 {
        use crate::ffi::{
            AV1_COLOR_RANGE, AV1_ENABLE_CDEF, AV1_ENABLE_DUAL_FILTER, AV1_ENABLE_FILTER_INTRA,
            AV1_ENABLE_INTERINTRA_COMPOUND, AV1_ENABLE_INTRA_EDGE_FILTER, AV1_ENABLE_JNT_COMP,
            AV1_ENABLE_MASKED_COMPOUND, AV1_ENABLE_ORDER_HINT, AV1_FILM_GRAIN_PARAMS_PRESENT,
            AV1_MONO_CHROME, AV1_STILL_PICTURE, AV1_SUBSAMPLING_X, AV1_SUBSAMPLING_Y,
            AV1_USE_128X128_SUPERBLOCK,
        };
        (u32::from(seq.still_picture()) << AV1_STILL_PICTURE)
            | (u32::from(seq.use_128x128_superblock()) << AV1_USE_128X128_SUPERBLOCK)
            | (u32::from(seq.enable_filter_intra()) << AV1_ENABLE_FILTER_INTRA)
            | (u32::from(seq.enable_intra_edge_filter()) << AV1_ENABLE_INTRA_EDGE_FILTER)
            | (u32::from(seq.enable_interintra_compound()) << AV1_ENABLE_INTERINTRA_COMPOUND)
            | (u32::from(seq.enable_masked_compound()) << AV1_ENABLE_MASKED_COMPOUND)
            | (u32::from(seq.enable_dual_filter()) << AV1_ENABLE_DUAL_FILTER)
            | (u32::from(seq.enable_order_hint()) << AV1_ENABLE_ORDER_HINT)
            | (u32::from(seq.enable_jnt_comp()) << AV1_ENABLE_JNT_COMP)
            | (u32::from(seq.enable_cdef()) << AV1_ENABLE_CDEF)
            | (u32::from(seq.mono_chrome()) << AV1_MONO_CHROME)
            | (u32::from(seq.flags & SEQ_COLOR_RANGE != 0) << AV1_COLOR_RANGE)
            | (u32::from(seq.subsampling_x()) << AV1_SUBSAMPLING_X)
            | (u32::from(seq.subsampling_y()) << AV1_SUBSAMPLING_Y)
            | (u32::from(seq.film_grain_params_present()) << AV1_FILM_GRAIN_PARAMS_PRESENT)
    }

    /// Packed picture flags.
    fn pic_fields(header: &FrameHeader) -> u32 {
        use crate::ffi::{
            AV1_ALLOW_HIGH_PRECISION_MV, AV1_ALLOW_INTRABC, AV1_ALLOW_SCREEN_CONTENT_TOOLS,
            AV1_ALLOW_WARPED_MOTION, AV1_DISABLE_CDF_UPDATE, AV1_DISABLE_FRAME_END_UPDATE_CDF,
            AV1_ERROR_RESILIENT_MODE, AV1_FORCE_INTEGER_MV, AV1_FRAME_TYPE,
            AV1_IS_MOTION_MODE_SWITCHABLE, AV1_SHOWABLE_FRAME, AV1_SHOW_FRAME,
            AV1_UNIFORM_TILE_SPACING, AV1_USE_REF_FRAME_MVS, AV1_USE_SUPERRES,
        };
        (u32::from(header.frame_type.id()) << AV1_FRAME_TYPE)
            | (u32::from(header.show_frame()) << AV1_SHOW_FRAME)
            | (u32::from(header.showable_frame()) << AV1_SHOWABLE_FRAME)
            | (u32::from(header.error_resilient_mode()) << AV1_ERROR_RESILIENT_MODE)
            | (u32::from(header.disable_cdf_update()) << AV1_DISABLE_CDF_UPDATE)
            | (u32::from(header.allow_screen_content_tools()) << AV1_ALLOW_SCREEN_CONTENT_TOOLS)
            | (u32::from(header.force_integer_mv()) << AV1_FORCE_INTEGER_MV)
            | (u32::from(header.allow_intrabc()) << AV1_ALLOW_INTRABC)
            | (u32::from(header.superres_denom != SUPERRES_NUM) << AV1_USE_SUPERRES)
            | (u32::from(header.allow_high_precision_mv()) << AV1_ALLOW_HIGH_PRECISION_MV)
            | (u32::from(header.is_motion_mode_switchable()) << AV1_IS_MOTION_MODE_SWITCHABLE)
            | (u32::from(header.use_ref_frame_mvs()) << AV1_USE_REF_FRAME_MVS)
            | (u32::from(header.disable_frame_end_update_cdf()) << AV1_DISABLE_FRAME_END_UPDATE_CDF)
            | (u32::from(header.uniform_tile_spacing) << AV1_UNIFORM_TILE_SPACING)
            | (u32::from(header.allow_warped_motion()) << AV1_ALLOW_WARPED_MOTION)
    }

    /// Segmentation parameters.
    fn segmentation(header: &FrameHeader) -> VaSegmentationAv1 {
        use crate::ffi::{
            AV1_SEG_ENABLED, AV1_SEG_TEMPORAL_UPDATE, AV1_SEG_UPDATE_DATA, AV1_SEG_UPDATE_MAP,
        };
        let mut feature_mask = [0u8; MAX_SEGMENTS];
        for (mask, enabled) in feature_mask.iter_mut().zip(header.seg_enabled.iter()) {
            for (bit, on) in enabled.iter().enumerate().take(SEG_LEVELS) {
                if *on {
                    *mask |= 1 << bit;
                }
            }
        }
        let mut feature_data = [[0i16; SEG_LEVELS]; MAX_SEGMENTS];
        for (row, values) in feature_data.iter_mut().zip(header.seg_value.iter()) {
            row.copy_from_slice(values);
        }
        VaSegmentationAv1 {
            segment_info_fields: (u32::from(header.segmentation_enabled()) << AV1_SEG_ENABLED)
                | (u32::from(header.segmentation_update_map()) << AV1_SEG_UPDATE_MAP)
                | (u32::from(header.segmentation_update_data()) << AV1_SEG_UPDATE_DATA)
                | (u32::from(header.segmentation_temporal_update()) << AV1_SEG_TEMPORAL_UPDATE),
            feature_mask,
            feature_data,
            va_reserved: [0; 4],
        }
    }

    /// Film-grain parameters.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt coefficient falls outside the
    /// parameter range.
    fn film_grain(fg: &FilmGrain) -> Result<VaFilmGrainAv1, VaApiError> {
        use crate::ffi::{
            AV1_APPLY_GRAIN, AV1_AR_COEFF_LAG, AV1_AR_COEFF_SHIFT_MINUS_6,
            AV1_CHROMA_SCALING_FROM_LUMA, AV1_CLIP_TO_RESTRICTED_RANGE, AV1_GRAIN_SCALE_SHIFT,
            AV1_GRAIN_SCALING_MINUS_8, AV1_OVERLAP_FLAG,
        };
        let mut grain_info = VaFilmGrainAv1 {
            film_grain_info_fields: (u32::from(fg.apply_grain()) << AV1_APPLY_GRAIN)
                | (u32::from(fg.chroma_scaling_from_luma()) << AV1_CHROMA_SCALING_FROM_LUMA)
                | (u32::from(fg.grain_scaling_minus_8) << AV1_GRAIN_SCALING_MINUS_8)
                | (u32::from(fg.ar_coeff_lag) << AV1_AR_COEFF_LAG)
                | (u32::from(fg.ar_coeff_shift_minus_6) << AV1_AR_COEFF_SHIFT_MINUS_6)
                | (u32::from(fg.grain_scale_shift) << AV1_GRAIN_SCALE_SHIFT)
                | (u32::from(fg.overlap_flag()) << AV1_OVERLAP_FLAG)
                | (u32::from(fg.clip_to_restricted_range()) << AV1_CLIP_TO_RESTRICTED_RANGE),
            grain_seed: fg.grain_seed,
            num_y_points: fg.num_y_points,
            point_y_value: [0; 14],
            point_y_scaling: [0; 14],
            num_cb_points: fg.num_cb_points,
            point_cb_value: [0; 10],
            point_cb_scaling: [0; 10],
            num_cr_points: fg.num_cr_points,
            point_cr_value: [0; 10],
            point_cr_scaling: [0; 10],
            ar_coeffs_y: Self::ar_coeffs::<24>(&fg.ar_coeffs_y)?,
            ar_coeffs_cb: Self::ar_coeffs::<25>(&fg.ar_coeffs_cb)?,
            ar_coeffs_cr: Self::ar_coeffs::<25>(&fg.ar_coeffs_cr)?,
            cb_mult: fg.cb_mult,
            cb_luma_mult: fg.cb_luma_mult,
            cb_offset: fg.cb_offset,
            cr_mult: fg.cr_mult,
            cr_luma_mult: fg.cr_luma_mult,
            cr_offset: fg.cr_offset,
            va_reserved: [0; 4],
        };
        for (index, pair) in fg.scaling_points_y.iter().enumerate() {
            grain_info.point_y_value[index] = pair[0];
            grain_info.point_y_scaling[index] = pair[1];
        }
        for (index, pair) in fg.scaling_points_cb.iter().enumerate() {
            grain_info.point_cb_value[index] = pair[0];
            grain_info.point_cb_scaling[index] = pair[1];
        }
        for (index, pair) in fg.scaling_points_cr.iter().enumerate() {
            grain_info.point_cr_value[index] = pair[0];
            grain_info.point_cr_scaling[index] = pair[1];
        }
        Ok(grain_info)
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
    fn tile_widths(header: &FrameHeader) -> Result<[u16; 63], VaApiError> {
        let count = usize::from(header.tile_cols);
        if count > 63 {
            return Err(VaApiError::Parse("tile columns range".to_owned()));
        }
        let mut widths = [0u16; 63];
        for (out, width) in widths
            .iter_mut()
            .zip(header.width_in_sbs_minus_1.iter())
            .take(count)
        {
            *out = *width;
        }
        Ok(widths)
    }

    /// Tile heights in superblocks, minus one.
    fn tile_heights(header: &FrameHeader) -> Result<[u16; 63], VaApiError> {
        let count = usize::from(header.tile_rows);
        if count > 63 {
            return Err(VaApiError::Parse("tile rows range".to_owned()));
        }
        let mut heights = [0u16; 63];
        for (out, height) in heights
            .iter_mut()
            .zip(header.height_in_sbs_minus_1.iter())
            .take(count)
        {
            *out = *height;
        }
        Ok(heights)
    }

    /// Packed loop-filter flags.
    fn loop_filter_fields(header: &FrameHeader) -> u8 {
        use crate::ffi::{
            AV1_MODE_REF_DELTA_ENABLED, AV1_MODE_REF_DELTA_UPDATE, AV1_SHARPNESS_LEVEL,
        };
        let sharpness = (header.loop_filter_sharpness << AV1_SHARPNESS_LEVEL) & 0x7;
        let enabled = u8::from(header.loop_filter_delta_enabled()) << AV1_MODE_REF_DELTA_ENABLED;
        let update = u8::from(header.loop_filter_delta_update()) << AV1_MODE_REF_DELTA_UPDATE;
        sharpness | enabled | update
    }

    /// Packed quantizer-matrix flags.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt matrix level falls outside the
    /// parameter range.
    fn qmatrix_fields(header: &FrameHeader) -> Result<u16, VaApiError> {
        use crate::ffi::{AV1_QM_U, AV1_QM_V, AV1_QM_Y, AV1_USING_QMATRIX};
        let range = |what: &str| VaApiError::Parse(format!("quant {what} range"));
        let qm_y = u16::from(header.qm_y);
        let qm_u = u16::from(header.qm_u);
        let qm_v = u16::from(header.qm_v);
        if qm_y > 15 || qm_u > 15 || qm_v > 15 {
            return Err(range("matrix"));
        }
        Ok((u16::from(header.using_qmatrix()) << AV1_USING_QMATRIX)
            | (qm_y << AV1_QM_Y)
            | (qm_u << AV1_QM_U)
            | (qm_v << AV1_QM_V))
    }

    /// Packed mode-control flags.
    fn mode_control_fields(header: &FrameHeader) -> u32 {
        use crate::ffi::{
            AV1_DELTA_LF_MULTI, AV1_DELTA_LF_PRESENT, AV1_DELTA_Q_PRESENT, AV1_LOG2_DELTA_LF_RES,
            AV1_LOG2_DELTA_Q_RES, AV1_REDUCED_TX_SET_USED, AV1_REFERENCE_SELECT,
            AV1_SKIP_MODE_PRESENT, AV1_TX_MODE,
        };
        (u32::from(header.delta_q_present()) << AV1_DELTA_Q_PRESENT)
            | (u32::from(header.delta_q_res) << AV1_LOG2_DELTA_Q_RES)
            | (u32::from(header.delta_lf_present()) << AV1_DELTA_LF_PRESENT)
            | (u32::from(header.delta_lf_res) << AV1_LOG2_DELTA_LF_RES)
            | (u32::from(header.delta_lf_multi()) << AV1_DELTA_LF_MULTI)
            | (u32::from(header.tx_mode) << AV1_TX_MODE)
            | (u32::from(header.reference_select()) << AV1_REFERENCE_SELECT)
            | (u32::from(header.reduced_tx_set()) << AV1_REDUCED_TX_SET_USED)
            | (u32::from(header.skip_mode_present()) << AV1_SKIP_MODE_PRESENT)
    }

    /// Packed CDEF strengths.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt strength falls outside the
    /// parameter range.
    fn cdef_strengths(pri: [u8; 8], sec: [u8; 8], bits: u8) -> Result<[u8; 8], VaApiError> {
        let mut strengths = [0u8; 8];
        let count = (1usize << bits.min(3)).min(8);
        for (index, out) in strengths.iter_mut().enumerate().take(count) {
            let strength = (u16::from(pri[index] & 0xf) << 2) | u16::from(sec[index].min(3));
            *out = u8::try_from(strength)
                .map_err(|_| VaApiError::Parse("cdef strength range".to_owned()))?;
        }
        Ok(strengths)
    }

    /// Packed restoration flags.
    fn restoration_fields(header: &FrameHeader) -> u16 {
        use crate::ffi::{
            AV1_CBFRAME_RESTORATION_TYPE, AV1_CRFRAME_RESTORATION_TYPE, AV1_LR_UNIT_SHIFT,
            AV1_LR_UV_SHIFT, AV1_YFRAME_RESTORATION_TYPE,
        };
        (Self::restoration_type(header.lr_type[0]) << AV1_YFRAME_RESTORATION_TYPE)
            | (Self::restoration_type(header.lr_type[1]) << AV1_CBFRAME_RESTORATION_TYPE)
            | (Self::restoration_type(header.lr_type[2]) << AV1_CRFRAME_RESTORATION_TYPE)
            | (u16::from(header.lr_unit_shift) << AV1_LR_UNIT_SHIFT)
            | (u16::from(header.lr_uv_shift) << AV1_LR_UV_SHIFT)
    }

    /// Restoration type remapped onto the VA-API order.
    fn restoration_type(kind: u8) -> u16 {
        match kind {
            1 => 2,
            2 => 3,
            3 => 1,
            _ => 0,
        }
    }

    /// Warped-motion parameters by `LAST` to `ALTREF` order.
    fn warped_motion(motion: &[GlobalMotion; REFS_PER_FRAME]) -> [VaWarpedMotionAv1; 7] {
        let mut wm = [VaWarpedMotionAv1 {
            wmtype: 0,
            wmmat: [0; 8],
            invalid: 0,
            va_reserved: [0; 4],
        }; 7];
        for (slot, params) in wm.iter_mut().zip(motion.iter()) {
            slot.wmtype = c_int::from(params.warp_type);
            slot.wmmat[..6].copy_from_slice(&params.params);
            slot.invalid = u8::from(params.invalid);
        }
        wm
    }
}

#[cfg(test)]
mod tests {
    use rust_video_av1::{
        FG_APPLY, FH_ERROR_RESILIENT, FH_SHOWABLE_FRAME, FH_SHOW_FRAME, FH_USING_QMATRIX,
        SEQ_ORDER_HINT, SEQ_STILL_PICTURE, SEQ_SUPERBLOCK_128,
    };

    use super::Params;
    use crate::av1::{FrameHeader, FrameType, SequenceHeader};
    use crate::av1_frontend::{Tile, NUM_SLOTS};
    use crate::error::VaApiError;
    use crate::h264_frontend::SurfaceSlot;

    /// Surfaces backing pool slots.
    const SURFACES: [u32; NUM_SLOTS + 4] = [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21];

    /// Sequence header for Profile 0 8-bit 4:2:0.
    fn sequence() -> SequenceHeader {
        rust_video_av1::parse_sequence_header::<VaApiError>(&crate::fixtures::seq_payload())
            .expect("fixture sequence")
    }

    /// Frame header for a shown 64x64 keyframe.
    fn keyframe() -> FrameHeader {
        FrameHeader {
            frame_type: FrameType::Key,
            flags: FH_SHOW_FRAME,
            frame_width: 64,
            frame_height: 64,
            upscaled_width: 64,
            render_width: 64,
            render_height: 64,
            superres_denom: 8,
            tile_cols: 1,
            tile_rows: 1,
            ..FrameHeader::default()
        }
    }

    /// Reference slots with all entries unset.
    fn empty_refs() -> [Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS] {
        [None; NUM_SLOTS]
    }

    #[test]
    fn profile_maps_profile_zero() {
        assert_eq!(
            Params::profile(0).expect("profile 0"),
            crate::ffi::VA_PROFILE_AV1_PROFILE0
        );
        assert!(matches!(
            Params::profile(1),
            Err(VaApiError::Unsupported(_))
        ));
        assert!(matches!(
            Params::profile(2),
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
        seq.flags |= SEQ_STILL_PICTURE | SEQ_SUPERBLOCK_128 | SEQ_ORDER_HINT;
        let mut header = keyframe();
        header.flags |= FH_SHOW_FRAME | FH_SHOWABLE_FRAME | FH_ERROR_RESILIENT;
        header.uniform_tile_spacing = true;
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
        assert_eq!(
            picture.pic_info_fields & 0x3,
            u32::from(FrameType::Key.id()) & 0x3
        );
    }

    #[test]
    fn picture_maps_segmentation_and_grain() {
        let seq = sequence();
        let mut header = keyframe();
        header.flags |= rust_video_av1::FH_SEG_ENABLED | rust_video_av1::FH_SEG_UPDATE_MAP;
        header.seg_enabled[0][0] = true;
        header.seg_value[0][0] = -4;
        header.film_grain.flags |= FG_APPLY;
        header.film_grain.grain_seed = 7;
        header.film_grain.num_y_points = 2;
        header.film_grain.scaling_points_y[0] = [9, 0];
        header.film_grain.ar_coeffs_y[0] = 128 + 5;
        header.film_grain.ar_coeffs_cb[0] = 128 - 3;
        header.film_grain.ar_coeffs_cr[24] = 128;
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
        header.lr_type = [1, 3, 2];
        header.global_motion[0].warp_type = 1;
        header.global_motion[0].params = [3, 5, 0, 0, 0, 0];
        header.global_motion[0].invalid = true;
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
        header.cdef_damping_minus_3 = 2;
        header.cdef_bits = 2;
        header.cdef_y_pri[0] = 9;
        header.cdef_y_sec[0] = 4;
        header.base_q_idx = 128;
        header.delta_q_y_dc = -2;
        header.flags |= FH_USING_QMATRIX;
        header.qm_y = 5;
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
        header.qm_u = 16;
        assert!(matches!(
            Params::new(&SURFACES).picture(&seq, &header, &empty_refs(), 18, 18),
            Err(VaApiError::Parse(_))
        ));
    }

    #[test]
    fn picture_maps_tiles_and_filters() {
        let seq = sequence();
        let mut header = keyframe();
        header.tile_cols = 2;
        header.width_in_sbs_minus_1[0] = 3;
        header.width_in_sbs_minus_1[1] = 5;
        header.context_update_tile_id = 1;
        header.loop_filter_level = [6, 6, 4, 4];
        header.loop_filter_sharpness = 3;
        header.flags |= rust_video_av1::FH_LOOP_DELTA_ENABLED;
        header.loop_ref_deltas[0] = 2;
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
        header.tile_cols = 64;
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
        };
        let slice = Params::slice(&tile, 16, 32).expect("slice maps");
        assert_eq!(slice.slice_data_size, 32);
        assert_eq!(slice.slice_data_offset, 16);
        assert_eq!(slice.slice_data_flag, crate::ffi::VA_SLICE_DATA_FLAG_ALL);
        assert_eq!(slice.tile_row, 1);
        assert_eq!(slice.tile_column, 2);
        assert_eq!(slice.tg_start, 0);
        let tile = Tile {
            offset: 0,
            size: 0,
            row: u32::MAX,
            col: 0,
        };
        assert!(matches!(
            Params::slice(&tile, 0, 0),
            Err(VaApiError::Parse(_))
        ));
    }
}
