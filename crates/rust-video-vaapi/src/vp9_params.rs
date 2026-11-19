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
use crate::vp9::{FrameHeader, MAX_SEGMENTS};
use crate::vp9_frontend::NUM_SLOTS;
use crate::vp9_lookups::{
    AC_QLOOKUP, AC_QLOOKUP_10, AC_QLOOKUP_12, DC_QLOOKUP, DC_QLOOKUP_10, DC_QLOOKUP_12,
};

/// Last-frame reference index, shared by `Params` and `Segmentation`.
const LAST_FRAME: usize = 1;
/// Golden-frame reference index, shared by `Params` and `Segmentation`.
const GOLDEN_FRAME: usize = 2;
/// Altref-frame reference index, shared by `Params` and `Segmentation`.
const ALTREF_FRAME: usize = 3;

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
            0 => Ok(crate::ffi::VA_PROFILE_VP9_PROFILE0),
            other => Err(VaApiError::Unsupported(format!("vp9 profile {other}"))),
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
        header: &FrameHeader,
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
            frame_width: u16::try_from(header.frame_width).map_err(|_| range("frame width"))?,
            frame_height: u16::try_from(header.frame_height).map_err(|_| range("frame height"))?,
            reference_frames,
            pic_fields: Self::pic_fields(header),
            filter_level: header.filter_level,
            sharpness_level: header.sharpness,
            log2_tile_rows: header.tile_rows_log2,
            log2_tile_columns: header.tile_cols_log2,
            frame_header_length_in_bytes: u8::try_from(header.uncompressed_size)
                .map_err(|_| range("header length"))?,
            first_partition_size: header.header_size_in_bytes,
            mb_segment_tree_probs: header.tree_probs,
            segment_pred_probs: header.pred_probs,
            profile: header.profile,
            bit_depth: header.bit_depth,
            va_reserved: [0; 8],
        })
    }

    /// Slice-parameter buffer for a frame of `size` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt reference id falls outside the
    /// parameter range.
    pub fn slice(
        header: &FrameHeader,
        size: usize,
    ) -> Result<VaSliceParameterBufferVp9, VaApiError> {
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
        let live = if header.segmentation_enabled() {
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
    fn pic_fields(header: &FrameHeader) -> u32 {
        use crate::ffi::{
            VP9_ALLOW_HIGH_PRECISION_MV, VP9_ALT_REF_FRAME, VP9_ALT_REF_FRAME_SIGN_BIAS,
            VP9_ERROR_RESILIENT_MODE, VP9_FRAME_CONTEXT_IDX, VP9_FRAME_PARALLEL_DECODING_MODE,
            VP9_FRAME_TYPE, VP9_GOLDEN_REF_FRAME, VP9_GOLDEN_REF_FRAME_SIGN_BIAS, VP9_INTRA_ONLY,
            VP9_LAST_REF_FRAME, VP9_LAST_REF_FRAME_SIGN_BIAS, VP9_LOSSLESS_FLAG,
            VP9_MCOMP_FILTER_TYPE, VP9_REFRESH_FRAME_CONTEXT, VP9_RESET_FRAME_CONTEXT,
            VP9_SEGMENTATION_ENABLED, VP9_SEGMENTATION_TEMPORAL_UPDATE,
            VP9_SEGMENTATION_UPDATE_MAP, VP9_SHOW_FRAME, VP9_SUBSAMPLING_X, VP9_SUBSAMPLING_Y,
        };
        (u32::from(header.subsampling_x()) << VP9_SUBSAMPLING_X)
            | (u32::from(header.subsampling_y()) << VP9_SUBSAMPLING_Y)
            | (u32::from(!header.frame_type.is_intra()) << VP9_FRAME_TYPE)
            | (u32::from(header.show_frame()) << VP9_SHOW_FRAME)
            | (u32::from(header.error_resilient_mode()) << VP9_ERROR_RESILIENT_MODE)
            | (u32::from(header.intra_only()) << VP9_INTRA_ONLY)
            | (u32::from(header.allow_high_precision_mv()) << VP9_ALLOW_HIGH_PRECISION_MV)
            | (u32::from(header.interp_filter) << VP9_MCOMP_FILTER_TYPE)
            | (u32::from(header.frame_parallel_decoding_mode()) << VP9_FRAME_PARALLEL_DECODING_MODE)
            | (u32::from(header.reset_frame_context) << VP9_RESET_FRAME_CONTEXT)
            | (u32::from(header.refresh_frame_context()) << VP9_REFRESH_FRAME_CONTEXT)
            | (u32::from(header.frame_context_idx) << VP9_FRAME_CONTEXT_IDX)
            | (u32::from(header.segmentation_enabled()) << VP9_SEGMENTATION_ENABLED)
            | (u32::from(header.segmentation_temporal_update()) << VP9_SEGMENTATION_TEMPORAL_UPDATE)
            | (u32::from(header.segmentation_update_map()) << VP9_SEGMENTATION_UPDATE_MAP)
            | (u32::from(header.ref_frame_idx[0]) << VP9_LAST_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[LAST_FRAME]) << VP9_LAST_REF_FRAME_SIGN_BIAS)
            | (u32::from(header.ref_frame_idx[1]) << VP9_GOLDEN_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[GOLDEN_FRAME])
                << VP9_GOLDEN_REF_FRAME_SIGN_BIAS)
            | (u32::from(header.ref_frame_idx[2]) << VP9_ALT_REF_FRAME)
            | (u32::from(header.ref_frame_sign_bias[ALTREF_FRAME]) << VP9_ALT_REF_FRAME_SIGN_BIAS)
            | (u32::from(Self::lossless(header)) << VP9_LOSSLESS_FLAG)
    }

    /// True for bit-identical frames: zero base quantizer and deltas.
    fn lossless(header: &FrameHeader) -> bool {
        header.base_q_idx == 0
            && header.delta_q_y_dc == 0
            && header.delta_q_uv_dc == 0
            && header.delta_q_uv_ac == 0
    }

    /// Slice segment entry for `segment`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a corrupt reference id falls outside the
    /// parameter range.
    fn segment(
        header: &FrameHeader,
        segment: &Segmentation,
    ) -> Result<VaSegmentParameterVp9, VaApiError> {
        use crate::ffi::{VP9_SEG_REFERENCE, VP9_SEG_REFERENCE_ENABLED, VP9_SEG_REFERENCE_SKIPPED};
        let enabled = header.segmentation_enabled();
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

/// Resolved per-segment submit values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segmentation {
    /// Loop filter level
    pub lvl_lookup: [[u8; 2]; 4],

    /// AC quant scale for luma component
    pub luma_ac_quant_scale: i16,
    /// DC quant scale for luma component
    pub luma_dc_quant_scale: i16,
    /// AC quant scale for chroma component
    pub chroma_ac_quant_scale: i16,
    /// DC quant scale for chroma component
    pub chroma_dc_quant_scale: i16,

    /// Whether the alternate reference frame segment feature is enabled (`SEG_LVL_REF_FRAME`)
    pub reference_frame_enabled: bool,
    /// The feature data for the reference frame featire
    pub reference_frame: i16,
    /// Whether the skip segment feature is enabled (`SEG_LVL_SKIP`)
    pub reference_skip_enabled: bool,
}

