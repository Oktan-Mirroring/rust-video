//! VP9 VA-API parameter buffers from parsed syntax.
//!
//! Portable: pure mapping from frame syntax onto the `ffi` layouts; no
//! display handle needed.

use std::os::raw::c_int;

use crate::error::VaApiError;
use crate::ffi::{
    VaDecPictureParameterBufferVp9, VaSegmentParameterVp9, VaSliceParameterBufferVp9, VaSurfaceId,
};
use crate::frontend::SurfaceSlot;
use crate::vp9::frontend::NUM_SLOTS;
use crate::vp9::parser::{
    Header, Profile, Segmentation, ALTREF_FRAME, GOLDEN_FRAME, LAST_FRAME, MAX_SEGMENTS,
};

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
            Profile::Profile0 => Ok(crate::ffi::VA_PROFILE_VP9_PROFILE0),
            other => Err(VaApiError::Unsupported(format!(
                "vp9 profile {}",
                other as u8
            ))),
        }
    }

    /// Picture-parameter buffer for `header`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a reference slot lost its surface or a
    /// corrupt size falls outside the parameter range.
    pub fn picture(
        &self,
        header: &Header,
        refs: &[Option<(SurfaceSlot, (u32, u32))>; NUM_SLOTS],
    ) -> Result<VaDecPictureParameterBufferVp9, VaApiError> {
        let range = |what: &str| VaApiError::Parse(format!("picture {what} range"));
        let mut reference_frames = [crate::ffi::VA_INVALID_ID; NUM_SLOTS];
        for (surface, entry) in reference_frames.iter_mut().zip(refs.iter()) {
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
        Ok(VaDecPictureParameterBufferVp9 {
            frame_width: u16::try_from(header.width).map_err(|_| range("frame width"))?,
            frame_height: u16::try_from(header.height).map_err(|_| range("frame height"))?,
            reference_frames,
            pic_fields: Self::pic_fields(header),
            filter_level: header.lf.level,
            sharpness_level: header.lf.sharpness,
            log2_tile_rows: header.tile_rows_log2,
            log2_tile_columns: header.tile_cols_log2,
            frame_header_length_in_bytes: u8::try_from(header.uncompressed_header_size_in_bytes)
                .map_err(|_| range("header length"))?,
            first_partition_size: header.compressed_header_size_in_bytes,
            mb_segment_tree_probs: header.seg.tree_probs,
            segment_pred_probs: header.seg.pred_probs,
            profile: header.profile as u8,
            bit_depth: header.color.bit_depth as u8,
            va_reserved: [0; 8],
        })
    }

    /// Slice-parameter buffer for a frame of `size` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt frame size falls outside the
    /// parameter range.
    pub fn slice(header: &Header, size: usize) -> Result<VaSliceParameterBufferVp9, VaApiError> {
        let mut segments: [Segmentation; MAX_SEGMENTS] = Default::default();
        Segmentation::update_segmentation(&mut segments, header);
        let mut seg_param = [VaSegmentParameterVp9 {
            segment_flags: 0,
            filter_level: [[0; 2]; 4],
            luma_ac_quant_scale: 0,
            luma_dc_quant_scale: 0,
            chroma_ac_quant_scale: 0,
            chroma_dc_quant_scale: 0,
            va_reserved: [0; 4],
        }; MAX_SEGMENTS];
        let live = if header.seg.map.enabled {
            MAX_SEGMENTS
        } else {
            1
        };
        for (out, segment) in seg_param.iter_mut().zip(segments.iter()).take(live) {
            *out = Self::segment(header, segment)?;
        }
        Ok(VaSliceParameterBufferVp9 {
            slice_data_size: u32::try_from(size)
                .map_err(|_| VaApiError::Parse("frame size range".to_owned()))?,
            slice_data_offset: 0,
            slice_data_flag: crate::ffi::VA_SLICE_DATA_FLAG_ALL,
            seg_param,
            va_reserved: [0; 4],
        })
    }

    /// Packed picture flags.
    fn pic_fields(header: &Header) -> u32 {
        use crate::ffi::{
            VP9_ALLOW_HIGH_PRECISION_MV, VP9_ALT_REF_FRAME, VP9_ALT_REF_FRAME_SIGN_BIAS,
            VP9_ERROR_RESILIENT_MODE, VP9_FRAME_CONTEXT_IDX, VP9_FRAME_PARALLEL_DECODING_MODE,
            VP9_FRAME_TYPE, VP9_GOLDEN_REF_FRAME, VP9_GOLDEN_REF_FRAME_SIGN_BIAS, VP9_INTRA_ONLY,
            VP9_LAST_REF_FRAME, VP9_LAST_REF_FRAME_SIGN_BIAS, VP9_LOSSLESS_FLAG,
            VP9_MCOMP_FILTER_TYPE, VP9_REFRESH_FRAME_CONTEXT, VP9_RESET_FRAME_CONTEXT,
            VP9_SEGMENTATION_ENABLED, VP9_SEGMENTATION_TEMPORAL_UPDATE,
            VP9_SEGMENTATION_UPDATE_MAP, VP9_SHOW_FRAME, VP9_SUBSAMPLING_X, VP9_SUBSAMPLING_Y,
        };
        (u32::from(header.color.subsampling_x) << VP9_SUBSAMPLING_X)
            | (u32::from(header.color.subsampling_y) << VP9_SUBSAMPLING_Y)
            | ((header.frame_type as u32) << VP9_FRAME_TYPE)
            | (u32::from(header.show.show_frame) << VP9_SHOW_FRAME)
            | (u32::from(header.resilience.error_resilient_mode) << VP9_ERROR_RESILIENT_MODE)
            | (u32::from(header.mode.intra_only) << VP9_INTRA_ONLY)
            | (u32::from(header.mode.allow_high_precision_mv) << VP9_ALLOW_HIGH_PRECISION_MV)
            | ((header.interpolation_filter as u32) << VP9_MCOMP_FILTER_TYPE)
            | (u32::from(header.resilience.frame_parallel_decoding_mode)
                << VP9_FRAME_PARALLEL_DECODING_MODE)
            | (u32::from(header.reset_frame_context) << VP9_RESET_FRAME_CONTEXT)
            | (u32::from(header.resilience.refresh_frame_context) << VP9_REFRESH_FRAME_CONTEXT)
            | (u32::from(header.frame_context_idx) << VP9_FRAME_CONTEXT_IDX)
            | (u32::from(header.seg.map.enabled) << VP9_SEGMENTATION_ENABLED)
            | (u32::from(header.seg.map.temporal_update) << VP9_SEGMENTATION_TEMPORAL_UPDATE)
            | (u32::from(header.seg.map.update_map) << VP9_SEGMENTATION_UPDATE_MAP)
            | (u32::from(header.ref_frame_idx[0]) << VP9_LAST_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[LAST_FRAME]) << VP9_LAST_REF_FRAME_SIGN_BIAS)
            | (u32::from(header.ref_frame_idx[1]) << VP9_GOLDEN_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[GOLDEN_FRAME])
                << VP9_GOLDEN_REF_FRAME_SIGN_BIAS)
            | (u32::from(header.ref_frame_idx[2]) << VP9_ALT_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[ALTREF_FRAME]) << VP9_ALT_REF_FRAME_SIGN_BIAS)
            | (u32::from(header.lossless) << VP9_LOSSLESS_FLAG)
    }

    /// Slice segment entry for `segment`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt reference id falls outside the
    /// parameter range.
    fn segment(
        header: &Header,
        segment: &Segmentation,
    ) -> Result<VaSegmentParameterVp9, VaApiError> {
        use crate::ffi::{VP9_SEG_REFERENCE, VP9_SEG_REFERENCE_ENABLED, VP9_SEG_REFERENCE_SKIPPED};
        let enabled = header.seg.map.enabled;
        let reference = u16::try_from(segment.reference_frame)
            .map_err(|_| VaApiError::Parse("segment reference range".to_owned()))?;
        Ok(VaSegmentParameterVp9 {
            segment_flags: (u16::from(enabled && segment.reference_frame_enabled)
                << VP9_SEG_REFERENCE_ENABLED)
                | ((reference & 0x3) << VP9_SEG_REFERENCE)
                | (u16::from(enabled && segment.reference_skip_enabled)
                    << VP9_SEG_REFERENCE_SKIPPED),
            filter_level: segment.lvl_lookup,
            luma_ac_quant_scale: segment.luma_ac_quant_scale,
            luma_dc_quant_scale: segment.luma_dc_quant_scale,
            chroma_ac_quant_scale: segment.chroma_ac_quant_scale,
            chroma_dc_quant_scale: segment.chroma_dc_quant_scale,
            va_reserved: [0; 4],
        })
    }
}