impl Segmentation {
    /// Intra reference slot.
    const INTRA_FRAME: usize = 0;
    /// Reference slot count.
    const MAX_REF_FRAMES: usize = 4;
    /// Loop filter mode delta count.
    const MAX_MODE_LF_DELTAS: usize = 2;
    /// Loop filter ceiling.
    const MAX_LOOP_FILTER: i32 = 63;
    /// Alternate-quantizer feature level.
    const ALT_Q: usize = 0;
    /// Alternate-filter feature level.
    const ALT_L: usize = 1;
    /// Reference-frame feature level.
    const REF_FRAME: usize = 2;
    /// Skip feature level.
    const SKIP: usize = 3;

    /// Resolve submit values for every segment of `header`.
    pub fn update_segmentation(
        segmentation: &mut [Segmentation; MAX_SEGMENTS],
        header: &FrameHeader,
    ) {
        let n_shift = header.filter_level >> 5;

        for segment_id in 0..u8::try_from(MAX_SEGMENTS).unwrap_or(u8::MAX) {
            let mut lvl_lookup: [[u8; Self::MAX_MODE_LF_DELTAS]; Self::MAX_REF_FRAMES];

            if header.filter_level == 0 {
                lvl_lookup = Default::default();
            } else {
                let mut lvl_seg = i32::from(header.filter_level);

                if Self::feature_active(header, segment_id, Self::ALT_L) {
                    if header.segmentation_abs_delta() {
                        lvl_seg = i32::from(Self::feature_data(header, segment_id, Self::ALT_L));
                    } else {
                        lvl_seg += i32::from(Self::feature_data(header, segment_id, Self::ALT_L));
                    }
                }

                let lvl_seg =
                    u8::try_from(lvl_seg.clamp(0, Self::MAX_LOOP_FILTER)).unwrap_or(u8::MAX);

                if header.loop_filter_delta_enabled() {
                    let intra_delta = i32::from(header.ref_deltas[Self::INTRA_FRAME]);
                    let mut intra_lvl = i32::from(lvl_seg) + (intra_delta << n_shift);

                    lvl_lookup = segmentation[usize::from(segment_id)].lvl_lookup;
                    lvl_lookup[Self::INTRA_FRAME][0] =
                        u8::try_from(intra_lvl.clamp(0, Self::MAX_LOOP_FILTER)).unwrap_or(u8::MAX);

                    #[allow(clippy::needless_range_loop)]
                    for ref_ in LAST_FRAME..Self::MAX_REF_FRAMES {
                        for mode in 0..Self::MAX_MODE_LF_DELTAS {
                            let ref_delta = i32::from(header.ref_deltas[ref_]);
                            let mode_delta = i32::from(header.mode_deltas[mode]);

                            intra_lvl = i32::from(lvl_seg)
                                + (ref_delta << n_shift)
                                + (mode_delta << n_shift);

                            lvl_lookup[ref_][mode] =
                                u8::try_from(intra_lvl.clamp(0, Self::MAX_LOOP_FILTER))
                                    .unwrap_or(u8::MAX);
                        }
                    }
                } else {
                    lvl_lookup = [[lvl_seg; Self::MAX_MODE_LF_DELTAS]; Self::MAX_REF_FRAMES];
                }
            }

            segmentation[usize::from(segment_id)] = Segmentation {
                lvl_lookup,
                luma_ac_quant_scale: Self::ac_quant(header, segment_id, true),
                luma_dc_quant_scale: Self::dc_quant(header, segment_id, true),
                chroma_ac_quant_scale: Self::ac_quant(header, segment_id, false),
                chroma_dc_quant_scale: Self::dc_quant(header, segment_id, false),
                reference_frame_enabled: Self::feature_enabled(header, segment_id, Self::REF_FRAME),
                reference_frame: Self::feature_data(header, segment_id, Self::REF_FRAME),
                reference_skip_enabled: Self::feature_enabled(header, segment_id, Self::SKIP),
            }
        }
    }

    /// True when `feature` of `segment_id` affects decode.
    fn feature_active(header: &FrameHeader, segment_id: u8, feature: usize) -> bool {
        header.segmentation_enabled() && Self::feature_enabled(header, segment_id, feature)
    }

    /// True when `feature` of `segment_id` carries coded data.
    fn feature_enabled(header: &FrameHeader, segment_id: u8, feature: usize) -> bool {
        header.seg_enabled[usize::from(segment_id)][feature]
    }

    /// Coded data of `feature` on `segment_id`.
    fn feature_data(header: &FrameHeader, segment_id: u8, feature: usize) -> i16 {
        header.seg_value[usize::from(segment_id)][feature]
    }

    /// Quantizer index of `segment_id` per 8.6.1.
    fn qindex(header: &FrameHeader, segment_id: u8) -> u8 {
        let base_q_idx = header.base_q_idx;

        if Self::feature_active(header, segment_id, Self::ALT_Q) {
            let mut data = i32::from(Self::feature_data(header, segment_id, Self::ALT_Q));

            if !header.segmentation_abs_delta() {
                data += i32::from(base_q_idx);
            }

            u8::try_from(data.clamp(0, 255)).unwrap_or(u8::MAX)
        } else {
            base_q_idx
        }
    }

    /// DC quantizer of `segment_id` per 8.6.1.
    fn dc_quant(header: &FrameHeader, segment_id: u8, luma: bool) -> i16 {
        let delta_q_dc = i32::from(if luma {
            header.delta_q_y_dc
        } else {
            header.delta_q_uv_dc
        });
        let qindex = Self::qindex(header, segment_id);
        let q_table_idx =
            u8::try_from((i32::from(qindex) + delta_q_dc).clamp(0, 255)).unwrap_or(u8::MAX);

        let table = match header.bit_depth {
            10 => &DC_QLOOKUP_10,
            12 => &DC_QLOOKUP_12,
            _ => &DC_QLOOKUP,
        };

        table[usize::from(q_table_idx)]
    }

    /// AC quantizer of `segment_id` per 8.6.1.
    fn ac_quant(header: &FrameHeader, segment_id: u8, luma: bool) -> i16 {
        let delta_q_ac = i32::from(if luma { 0 } else { header.delta_q_uv_ac });
        let qindex = Self::qindex(header, segment_id);
        let q_table_idx =
            u8::try_from((i32::from(qindex) + delta_q_ac).clamp(0, 255)).unwrap_or(u8::MAX);

        let table = match header.bit_depth {
            10 => &AC_QLOOKUP_10,
            12 => &AC_QLOOKUP_12,
            _ => &AC_QLOOKUP,
        };

        table[usize::from(q_table_idx)]
    }
}
