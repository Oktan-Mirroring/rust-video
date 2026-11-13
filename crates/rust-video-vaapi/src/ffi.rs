//! Minimal libva rebind for decode, encode, and video processing over DRM.
//!
//! Layouts mirror `/usr/include/va/va.h`, `va_dec_hevc.h`, and
//! `va_dec_av1.h` from libva 1.22.0; the byte sizes pinned below are
//! that contract.

#![allow(unsafe_code)]

use std::os::raw::c_int;
use std::os::raw::{c_char, c_uint, c_ushort, c_void};

/// Opaque VA-API display handle.
pub type VaDisplay = *mut c_void;
/// VA-API call status.
pub type VaStatus = c_int;
/// VA-API surface handle.
pub type VaSurfaceId = u32;
/// VA-API context handle.
pub type VaContextId = u32;
/// VA-API buffer handle.
pub type VaBufferId = u32;
/// VA-API config handle.
pub type VaConfigId = u32;
/// VA-API image handle.
pub type VaImageId = u32;

/// Successful VA-API call.
pub const VA_STATUS_SUCCESS: VaStatus = 0;
/// Invalid VA-API handle.
pub const VA_INVALID_ID: u32 = 0xffff_ffff;
/// H.264 Constrained Baseline profile.
pub const VA_PROFILE_H264_CONSTRAINED_BASELINE: c_int = 13;
/// H.264 Main profile.
pub const VA_PROFILE_H264_MAIN: c_int = 6;
/// H.264 High profile.
pub const VA_PROFILE_H264_HIGH: c_int = 7;
/// Variable-length decode entrypoint.
pub const VA_ENTRYPOINT_VLD: c_int = 1;
/// 8-bit 4:2:0 render-target format.
pub const VA_RT_FORMAT_YUV420: c_uint = 0x0000_0001;
/// Render-target attribute type.
pub const VA_CONFIG_ATTRIB_RT_FORMAT: c_int = 0;
/// Attribute unsupported marker.
pub const VA_ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;
/// Picture-parameter buffer kind.
pub const VA_PICTURE_PARAMETER_BUFFER_TYPE: c_int = 0;
/// Inverse-quantization-matrix buffer kind.
pub const VA_IQ_MATRIX_BUFFER_TYPE: c_int = 1;
/// Slice-parameter buffer kind.
pub const VA_SLICE_PARAMETER_BUFFER_TYPE: c_int = 4;
/// Slice-data buffer kind.
pub const VA_SLICE_DATA_BUFFER_TYPE: c_int = 5;
/// Whole slice carried in one buffer.
pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0x00;
/// NV12 image fourcc.
pub const VA_FOURCC_NV12: u32 = 0x3231_564e;
/// Progressive-only context flag.
pub const VA_PROGRESSIVE: c_int = 0x1;
/// HEVC Main profile.
pub const VA_PROFILE_HEVC_MAIN: c_int = 17;
/// AV1 Profile 0.
pub const VA_PROFILE_AV1_PROFILE0: c_int = 32;
/// No profile, for video processing configs.
pub const VA_PROFILE_NONE: c_int = -1;
/// Slice-level encode entrypoint.
pub const VA_ENTRYPOINT_ENC_SLICE: c_int = 6;
/// Video processing entrypoint.
pub const VA_ENTRYPOINT_VIDEO_PROC: c_int = 10;
/// Rate-control attribute type.
pub const VA_CONFIG_ATTRIB_RATE_CONTROL: c_int = 5;
/// Packed-header attribute type.
pub const VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS: c_int = 10;
/// Constant-QP rate control.
pub const VA_RC_CQP: u32 = 0x0000_0010;
/// Packed sequence header support flag.
pub const VA_ENC_PACKED_HEADER_SEQUENCE: u32 = 0x0000_0001;
/// Packed picture header support flag.
pub const VA_ENC_PACKED_HEADER_PICTURE: u32 = 0x0000_0002;
/// Packed slice header support flag.
pub const VA_ENC_PACKED_HEADER_SLICE: u32 = 0x0000_0004;
/// Coded-bitstream buffer kind.
pub const VA_ENC_CODED_BUFFER_TYPE: c_int = 21;
/// Encode sequence-parameter buffer kind.
pub const VA_ENC_SEQUENCE_PARAMETER_BUFFER_TYPE: c_int = 22;
/// Encode picture-parameter buffer kind.
pub const VA_ENC_PICTURE_PARAMETER_BUFFER_TYPE: c_int = 23;
/// Encode slice-parameter buffer kind.
pub const VA_ENC_SLICE_PARAMETER_BUFFER_TYPE: c_int = 24;
/// Packed-header parameter buffer kind.
pub const VA_ENC_PACKED_HEADER_PARAMETER_BUFFER_TYPE: c_int = 25;
/// Packed-header data buffer kind.
pub const VA_ENC_PACKED_HEADER_DATA_BUFFER_TYPE: c_int = 26;
/// Packed sequence header type.
pub const VA_ENC_PACKED_HEADER_TYPE_SEQUENCE: u32 = 1;
/// Packed picture header type.
pub const VA_ENC_PACKED_HEADER_TYPE_PICTURE: u32 = 2;
/// Packed slice header type.
pub const VA_ENC_PACKED_HEADER_TYPE_SLICE: u32 = 3;
/// Packed raw header type.
pub const VA_ENC_PACKED_HEADER_TYPE_RAW_DATA: u32 = 4;
/// Video-processing pipeline-parameter buffer kind.
pub const VA_PROC_PIPELINE_PARAMETER_BUFFER_TYPE: c_int = 41;
/// Default processing mode.
pub const VA_PROC_DEFAULT_MODE: c_int = 0;
/// Unspecified input/output color standard.
pub const VA_PROC_COLOR_STANDARD_NONE: c_int = 0;
/// DRM PRIME version-two memory type.
pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 0x4000_0000;
/// Export handle for reading by the importer.
pub const VA_EXPORT_SURFACE_READ_ONLY: u32 = 0x0001;
/// Export planes as separate layers.
pub const VA_EXPORT_SURFACE_SEPARATE_LAYERS: u32 = 0x0004;

/// `VAPictureH264` reference flags.
pub const VA_PICTURE_H264_INVALID: u32 = 0x0000_0001;
/// Short-term reference flag.
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x0000_0008;
/// Long-term reference flag.
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x0000_0010;

/// HEVC invalid-picture flag.
pub const VA_PICTURE_HEVC_INVALID: u32 = 0x0000_0001;
/// HEVC long-term reference flag.
pub const VA_PICTURE_HEVC_LONG_TERM_REFERENCE: u32 = 0x0000_0008;
/// HEVC short-term current-before-RPS flag.
pub const VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 0x0000_0010;
/// HEVC short-term current-after-RPS flag.
pub const VA_PICTURE_HEVC_RPS_ST_CURR_AFTER: u32 = 0x0000_0020;
/// HEVC long-term current-RPS flag.
pub const VA_PICTURE_HEVC_RPS_LT_CURR: u32 = 0x0000_0040;

/// `seq_fields` bit shifts inside `VAPictureParameterBufferH264`.
pub const SEQ_CHROMA_FORMAT_IDC: u32 = 0;
/// Separate-colour-plane bit shift.
pub const SEQ_SEPARATE_COLOUR_PLANE: u32 = 2;
/// Frame-number-gap bit shift.
pub const SEQ_GAPS_IN_FRAME_NUM: u32 = 3;
/// Frame-only bit shift.
pub const SEQ_FRAME_MBS_ONLY: u32 = 4;
/// MBAFF bit shift.
pub const SEQ_MBAFF: u32 = 5;
/// Direct-8x8-inference bit shift.
pub const SEQ_DIRECT_8X8_INFERENCE: u32 = 6;
/// Bipred-8x8 bit shift.
pub const SEQ_MIN_LUMA_BI_PRED_SIZE_8X8: u32 = 7;
/// Frame-number-bits bit shift.
pub const SEQ_LOG2_MAX_FRAME_NUM_MINUS4: u32 = 8;
/// Picture-order-count-type bit shift.
pub const SEQ_PIC_ORDER_CNT_TYPE: u32 = 12;
/// Picture-order-count-bits bit shift.
pub const SEQ_LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4: u32 = 14;
/// Always-zero-delta bit shift.
pub const SEQ_DELTA_PIC_ORDER_ALWAYS_ZERO: u32 = 18;

/// `pic_fields` bit shifts inside `VAPictureParameterBufferH264`.
pub const PIC_ENTROPY_CODING_MODE: u32 = 0;
/// Weighted-prediction bit shift.
pub const PIC_WEIGHTED_PRED: u32 = 1;
/// Weighted-bipred-idc bit shift.
pub const PIC_WEIGHTED_BIPRED_IDC: u32 = 2;
/// Transform-8x8 bit shift.
pub const PIC_TRANSFORM_8X8_MODE: u32 = 4;
/// Field-picture bit shift.
pub const PIC_FIELD_PIC: u32 = 5;
/// Constrained-intra-prediction bit shift.
pub const PIC_CONSTRAINED_INTRA_PRED: u32 = 6;
/// Bottom-field-order-present bit shift.
pub const PIC_ORDER_PRESENT: u32 = 7;
/// Deblocking-present bit shift.
pub const PIC_DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 8;
/// Redundant-picture-count-present bit shift.
pub const PIC_REDUNDANT_PIC_CNT_PRESENT: u32 = 9;
/// Reference-picture bit shift.
pub const PIC_REFERENCE_PIC: u32 = 10;

/// `pic_fields` bit shifts inside `VAPictureParameterBufferHEVC`.
pub const HEVC_CHROMA_FORMAT_IDC: u32 = 0;
/// Separate-colour-plane bit shift.
pub const HEVC_SEPARATE_COLOUR_PLANE: u32 = 2;
/// PCM-enabled bit shift.
pub const HEVC_PCM_ENABLED: u32 = 3;
/// Scaling-list-enabled bit shift.
pub const HEVC_SCALING_LIST_ENABLED: u32 = 4;
/// Transform-skip-enabled bit shift.
pub const HEVC_TRANSFORM_SKIP_ENABLED: u32 = 5;
/// Asymmetric-partition bit shift.
pub const HEVC_AMP_ENABLED: u32 = 6;
/// Strong-intra-smoothing bit shift.
pub const HEVC_STRONG_INTRA_SMOOTHING_ENABLED: u32 = 7;
/// Sign-data-hiding bit shift.
pub const HEVC_SIGN_DATA_HIDING_ENABLED: u32 = 8;
/// Constrained-intra-prediction bit shift.
pub const HEVC_CONSTRAINED_INTRA_PRED: u32 = 9;
/// CU-QP-delta-enabled bit shift.
pub const HEVC_CU_QP_DELTA_ENABLED: u32 = 10;
/// Weighted-prediction bit shift.
pub const HEVC_WEIGHTED_PRED: u32 = 11;
/// Weighted-biprediction bit shift.
pub const HEVC_WEIGHTED_BIPRED: u32 = 12;
/// Transquant-bypass-enabled bit shift.
pub const HEVC_TRANSQUANT_BYPASS_ENABLED: u32 = 13;
/// Tiles-enabled bit shift.
pub const HEVC_TILES_ENABLED: u32 = 14;
/// Entropy-coding-sync-enabled bit shift.
pub const HEVC_ENTROPY_CODING_SYNC_ENABLED: u32 = 15;
/// PPS loop-filter-across-slices bit shift.
pub const HEVC_PPS_LOOP_FILTER_ACROSS_SLICES: u32 = 16;
/// Loop-filter-across-tiles bit shift.
pub const HEVC_LOOP_FILTER_ACROSS_TILES: u32 = 17;
/// PCM-loop-filter-disabled bit shift.
pub const HEVC_PCM_LOOP_FILTER_DISABLED: u32 = 18;

/// `slice_parsing_fields` bit shifts inside `VAPictureParameterBufferHEVC`.
pub const HEVC_LISTS_MODIFICATION_PRESENT: u32 = 0;
/// Long-term-reference-pictures-present bit shift.
pub const HEVC_LONG_TERM_REF_PICS_PRESENT: u32 = 1;
/// SPS-temporal-MVP-enabled bit shift.
pub const HEVC_SPS_TEMPORAL_MVP_ENABLED: u32 = 2;
/// CABAC-init-present bit shift.
pub const HEVC_CABAC_INIT_PRESENT: u32 = 3;
/// Output-flag-present bit shift.
pub const HEVC_OUTPUT_FLAG_PRESENT: u32 = 4;
/// Dependent-slice-segments-enabled bit shift.
pub const HEVC_DEPENDENT_SLICE_SEGMENTS_ENABLED: u32 = 5;
/// PPS-slice-chroma-QP-offsets-present bit shift.
pub const HEVC_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT: u32 = 6;
/// Sample-adaptive-offset-enabled bit shift.
pub const HEVC_SAMPLE_ADAPTIVE_OFFSET_ENABLED: u32 = 7;
/// Deblocking-filter-override-enabled bit shift.
pub const HEVC_DEBLOCKING_FILTER_OVERRIDE_ENABLED: u32 = 8;
/// PPS-disable-deblocking-filter bit shift.
pub const HEVC_PPS_DISABLE_DEBLOCKING_FILTER: u32 = 9;
/// Slice-segment-header-extension-present bit shift.
pub const HEVC_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u32 = 10;
/// Random-access-picture bit shift.
pub const HEVC_RAP_PIC: u32 = 11;
/// IDR-picture bit shift.
pub const HEVC_IDR_PIC: u32 = 12;
/// Intra-picture bit shift.
pub const HEVC_INTRA_PIC: u32 = 13;

/// `LongSliceFlags` bit shifts inside `VASliceParameterBufferHEVC`.
pub const HEVC_LAST_SLICE_OF_PIC: u32 = 0;
/// Dependent-slice-segment bit shift.
pub const HEVC_DEPENDENT_SLICE_SEGMENT: u32 = 1;
/// Slice-type bit shift.
pub const HEVC_SLICE_TYPE: u32 = 2;
/// Colour-plane-id bit shift.
pub const HEVC_COLOR_PLANE_ID: u32 = 4;
/// Slice-SAO-luma bit shift.
pub const HEVC_SLICE_SAO_LUMA: u32 = 6;
/// Slice-SAO-chroma bit shift.
pub const HEVC_SLICE_SAO_CHROMA: u32 = 7;
/// MVD-L1-zero bit shift.
pub const HEVC_MVD_L1_ZERO: u32 = 8;
/// CABAC-init bit shift.
pub const HEVC_CABAC_INIT: u32 = 9;
/// Slice-temporal-MVP-enabled bit shift.
pub const HEVC_SLICE_TEMPORAL_MVP_ENABLED: u32 = 10;
/// Slice-deblocking-filter-disabled bit shift.
pub const HEVC_SLICE_DEBLOCKING_FILTER_DISABLED: u32 = 11;
/// Collocated-from-L0 bit shift.
pub const HEVC_COLLOCATED_FROM_L0: u32 = 12;
/// Slice-loop-filter-across-slices bit shift.
pub const HEVC_SLICE_LOOP_FILTER_ACROSS_SLICES: u32 = 13;

/// `seq_info_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_STILL_PICTURE: u32 = 0;
/// 128x128-superblock bit shift.
pub const AV1_USE_128X128_SUPERBLOCK: u32 = 1;
/// Filter-intra-enabled bit shift.
pub const AV1_ENABLE_FILTER_INTRA: u32 = 2;
/// Intra-edge-filter-enabled bit shift.
pub const AV1_ENABLE_INTRA_EDGE_FILTER: u32 = 3;
/// Interintra-compound-enabled bit shift.
pub const AV1_ENABLE_INTERINTRA_COMPOUND: u32 = 4;
/// Masked-compound-enabled bit shift.
pub const AV1_ENABLE_MASKED_COMPOUND: u32 = 5;
/// Dual-filter-enabled bit shift.
pub const AV1_ENABLE_DUAL_FILTER: u32 = 6;
/// Order-hint-enabled bit shift.
pub const AV1_ENABLE_ORDER_HINT: u32 = 7;
/// Joint-compound-enabled bit shift.
pub const AV1_ENABLE_JNT_COMP: u32 = 8;
/// CDEF-enabled bit shift.
pub const AV1_ENABLE_CDEF: u32 = 9;
/// Monochrome bit shift.
pub const AV1_MONO_CHROME: u32 = 10;
/// Color-range bit shift.
pub const AV1_COLOR_RANGE: u32 = 11;
/// Horizontal-subsampling bit shift.
pub const AV1_SUBSAMPLING_X: u32 = 12;
/// Vertical-subsampling bit shift.
pub const AV1_SUBSAMPLING_Y: u32 = 13;
/// Film-grain-params-present bit shift.
pub const AV1_FILM_GRAIN_PARAMS_PRESENT: u32 = 15;

/// `pic_info_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_FRAME_TYPE: u32 = 0;
/// Show-frame bit shift.
pub const AV1_SHOW_FRAME: u32 = 2;
/// Showable-frame bit shift.
pub const AV1_SHOWABLE_FRAME: u32 = 3;
/// Error-resilient-mode bit shift.
pub const AV1_ERROR_RESILIENT_MODE: u32 = 4;
/// Disable-CDF-update bit shift.
pub const AV1_DISABLE_CDF_UPDATE: u32 = 5;
/// Allow-screen-content-tools bit shift.
pub const AV1_ALLOW_SCREEN_CONTENT_TOOLS: u32 = 6;
/// Force-integer-motion-vector bit shift.
pub const AV1_FORCE_INTEGER_MV: u32 = 7;
/// Allow-intra-block-copy bit shift.
pub const AV1_ALLOW_INTRABC: u32 = 8;
/// Superres bit shift.
pub const AV1_USE_SUPERRES: u32 = 9;
/// High-precision-motion-vector bit shift.
pub const AV1_ALLOW_HIGH_PRECISION_MV: u32 = 10;
/// Switchable-motion-mode bit shift.
pub const AV1_IS_MOTION_MODE_SWITCHABLE: u32 = 11;
/// Reference-frame-motion-vectors bit shift.
pub const AV1_USE_REF_FRAME_MVS: u32 = 12;
/// Disable-frame-end-CDF-update bit shift.
pub const AV1_DISABLE_FRAME_END_UPDATE_CDF: u32 = 13;
/// Uniform-tile-spacing bit shift.
pub const AV1_UNIFORM_TILE_SPACING: u32 = 14;
/// Warped-motion bit shift.
pub const AV1_ALLOW_WARPED_MOTION: u32 = 15;

/// `loop_filter_info_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_SHARPNESS_LEVEL: u32 = 0;
/// Mode-reference-delta-enabled bit shift.
pub const AV1_MODE_REF_DELTA_ENABLED: u32 = 3;
/// Mode-reference-delta-update bit shift.
pub const AV1_MODE_REF_DELTA_UPDATE: u32 = 4;

/// `qmatrix_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_USING_QMATRIX: u32 = 0;
/// Luma-quantizer-matrix bit shift.
pub const AV1_QM_Y: u32 = 1;
/// Cb-quantizer-matrix bit shift.
pub const AV1_QM_U: u32 = 5;
/// Cr-quantizer-matrix bit shift.
pub const AV1_QM_V: u32 = 9;

/// `mode_control_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_DELTA_Q_PRESENT: u32 = 0;
/// Delta-Q-resolution bit shift.
pub const AV1_LOG2_DELTA_Q_RES: u32 = 1;
/// Delta-loop-filter-present bit shift.
pub const AV1_DELTA_LF_PRESENT: u32 = 3;
/// Delta-loop-filter-resolution bit shift.
pub const AV1_LOG2_DELTA_LF_RES: u32 = 4;
/// Multi-delta-loop-filter bit shift.
pub const AV1_DELTA_LF_MULTI: u32 = 6;
/// Transform-mode bit shift.
pub const AV1_TX_MODE: u32 = 7;
/// Reference-select bit shift.
pub const AV1_REFERENCE_SELECT: u32 = 9;
/// Reduced-transform-set bit shift.
pub const AV1_REDUCED_TX_SET_USED: u32 = 10;
/// Skip-mode-present bit shift.
pub const AV1_SKIP_MODE_PRESENT: u32 = 11;

/// `loop_restoration_fields` bit shifts inside `VADecPictureParameterBufferAV1`.
pub const AV1_YFRAME_RESTORATION_TYPE: u32 = 0;
/// Cb-restoration-type bit shift.
pub const AV1_CBFRAME_RESTORATION_TYPE: u32 = 2;
/// Cr-restoration-type bit shift.
pub const AV1_CRFRAME_RESTORATION_TYPE: u32 = 4;
/// Restoration-unit-shift bit shift.
pub const AV1_LR_UNIT_SHIFT: u32 = 6;
/// Restoration-chroma-shift bit shift.
pub const AV1_LR_UV_SHIFT: u32 = 8;

/// `segment_info_fields` bit shifts inside `VASegmentationStructAV1`.
pub const AV1_SEG_ENABLED: u32 = 0;
/// Segmentation-map-update bit shift.
pub const AV1_SEG_UPDATE_MAP: u32 = 1;
/// Segmentation-temporal-update bit shift.
pub const AV1_SEG_TEMPORAL_UPDATE: u32 = 2;
/// Segmentation-data-update bit shift.
pub const AV1_SEG_UPDATE_DATA: u32 = 3;

/// `film_grain_info_fields` bit shifts inside `VAFilmGrainStructAV1`.
pub const AV1_APPLY_GRAIN: u32 = 0;
/// Chroma-scaling-from-luma bit shift.
pub const AV1_CHROMA_SCALING_FROM_LUMA: u32 = 1;
/// Grain-scaling-minus-8 bit shift.
pub const AV1_GRAIN_SCALING_MINUS_8: u32 = 2;
/// Autoregressive-lag bit shift.
pub const AV1_AR_COEFF_LAG: u32 = 4;
/// Autoregressive-shift bit shift.
pub const AV1_AR_COEFF_SHIFT_MINUS_6: u32 = 6;
/// Grain-scale-shift bit shift.
pub const AV1_GRAIN_SCALE_SHIFT: u32 = 8;
/// Overlap bit shift.
pub const AV1_OVERLAP_FLAG: u32 = 10;
/// Restricted-range-clip bit shift.
pub const AV1_CLIP_TO_RESTRICTED_RANGE: u32 = 11;

/// AV1 identity warp transform.
pub const AV1_WARP_IDENTITY: c_int = 0;
/// AV1 translational warp transform.
pub const AV1_WARP_TRANSLATION: c_int = 1;
/// AV1 rotation-zoom warp transform.
pub const AV1_WARP_ROTZOOM: c_int = 2;
/// AV1 affine warp transform.
pub const AV1_WARP_AFFINE: c_int = 3;

/// H.264 reference picture descriptor.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaPictureH264 {
    /// Decoded surface handle.
    pub picture_id: VaSurfaceId,
    /// Frame number, or long-term index for long-term refs.
    pub frame_idx: u32,
    /// `VA_PICTURE_H264_*` flags.
    pub flags: u32,
    /// Top-field order count.
    pub top_field_order_cnt: i32,
    /// Bottom-field order count.
    pub bottom_field_order_cnt: i32,
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// H.264 picture-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaPictureParameterBufferH264 {
    /// Picture under decode.
    pub curr_pic: VaPictureH264,
    /// Decoded-picture-buffer contents.
    pub reference_frames: [VaPictureH264; 16],
    /// Coded width in macroblocks, minus one.
    pub picture_width_in_mbs_minus1: u16,
    /// Coded height in macroblocks, minus one.
    pub picture_height_in_mbs_minus1: u16,
    /// Luma bit depth, minus eight.
    pub bit_depth_luma_minus8: u8,
    /// Chroma bit depth, minus eight.
    pub bit_depth_chroma_minus8: u8,
    /// Maximum reference frames.
    pub num_ref_frames: u8,
    /// Packed sequence flags; see `SEQ_*`.
    pub seq_fields: u32,
    /// Slice-group count; FMO unsupported, zero.
    pub num_slice_groups_minus1: u8,
    /// Slice-group map; FMO unsupported, zero.
    pub slice_group_map_type: u8,
    /// Slice-group change rate; FMO unsupported, zero.
    pub slice_group_change_rate_minus1: u16,
    /// Initial luma quantization, minus 26.
    pub pic_init_qp_minus26: i8,
    /// Initial SP/SI quantization, minus 26.
    pub pic_init_qs_minus26: i8,
    /// Cb quantization offset.
    pub chroma_qp_index_offset: i8,
    /// Cr quantization offset.
    pub second_chroma_qp_index_offset: i8,
    /// Packed picture flags; see `PIC_*`.
    pub pic_fields: u32,
    /// Current frame number.
    pub frame_num: u16,
    /// Reserved, zero.
    pub reserved: [u32; 8],
}

/// H.264 inverse-quantization-matrix buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaIqMatrixBufferH264 {
    /// 4x4 scaling lists in raster order.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists in raster order.
    pub scaling_list_8x8: [[u8; 64]; 2],
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// H.264 slice-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaSliceParameterBufferH264 {
    /// Slice-data buffer bytes for this slice.
    pub slice_data_size: u32,
    /// Byte offset of the NAL header in the slice-data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*` flags.
    pub slice_data_flag: u32,
    /// Bit offset of `slice_data()` past the NAL header byte.
    pub slice_data_bit_offset: u16,
    /// First macroblock address in the slice.
    pub first_mb_in_slice: u16,
    /// Slice type.
    pub slice_type: u8,
    /// Direct-spatial-motion-prediction flag.
    pub direct_spatial_mv_pred_flag: u8,
    /// Active list-0 references, minus one.
    pub num_ref_idx_l0_active_minus1: u8,
    /// Active list-1 references, minus one.
    pub num_ref_idx_l1_active_minus1: u8,
    /// CABAC init table index.
    pub cabac_init_idc: u8,
    /// Slice quantization delta.
    pub slice_qp_delta: i8,
    /// Deblocking-filter disable index.
    pub disable_deblocking_filter_idc: u8,
    /// Deblocking alpha offset, divided by two.
    pub slice_alpha_c0_offset_div2: i8,
    /// Deblocking beta offset, divided by two.
    pub slice_beta_offset_div2: i8,
    /// List-0 reference pictures.
    pub ref_pic_list0: [VaPictureH264; 32],
    /// List-1 reference pictures.
    pub ref_pic_list1: [VaPictureH264; 32],
    /// Luma weight denominator.
    pub luma_log2_weight_denom: u8,
    /// Chroma weight denominator.
    pub chroma_log2_weight_denom: u8,
    /// List-0 luma weights present.
    pub luma_weight_l0_flag: u8,
    /// List-0 luma weights.
    pub luma_weight_l0: [i16; 32],
    /// List-0 luma offsets.
    pub luma_offset_l0: [i16; 32],
    /// List-0 chroma weights present.
    pub chroma_weight_l0_flag: u8,
    /// List-0 chroma weights.
    pub chroma_weight_l0: [[i16; 2]; 32],
    /// List-0 chroma offsets.
    pub chroma_offset_l0: [[i16; 2]; 32],
    /// List-1 luma weights present.
    pub luma_weight_l1_flag: u8,
    /// List-1 luma weights.
    pub luma_weight_l1: [i16; 32],
    /// List-1 luma offsets.
    pub luma_offset_l1: [i16; 32],
    /// List-1 chroma weights present.
    pub chroma_weight_l1_flag: u8,
    /// List-1 chroma weights.
    pub chroma_weight_l1: [[i16; 2]; 32],
    /// List-1 chroma offsets.
    pub chroma_offset_l1: [[i16; 2]; 32],
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// VA-API image format descriptor.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaImageFormat {
    /// Image fourcc.
    pub fourcc: u32,
    /// Byte order.
    pub byte_order: u32,
    /// Bits per pixel.
    pub bits_per_pixel: u32,
    /// Significant bits per pixel.
    pub depth: u32,
    /// Red mask.
    pub red_mask: u32,
    /// Green mask.
    pub green_mask: u32,
    /// Blue mask.
    pub blue_mask: u32,
    /// Alpha mask.
    pub alpha_mask: u32,
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// VA-API image descriptor.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaImage {
    /// Image handle.
    pub image_id: VaImageId,
    /// Image format.
    pub format: VaImageFormat,
    /// Image data buffer.
    pub buf: VaBufferId,
    /// Image width in samples.
    pub width: c_ushort,
    /// Image height in samples.
    pub height: c_ushort,
    /// Image data bytes.
    pub data_size: u32,
    /// Plane count, at most three.
    pub num_planes: u32,
    /// Row pitch per plane.
    pub pitches: [u32; 3],
    /// Byte offset per plane.
    pub offsets: [u32; 3],
    /// Palette entries; zero without a palette.
    pub num_palette_entries: i32,
    /// Palette entry bytes; zero without a palette.
    pub entry_bytes: i32,
    /// Palette component order.
    pub component_order: [i8; 4],
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// VA-API config attribute.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaConfigAttrib {
    /// Attribute type.
    pub attr_type: c_int,
    /// Attribute value flags.
    pub value: u32,
}

/// HEVC reference picture descriptor.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaPictureHevc {
    /// Reconstructed surface handle.
    pub picture_id: VaSurfaceId,
    /// Picture order count.
    pub pic_order_cnt: i32,
    /// `VA_PICTURE_HEVC_*` flags.
    pub flags: u32,
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// HEVC decode picture-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaPictureParameterBufferHevc {
    /// Picture under decode.
    pub curr_pic: VaPictureHevc,
    /// Decoded-picture-buffer contents.
    pub reference_frames: [VaPictureHevc; 15],
    /// Coded width in luma samples.
    pub pic_width_in_luma_samples: u16,
    /// Coded height in luma samples.
    pub pic_height_in_luma_samples: u16,
    /// Packed picture flags; see `HEVC_*`.
    pub pic_fields: u32,
    /// DPB size of the current temporal layer, minus one.
    pub sps_max_dec_pic_buffering_minus1: u8,
    /// Luma bit depth, minus eight.
    pub bit_depth_luma_minus8: u8,
    /// Chroma bit depth, minus eight.
    pub bit_depth_chroma_minus8: u8,
    /// PCM luma sample depth, minus one.
    pub pcm_sample_bit_depth_luma_minus1: u8,
    /// PCM chroma sample depth, minus one.
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    /// Minimum luma coding-block size, minus three.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// Coding-block size range.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// Minimum transform-block size, minus two.
    pub log2_min_transform_block_size_minus2: u8,
    /// Transform-block size range.
    pub log2_diff_max_min_transform_block_size: u8,
    /// Minimum PCM coding-block size, minus three.
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    /// PCM coding-block size range.
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    /// Maximum intra transform depth.
    pub max_transform_hierarchy_depth_intra: u8,
    /// Maximum inter transform depth.
    pub max_transform_hierarchy_depth_inter: u8,
    /// Initial quantization, minus 26.
    pub init_qp_minus26: i8,
    /// CU QP delta depth.
    pub diff_cu_qp_delta_depth: u8,
    /// Cb quantization offset.
    pub pps_cb_qp_offset: i8,
    /// Cr quantization offset.
    pub pps_cr_qp_offset: i8,
    /// Parallel-merge level, minus two.
    pub log2_parallel_merge_level_minus2: u8,
    /// Tile columns, minus one.
    pub num_tile_columns_minus1: u8,
    /// Tile rows, minus one.
    pub num_tile_rows_minus1: u8,
    /// Tile column widths, minus one.
    pub column_width_minus1: [u16; 19],
    /// Tile row heights, minus one.
    pub row_height_minus1: [u16; 21],
    /// Packed slice-parsing flags; see `HEVC_*`.
    pub slice_parsing_fields: u32,
    /// Picture-order-count lsb range, minus four.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// Short-term reference-picture-set count.
    pub num_short_term_ref_pic_sets: u8,
    /// Long-term reference pictures in the SPS.
    pub num_long_term_ref_pic_sps: u8,
    /// Default active list-0 references, minus one.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// Default active list-1 references, minus one.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// PPS beta offset, divided by two.
    pub pps_beta_offset_div2: i8,
    /// PPS Tc offset, divided by two.
    pub pps_tc_offset_div2: i8,
    /// Extra slice-header bits.
    pub num_extra_slice_header_bits: u8,
    /// Inline short-term reference-set bits, without emulation prevention.
    pub st_rps_bits: u32,
    /// Reserved, zero.
    pub reserved: [u32; 8],
}

/// HEVC decode slice-parameter buffer, long format.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaSliceParameterBufferHevc {
    /// Slice-data buffer bytes for this slice, NAL header included.
    pub slice_data_size: u32,
    /// Byte offset of the NAL header in the slice-data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*` flags.
    pub slice_data_flag: u32,
    /// Byte offset of `slice_data()` past the NAL header.
    pub slice_data_byte_offset: u32,
    /// First coding-tree-block address in the slice.
    pub slice_segment_address: u32,
    /// Reference lists as `ReferenceFrames` indices, `0xFF` invalid.
    pub ref_pic_list: [[u8; 15]; 2],
    /// Packed slice flags; see `HEVC_*`.
    pub long_slice_flags: u32,
    /// Collocated reference index, `0xFF` without temporal MVP.
    pub collocated_ref_idx: u8,
    /// Active list-0 references, minus one.
    pub num_ref_idx_l0_active_minus1: u8,
    /// Active list-1 references, minus one.
    pub num_ref_idx_l1_active_minus1: u8,
    /// Slice quantization delta.
    pub slice_qp_delta: i8,
    /// Slice Cb quantization offset.
    pub slice_cb_qp_offset: i8,
    /// Slice Cr quantization offset.
    pub slice_cr_qp_offset: i8,
    /// Slice beta offset, divided by two.
    pub slice_beta_offset_div2: i8,
    /// Slice Tc offset, divided by two.
    pub slice_tc_offset_div2: i8,
    /// Luma weight denominator.
    pub luma_log2_weight_denom: u8,
    /// Chroma weight denominator delta.
    pub delta_chroma_log2_weight_denom: i8,
    /// List-0 luma weight deltas.
    pub delta_luma_weight_l0: [i8; 15],
    /// List-0 luma offsets.
    pub luma_offset_l0: [i8; 15],
    /// List-0 chroma weight deltas.
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    /// List-0 chroma offsets.
    pub chroma_offset_l0: [[i8; 2]; 15],
    /// List-1 luma weight deltas.
    pub delta_luma_weight_l1: [i8; 15],
    /// List-1 luma offsets.
    pub luma_offset_l1: [i8; 15],
    /// List-1 chroma weight deltas.
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    /// List-1 chroma offsets.
    pub chroma_offset_l1: [[i8; 2]; 15],
    /// Merge candidates below five.
    pub five_minus_max_num_merge_cand: u8,
    /// Entry-point offset count.
    pub num_entry_point_offsets: u16,
    /// Entry-point subset index.
    pub entry_offset_to_subset_array: u16,
    /// Emulation-prevention bytes in the slice header.
    pub slice_data_num_emu_prevn_bytes: u16,
    /// Reserved, zero.
    pub reserved: [u32; 2],
}

/// HEVC decode inverse-quantization-matrix buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaIqMatrixBufferHevc {
    /// 4x4 scaling lists in raster order.
    pub scaling_list_4x4: [[u8; 16]; 6],
    /// 8x8 scaling lists in raster order.
    pub scaling_list_8x8: [[u8; 64]; 6],
    /// 16x16 scaling lists in raster order.
    pub scaling_list_16x16: [[u8; 64]; 6],
    /// 32x32 scaling lists in raster order.
    pub scaling_list_32x32: [[u8; 64]; 2],
    /// 16x16 scaling-list DC coefficients.
    pub scaling_list_dc_16x16: [u8; 6],
    /// 32x32 scaling-list DC coefficients.
    pub scaling_list_dc_32x32: [u8; 2],
    /// Reserved, zero.
    pub reserved: [u32; 4],
}

/// AV1 segmentation parameters.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaSegmentationAv1 {
    /// Packed segmentation flags; see `AV1_SEG_*`.
    pub segment_info_fields: u32,
    /// Segment feature data, clipped per feature limits.
    pub feature_data: [[i16; 8]; 8],
    /// Enabled features per segment, one bit per feature.
    pub feature_mask: [u8; 8],
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// AV1 film-grain parameters.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaFilmGrainAv1 {
    /// Packed film-grain flags; see `AV1_*`.
    pub film_grain_info_fields: u32,
    /// Synthesis random seed.
    pub grain_seed: u16,
    /// Luma scaling points.
    pub num_y_points: u8,
    /// Luma scaling control values.
    pub point_y_value: [u8; 14],
    /// Luma scaling output values.
    pub point_y_scaling: [u8; 14],
    /// Cb scaling points.
    pub num_cb_points: u8,
    /// Cb scaling control values.
    pub point_cb_value: [u8; 10],
    /// Cb scaling output values.
    pub point_cb_scaling: [u8; 10],
    /// Cr scaling points.
    pub num_cr_points: u8,
    /// Cr scaling control values.
    pub point_cr_value: [u8; 10],
    /// Cr scaling output values.
    pub point_cr_scaling: [u8; 10],
    /// Luma autoregressive coefficients.
    pub ar_coeffs_y: [i8; 24],
    /// Cb autoregressive coefficients.
    pub ar_coeffs_cb: [i8; 25],
    /// Cr autoregressive coefficients.
    pub ar_coeffs_cr: [i8; 25],
    /// Cb component multiplier.
    pub cb_mult: u8,
    /// Cb luma multiplier.
    pub cb_luma_mult: u8,
    /// Cb index offset.
    pub cb_offset: u16,
    /// Cr component multiplier.
    pub cr_mult: u8,
    /// Cr luma multiplier.
    pub cr_luma_mult: u8,
    /// Cr index offset.
    pub cr_offset: u16,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// AV1 warped-motion parameters for one reference.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaWarpedMotionAv1 {
    /// Warp type; see `AV1_WARP_*`.
    pub wmtype: c_int,
    /// Warp matrix, `gm_params` rows packed.
    pub wmmat: [i32; 8],
    /// Nonzero when the affine set is invalid.
    pub invalid: u8,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// AV1 decode picture-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaDecPictureParameterBufferAv1 {
    /// Bitstream profile.
    pub profile: u8,
    /// Order-hint bits, minus one.
    pub order_hint_bits_minus_1: u8,
    /// Bit-depth index: 0 for 8-bit, 1 for 10-bit, 2 for 12-bit.
    pub bit_depth_idx: u8,
    /// Matrix coefficients.
    pub matrix_coefficients: u8,
    /// Packed sequence flags; see `AV1_*`.
    pub seq_info_fields: u32,
    /// Surface under decode, grain-free with film grain.
    pub current_frame: VaSurfaceId,
    /// Grain-applied display surface, with film grain only.
    pub current_display_picture: VaSurfaceId,
    /// Large-scale-tile anchor count; zero without large tiles.
    pub anchor_frames_num: u8,
    /// Large-scale-tile anchors; null without large tiles.
    pub anchor_frames_list: *const VaSurfaceId,
    /// Frame width, minus one.
    pub frame_width_minus1: u16,
    /// Frame height, minus one.
    pub frame_height_minus1: u16,
    /// Output width in tiles, minus one; large tiles only.
    pub output_frame_width_in_tiles_minus_1: u16,
    /// Output height in tiles, minus one; large tiles only.
    pub output_frame_height_in_tiles_minus_1: u16,
    /// Reference surfaces by slot.
    pub ref_frame_map: [VaSurfaceId; 8],
    /// Reference slots by `LAST` to `ALTREF` order.
    pub ref_frame_idx: [u8; 7],
    /// Primary reference index, 7 for none.
    pub primary_ref_frame: u8,
    /// Frame order hint.
    pub order_hint: u8,
    /// Segmentation parameters.
    pub seg_info: VaSegmentationAv1,
    /// Film-grain parameters.
    pub film_grain_info: VaFilmGrainAv1,
    /// Tile columns.
    pub tile_cols: u8,
    /// Tile rows.
    pub tile_rows: u8,
    /// Tile widths in superblocks, minus one.
    pub width_in_sbs_minus_1: [u16; 63],
    /// Tile heights in superblocks, minus one.
    pub height_in_sbs_minus_1: [u16; 63],
    /// Large-scale-tile count, minus one; large tiles only.
    pub tile_count_minus_1: u16,
    /// Context-update tile id.
    pub context_update_tile_id: u16,
    /// Packed picture flags; see `AV1_*`.
    pub pic_info_fields: u32,
    /// Superres denominator: 8 without superres.
    pub superres_scale_denominator: u8,
    /// Interpolation filter.
    pub interp_filter: u8,
    /// Luma loop-filter levels.
    pub filter_level: [u8; 2],
    /// Cb loop-filter level.
    pub filter_level_u: u8,
    /// Cr loop-filter level.
    pub filter_level_v: u8,
    /// Packed loop-filter flags; see `AV1_*`.
    pub loop_filter_info_fields: u8,
    /// Reference loop-filter deltas.
    pub ref_deltas: [i8; 8],
    /// Mode loop-filter deltas.
    pub mode_deltas: [i8; 2],
    /// Base quantization index.
    pub base_qindex: u8,
    /// Luma-DC quantization delta.
    pub y_dc_delta_q: i8,
    /// Cb-DC quantization delta.
    pub u_dc_delta_q: i8,
    /// Cb-AC quantization delta.
    pub u_ac_delta_q: i8,
    /// Cr-DC quantization delta.
    pub v_dc_delta_q: i8,
    /// Cr-AC quantization delta.
    pub v_ac_delta_q: i8,
    /// Packed quantizer-matrix flags; see `AV1_*`.
    pub qmatrix_fields: u16,
    /// Packed mode-control flags; see `AV1_*`.
    pub mode_control_fields: u32,
    /// CDEF damping, minus three.
    pub cdef_damping_minus_3: u8,
    /// CDEF strength bits.
    pub cdef_bits: u8,
    /// Packed luma CDEF strengths.
    pub cdef_y_strengths: [u8; 8],
    /// Packed chroma CDEF strengths.
    pub cdef_uv_strengths: [u8; 8],
    /// Packed restoration flags; see `AV1_*`.
    pub loop_restoration_fields: u16,
    /// Warped-motion parameters by `LAST` to `ALTREF` order.
    pub wm: [VaWarpedMotionAv1; 7],
    /// Reserved, zero.
    pub va_reserved: [u32; 8],
}

/// AV1 slice-parameter buffer, one per tile.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaSliceParameterBufferAv1 {
    /// Tile bytes in the slice-data buffer.
    pub slice_data_size: u32,
    /// Byte offset of the tile in the slice-data buffer.
    pub slice_data_offset: u32,
    /// `VA_SLICE_DATA_FLAG_*` flags.
    pub slice_data_flag: u32,
    /// Tile row.
    pub tile_row: u16,
    /// Tile column.
    pub tile_column: u16,
    /// First tile-group tile; deprecated, zero.
    pub tg_start: u16,
    /// Last tile-group tile; deprecated, zero.
    pub tg_end: u16,
    /// Large-scale-tile anchor index; zero without large tiles.
    pub anchor_frame_idx: u8,
    /// Large-scale-tile list index; zero without large tiles.
    pub tile_idx_in_tile_list: u16,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// H.264 encode sequence-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaEncSequenceParameterBufferH264 {
    /// Sequence set id.
    pub seq_parameter_set_id: u8,
    /// Level idc.
    pub level_idc: u8,
    /// Period between I frames.
    pub intra_period: u32,
    /// Period between IDR frames.
    pub intra_idr_period: u32,
    /// Period between I/P frames.
    pub ip_period: u32,
    /// Initial bitrate, CBR/VBR only.
    pub bits_per_second: u32,
    /// Maximum reference frames.
    pub max_num_ref_frames: u32,
    /// Picture width in macroblocks.
    pub picture_width_in_mbs: u16,
    /// Picture height in macroblocks.
    pub picture_height_in_mbs: u16,
    /// Packed sequence flags.
    pub seq_fields: u32,
    /// Luma bit depth, minus eight.
    pub bit_depth_luma_minus8: u8,
    /// Chroma bit depth, minus eight.
    pub bit_depth_chroma_minus8: u8,
    /// Reference frames in the order-count cycle.
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    /// Non-reference order-count offset.
    pub offset_for_non_ref_pic: i32,
    /// Top-to-bottom order-count offset.
    pub offset_for_top_to_bottom_field: i32,
    /// Per-frame order-count offsets.
    pub offset_for_ref_frame: [i32; 256],
    /// Frame cropping flag.
    pub frame_cropping_flag: u8,
    /// Left crop offset.
    pub frame_crop_left_offset: u32,
    /// Right crop offset.
    pub frame_crop_right_offset: u32,
    /// Top crop offset.
    pub frame_crop_top_offset: u32,
    /// Bottom crop offset.
    pub frame_crop_bottom_offset: u32,
    /// VUI present flag.
    pub vui_parameters_present_flag: u8,
    /// Packed VUI flags.
    pub vui_fields: u32,
    /// Aspect-ratio idc.
    pub aspect_ratio_idc: u8,
    /// Sample aspect width.
    pub sar_width: u32,
    /// Sample aspect height.
    pub sar_height: u32,
    /// Ticks per frame numerator.
    pub num_units_in_tick: u32,
    /// Ticks per second.
    pub time_scale: u32,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// H.264 encode picture-parameter buffer.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaEncPictureParameterBufferH264 {
    /// Picture under encode, plus the scratch reconstruction surface.
    pub curr_pic: VaPictureH264,
    /// Decoded-picture-buffer contents.
    pub reference_frames: [VaPictureH264; 16],
    /// Coded-bitstream buffer.
    pub coded_buf: VaBufferId,
    /// Picture set id.
    pub pic_parameter_set_id: u8,
    /// Sequence set id.
    pub seq_parameter_set_id: u8,
    /// Last-picture flags.
    pub last_picture: u8,
    /// Picture frame number.
    pub frame_num: u16,
    /// Initial quantization.
    pub pic_init_qp: u8,
    /// Active list-0 references, minus one.
    pub num_ref_idx_l0_active_minus1: u8,
    /// Active list-1 references, minus one.
    pub num_ref_idx_l1_active_minus1: u8,
    /// Cb quantization offset.
    pub chroma_qp_index_offset: i8,
    /// Cr quantization offset.
    pub second_chroma_qp_index_offset: i8,
    /// Packed picture flags.
    pub pic_fields: u32,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// H.264 encode slice-parameter buffer.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaEncSliceParameterBufferH264 {
    /// Starting macroblock address.
    pub macroblock_address: u32,
    /// Macroblock count in the slice.
    pub num_macroblocks: u32,
    /// Per-macroblock buffer, or invalid.
    pub macroblock_info: VaBufferId,
    /// Slice type.
    pub slice_type: u8,
    /// Picture set id.
    pub pic_parameter_set_id: u8,
    /// IDR id.
    pub idr_pic_id: u16,
    /// Order count modulo the lsb range.
    pub pic_order_cnt_lsb: u16,
    /// Bottom-field order delta.
    pub delta_pic_order_cnt_bottom: i32,
    /// Order-count deltas, top then bottom.
    pub delta_pic_order_cnt: [i32; 2],
    /// Direct-spatial-motion-prediction flag.
    pub direct_spatial_mv_pred_flag: u8,
    /// Reference-override flag.
    pub num_ref_idx_active_override_flag: u8,
    /// Active list-0 references, minus one.
    pub num_ref_idx_l0_active_minus1: u8,
    /// Active list-1 references, minus one.
    pub num_ref_idx_l1_active_minus1: u8,
    /// List-0 reference pictures.
    pub ref_pic_list0: [VaPictureH264; 32],
    /// List-1 reference pictures.
    pub ref_pic_list1: [VaPictureH264; 32],
    /// Luma weight denominator.
    pub luma_log2_weight_denom: u8,
    /// Chroma weight denominator.
    pub chroma_log2_weight_denom: u8,
    /// List-0 luma weights present.
    pub luma_weight_l0_flag: u8,
    /// List-0 luma weights.
    pub luma_weight_l0: [i16; 32],
    /// List-0 luma offsets.
    pub luma_offset_l0: [i16; 32],
    /// List-0 chroma weights present.
    pub chroma_weight_l0_flag: u8,
    /// List-0 chroma weights.
    pub chroma_weight_l0: [[i16; 2]; 32],
    /// List-0 chroma offsets.
    pub chroma_offset_l0: [[i16; 2]; 32],
    /// List-1 luma weights present.
    pub luma_weight_l1_flag: u8,
    /// List-1 luma weights.
    pub luma_weight_l1: [i16; 32],
    /// List-1 luma offsets.
    pub luma_offset_l1: [i16; 32],
    /// List-1 chroma weights present.
    pub chroma_weight_l1_flag: u8,
    /// List-1 chroma weights.
    pub chroma_weight_l1: [[i16; 2]; 32],
    /// List-1 chroma offsets.
    pub chroma_offset_l1: [[i16; 2]; 32],
    /// CABAC init table index.
    pub cabac_init_idc: u8,
    /// Slice quantization delta.
    pub slice_qp_delta: i8,
    /// Deblocking-filter disable index.
    pub disable_deblocking_filter_idc: u8,
    /// Deblocking alpha offset, divided by two.
    pub slice_alpha_c0_offset_div2: i8,
    /// Deblocking beta offset, divided by two.
    pub slice_beta_offset_div2: i8,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// HEVC encode sequence-parameter buffer.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaEncSequenceParameterBufferHevc {
    /// General profile idc.
    pub general_profile_idc: u8,
    /// General level idc, thirty times the level.
    pub general_level_idc: u8,
    /// General tier flag.
    pub general_tier_flag: u8,
    /// Period between I frames.
    pub intra_period: u32,
    /// Period between IDR frames.
    pub intra_idr_period: u32,
    /// Period between I/P frames.
    pub ip_period: u32,
    /// Initial bitrate, CBR/VBR only.
    pub bits_per_second: u32,
    /// Picture width in luma samples.
    pub pic_width_in_luma_samples: u16,
    /// Picture height in luma samples.
    pub pic_height_in_luma_samples: u16,
    /// Packed sequence flags.
    pub seq_fields: u32,
    /// Minimum luma block size, minus three.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// Block-size range.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// Minimum transform size, minus two.
    pub log2_min_transform_block_size_minus2: u8,
    /// Transform-size range.
    pub log2_diff_max_min_transform_block_size: u8,
    /// Maximum inter hierarchy depth.
    pub max_transform_hierarchy_depth_inter: u8,
    /// Maximum intra hierarchy depth.
    pub max_transform_hierarchy_depth_intra: u8,
    /// PCM luma depth, minus one.
    pub pcm_sample_bit_depth_luma_minus1: u32,
    /// PCM chroma depth, minus one.
    pub pcm_sample_bit_depth_chroma_minus1: u32,
    /// Minimum PCM block size, minus three.
    pub log2_min_pcm_luma_coding_block_size_minus3: u32,
    /// Maximum PCM block size, minus three.
    pub log2_max_pcm_luma_coding_block_size_minus3: u32,
    /// VUI present flag.
    pub vui_parameters_present_flag: u8,
    /// Packed VUI flags.
    pub vui_fields: u32,
    /// Aspect-ratio idc.
    pub aspect_ratio_idc: u8,
    /// Sample aspect width.
    pub sar_width: u32,
    /// Sample aspect height.
    pub sar_height: u32,
    /// Ticks per frame numerator.
    pub vui_num_units_in_tick: u32,
    /// Ticks per second.
    pub vui_time_scale: u32,
    /// Minimum spatial segmentation idc.
    pub min_spatial_segmentation_idc: u16,
    /// Maximum bytes per picture denominator.
    pub max_bytes_per_pic_denom: u8,
    /// Maximum bits per block denominator.
    pub max_bits_per_min_cu_denom: u8,
    /// Packed screen-content flags.
    pub scc_fields: u32,
    /// Reserved, zero.
    pub va_reserved: [u32; 7],
}

/// HEVC encode picture-parameter buffer.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaEncPictureParameterBufferHevc {
    /// Picture under encode, plus the scratch reconstruction surface.
    pub decoded_curr_pic: VaPictureHevc,
    /// Decoded-picture-buffer contents.
    pub reference_frames: [VaPictureHevc; 15],
    /// Coded-bitstream buffer.
    pub coded_buf: VaBufferId,
    /// Collocated reference index, or `0xFF` without temporal MVP.
    pub collocated_ref_pic_index: u8,
    /// Last-picture flags.
    pub last_picture: u8,
    /// Initial quantization.
    pub pic_init_qp: u8,
    /// CU QP delta depth.
    pub diff_cu_qp_delta_depth: u8,
    /// PPS Cb quantization offset.
    pub pps_cb_qp_offset: i8,
    /// PPS Cr quantization offset.
    pub pps_cr_qp_offset: i8,
    /// Tile columns, minus one.
    pub num_tile_columns_minus1: u8,
    /// Tile rows, minus one.
    pub num_tile_rows_minus1: u8,
    /// Tile column widths, minus one.
    pub column_width_minus1: [u8; 19],
    /// Tile row heights, minus one.
    pub row_height_minus1: [u8; 21],
    /// Parallel merge level, minus two.
    pub log2_parallel_merge_level_minus2: u8,
    /// CTU bit-size limit, zero for none.
    pub ctu_max_bitsize_allowed: u8,
    /// Default active list-0 references, minus one.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// Default active list-1 references, minus one.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// Picture set id for generated slice headers.
    pub slice_pic_parameter_set_id: u8,
    /// NAL unit type for generated slice headers.
    pub nal_unit_type: u8,
    /// Packed picture flags.
    pub pic_fields: u32,
    /// Hierarchical level, plus one.
    pub hierarchical_level_plus1: u8,
    /// Reserved, zero.
    pub va_byte_reserved: u8,
    /// Packed screen-content flags.
    pub scc_fields: u16,
    /// Reserved, zero.
    pub va_reserved: [u32; 15],
}

/// HEVC encode slice-parameter buffer.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaEncSliceParameterBufferHevc {
    /// Starting CTU address.
    pub slice_segment_address: u32,
    /// CTU count in the slice.
    pub num_ctu_in_slice: u32,
    /// Slice type.
    pub slice_type: u8,
    /// Picture set id.
    pub slice_pic_parameter_set_id: u8,
    /// Active list-0 references, minus one.
    pub num_ref_idx_l0_active_minus1: u8,
    /// Active list-1 references, minus one.
    pub num_ref_idx_l1_active_minus1: u8,
    /// List-0 reference pictures.
    pub ref_pic_list0: [VaPictureHevc; 15],
    /// List-1 reference pictures.
    pub ref_pic_list1: [VaPictureHevc; 15],
    /// Luma weight denominator.
    pub luma_log2_weight_denom: u8,
    /// Chroma weight denominator delta.
    pub delta_chroma_log2_weight_denom: i8,
    /// List-0 luma weight deltas.
    pub delta_luma_weight_l0: [i8; 15],
    /// List-0 luma offsets.
    pub luma_offset_l0: [i8; 15],
    /// List-0 chroma weight deltas.
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    /// List-0 chroma offsets.
    pub chroma_offset_l0: [[i8; 2]; 15],
    /// List-1 luma weight deltas.
    pub delta_luma_weight_l1: [i8; 15],
    /// List-1 luma offsets.
    pub luma_offset_l1: [i8; 15],
    /// List-1 chroma weight deltas.
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    /// List-1 chroma offsets.
    pub chroma_offset_l1: [[i8; 2]; 15],
    /// Maximum merge candidates.
    pub max_num_merge_cand: u8,
    /// Slice quantization delta.
    pub slice_qp_delta: i8,
    /// Slice Cb quantization offset.
    pub slice_cb_qp_offset: i8,
    /// Slice Cr quantization offset.
    pub slice_cr_qp_offset: i8,
    /// Slice beta offset, divided by two.
    pub slice_beta_offset_div2: i8,
    /// Slice Tc offset, divided by two.
    pub slice_tc_offset_div2: i8,
    /// Packed slice flags.
    pub slice_fields: u32,
    /// Weight-table bit offset in the packed header.
    pub pred_weight_table_bit_offset: u32,
    /// Weight-table bit length in the packed header.
    pub pred_weight_table_bit_length: u32,
    /// Reserved, zero.
    pub va_reserved: [u32; 6],
}

/// Encode packed-header parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaEncPackedHeaderParameterBuffer {
    /// Packed header type.
    pub header_type: u32,
    /// Data-buffer size in bits.
    pub bit_length: u32,
    /// Data holds emulation-prevention bytes.
    pub has_emulation_bytes: u8,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// Coded-buffer segment: one link of the encode output list.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaCodedBufferSegment {
    /// Data bytes in this segment.
    pub size: u32,
    /// Bit offset of the video data.
    pub bit_offset: u32,
    /// Driver status flags.
    pub status: u32,
    /// Reserved, zero.
    pub reserved: u32,
    /// Segment data.
    pub buf: *mut c_void,
    /// Next segment, or null.
    pub next: *mut c_void,
    /// Reserved, zero.
    pub va_reserved: [u32; 4],
}

/// DRM PRIME object: one exported file descriptor.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaDrmPrimeObject {
    /// PRIME file descriptor, owned by the caller.
    pub fd: c_int,
    /// Object size in bytes.
    pub size: u32,
    /// Format modifier.
    pub drm_format_modifier: u64,
}

/// DRM PRIME layer: one exported plane set.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaDrmPrimeLayer {
    /// DRM format fourcc.
    pub drm_format: u32,
    /// Plane count in this layer.
    pub num_planes: u32,
    /// Object index per plane.
    pub object_index: [u32; 4],
    /// Byte offset per plane.
    pub offset: [u32; 4],
    /// Row pitch per plane.
    pub pitch: [u32; 4],
}

/// DRM PRIME version-two surface descriptor.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaDrmPrimeDescriptor {
    /// Surface fourcc.
    pub fourcc: u32,
    /// Surface width in samples.
    pub width: u32,
    /// Surface height in samples.
    pub height: u32,
    /// Object count.
    pub num_objects: u32,
    /// Exported objects.
    pub objects: [VaDrmPrimeObject; 4],
    /// Layer count.
    pub num_layers: u32,
    /// Exported layers.
    pub layers: [VaDrmPrimeLayer; 4],
}

/// Video-processing color properties.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct VaProcColorProperties {
    /// Chroma sample location.
    pub chroma_sample_location: u8,
    /// Color range.
    pub color_range: u8,
    /// Colour primaries.
    pub colour_primaries: u8,
    /// Transfer characteristics.
    pub transfer_characteristics: u8,
    /// Matrix coefficients.
    pub matrix_coefficients: u8,
    /// Reserved, zero.
    pub reserved: [u8; 3],
}

/// Video-processing pipeline-parameter buffer.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct VaProcPipelineParameterBuffer {
    /// Source surface.
    pub surface: VaSurfaceId,
    /// Source region, null for the whole surface.
    pub surface_region: *const c_void,
    /// Requested input color standard.
    pub surface_color_standard: c_int,
    /// Output region, null for the whole surface.
    pub output_region: *const c_void,
    /// Background fill color.
    pub output_background_color: u32,
    /// Requested output color standard.
    pub output_color_standard: c_int,
    /// Pipeline flags.
    pub pipeline_flags: u32,
    /// Filter flags.
    pub filter_flags: u32,
    /// Filter buffers.
    pub filters: *mut VaBufferId,
    /// Filter count.
    pub num_filters: u32,
    /// Forward references.
    pub forward_references: *mut VaSurfaceId,
    /// Forward-reference count.
    pub num_forward_references: u32,
    /// Backward references.
    pub backward_references: *mut VaSurfaceId,
    /// Backward-reference count.
    pub num_backward_references: u32,
    /// Rotation state.
    pub rotation_state: u32,
    /// Blend state, null for none.
    pub blend_state: *const c_void,
    /// Mirror state.
    pub mirror_state: u32,
    /// Additional outputs.
    pub additional_outputs: *mut VaSurfaceId,
    /// Additional-output count.
    pub num_additional_outputs: u32,
    /// Input surface flags.
    pub input_surface_flag: u32,
    /// Output surface flags.
    pub output_surface_flag: u32,
    /// Input color properties.
    pub input_color_properties: VaProcColorProperties,
    /// Output color properties.
    pub output_color_properties: VaProcColorProperties,
    /// Processing mode.
    pub processing_mode: c_int,
    /// Output HDR metadata, null for SDR.
    pub output_hdr_metadata: *mut c_void,
    /// Reserved, zero.
    pub va_reserved: [u32; 16],
}

#[link(name = "va")]
#[link(name = "va-drm")]
unsafe extern "C" {
    /// DRM display for `fd`.
    pub fn vaGetDisplayDRM(fd: c_int) -> VaDisplay;
    /// Initialize `dpy`, reporting its version.
    pub fn vaInitialize(dpy: VaDisplay, major: *mut c_int, minor: *mut c_int) -> VaStatus;
    /// Release `dpy`.
    pub fn vaTerminate(dpy: VaDisplay) -> VaStatus;
    /// Driver description string.
    pub fn vaQueryVendorString(dpy: VaDisplay) -> *const c_char;

    /// Profile-list capacity.
    pub fn vaMaxNumProfiles(dpy: VaDisplay) -> c_int;
    /// Entrypoint-list capacity.
    pub fn vaMaxNumEntrypoints(dpy: VaDisplay) -> c_int;
    /// Profiles supported by `dpy`.
    pub fn vaQueryConfigProfiles(
        dpy: VaDisplay,
        profiles: *mut c_int,
        count: *mut c_int,
    ) -> VaStatus;
    /// Entrypoints supported for `profile`.
    pub fn vaQueryConfigEntrypoints(
        dpy: VaDisplay,
        profile: c_int,
        entrypoints: *mut c_int,
        count: *mut c_int,
    ) -> VaStatus;
    /// Attribute values for a profile/entrypoint pair.
    pub fn vaGetConfigAttributes(
        dpy: VaDisplay,
        profile: c_int,
        entrypoint: c_int,
        attribs: *mut VaConfigAttrib,
        count: c_int,
    ) -> VaStatus;
    /// Config for a profile/entrypoint pair.
    pub fn vaCreateConfig(
        dpy: VaDisplay,
        profile: c_int,
        entrypoint: c_int,
        attribs: *mut VaConfigAttrib,
        count: c_int,
        config: *mut VaConfigId,
    ) -> VaStatus;
    /// Release `config`.
    pub fn vaDestroyConfig(dpy: VaDisplay, config: VaConfigId) -> VaStatus;
    /// Render-target surfaces.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateSurfaces(
        dpy: VaDisplay,
        format: c_uint,
        width: c_uint,
        height: c_uint,
        surfaces: *mut VaSurfaceId,
        count: c_uint,
        attribs: *mut c_void,
        attrib_count: c_uint,
    ) -> VaStatus;
    /// Release `count` surfaces.
    pub fn vaDestroySurfaces(dpy: VaDisplay, surfaces: *mut VaSurfaceId, count: c_int) -> VaStatus;
    /// Decode context over `render_targets`.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateContext(
        dpy: VaDisplay,
        config: VaConfigId,
        width: c_int,
        height: c_int,
        flag: c_int,
        render_targets: *mut VaSurfaceId,
        target_count: c_int,
        context: *mut VaContextId,
    ) -> VaStatus;
    /// Release `context`.
    pub fn vaDestroyContext(dpy: VaDisplay, context: VaContextId) -> VaStatus;
    /// Buffer of `elements` items of `size` bytes holding `data`.
    #[allow(clippy::too_many_arguments)]
    pub fn vaCreateBuffer(
        dpy: VaDisplay,
        context: VaContextId,
        buffer_type: c_int,
        size: c_uint,
        elements: c_uint,
        data: *mut c_void,
        buffer: *mut VaBufferId,
    ) -> VaStatus;
    /// Release `buffer`.
    pub fn vaDestroyBuffer(dpy: VaDisplay, buffer: VaBufferId) -> VaStatus;
    /// Start rendering `target`.
    pub fn vaBeginPicture(dpy: VaDisplay, context: VaContextId, target: VaSurfaceId) -> VaStatus;
    /// Submit `count` buffers.
    pub fn vaRenderPicture(
        dpy: VaDisplay,
        context: VaContextId,
        buffers: *mut VaBufferId,
        count: c_int,
    ) -> VaStatus;
    /// Finish rendering.
    pub fn vaEndPicture(dpy: VaDisplay, context: VaContextId) -> VaStatus;
    /// Block until `surface` finishes decoding.
    pub fn vaSyncSurface(dpy: VaDisplay, surface: VaSurfaceId) -> VaStatus;
    /// Image view over `surface`.
    pub fn vaDeriveImage(dpy: VaDisplay, surface: VaSurfaceId, image: *mut VaImage) -> VaStatus;
    /// Release `image`.
    pub fn vaDestroyImage(dpy: VaDisplay, image: VaImageId) -> VaStatus;
    /// Map `buffer` into client memory.
    pub fn vaMapBuffer(dpy: VaDisplay, buffer: VaBufferId, data: *mut *mut c_void) -> VaStatus;
    /// Unmap `buffer`.
    pub fn vaUnmapBuffer(dpy: VaDisplay, buffer: VaBufferId) -> VaStatus;
    /// Static text for `status`.
    pub fn vaStatusStr(status: VaStatus) -> *const c_char;
    /// Export `surface` into `descriptor` for `mem_type`.
    pub fn vaExportSurfaceHandle(
        dpy: VaDisplay,
        surface: VaSurfaceId,
        mem_type: c_uint,
        flags: c_uint,
        descriptor: *mut c_void,
    ) -> VaStatus;
    /// Image-format-list capacity.
    pub fn vaMaxNumImageFormats(dpy: VaDisplay) -> c_int;
    /// Image formats supported by `dpy`.
    pub fn vaQueryImageFormats(
        dpy: VaDisplay,
        formats: *mut VaImageFormat,
        count: *mut c_int,
    ) -> VaStatus;
    /// Image of `format` at `width` x `height`.
    pub fn vaCreateImage(
        dpy: VaDisplay,
        format: *mut VaImageFormat,
        width: c_int,
        height: c_int,
        image: *mut VaImage,
    ) -> VaStatus;
    /// Copy `image` onto `surface`, scaled to the destination rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn vaPutImage(
        dpy: VaDisplay,
        surface: VaSurfaceId,
        image: VaImageId,
        src_x: c_int,
        src_y: c_int,
        src_width: c_uint,
        src_height: c_uint,
        dest_x: c_int,
        dest_y: c_int,
        dest_width: c_uint,
        dest_height: c_uint,
    ) -> VaStatus;
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};

    use super::{
        VaIqMatrixBufferH264, VaPictureH264, VaPictureParameterBufferH264,
        VaSliceParameterBufferH264,
    };

    #[test]
    fn picture_layout_matches_libva() {
        assert_eq!(size_of::<VaPictureH264>(), 36);
        assert_eq!(align_of::<VaPictureH264>(), 4);
    }

    #[test]
    fn picture_param_layout_matches_libva() {
        assert_eq!(size_of::<VaPictureParameterBufferH264>(), 672);
        assert_eq!(align_of::<VaPictureParameterBufferH264>(), 4);
    }

    #[test]
    fn iq_matrix_layout_matches_libva() {
        assert_eq!(size_of::<VaIqMatrixBufferH264>(), 240);
    }

    #[test]
    fn slice_param_layout_matches_libva() {
        assert_eq!(size_of::<VaSliceParameterBufferH264>(), 3128);
        assert_eq!(align_of::<VaSliceParameterBufferH264>(), 4);
    }

    #[test]
    fn hevc_decode_layouts_match_libva() {
        use super::{
            VaIqMatrixBufferHevc, VaPictureParameterBufferHevc, VaSliceParameterBufferHevc,
        };

        assert_eq!(size_of::<VaPictureParameterBufferHevc>(), 604);
        assert_eq!(align_of::<VaPictureParameterBufferHevc>(), 4);
        assert_eq!(size_of::<VaSliceParameterBufferHevc>(), 264);
        assert_eq!(align_of::<VaSliceParameterBufferHevc>(), 4);
        assert_eq!(size_of::<VaIqMatrixBufferHevc>(), 1016);
        assert_eq!(align_of::<VaIqMatrixBufferHevc>(), 4);
    }

    #[test]
    fn av1_decode_layouts_match_libva() {
        use super::{
            VaDecPictureParameterBufferAv1, VaFilmGrainAv1, VaSegmentationAv1,
            VaSliceParameterBufferAv1, VaWarpedMotionAv1,
        };

        assert_eq!(size_of::<VaSegmentationAv1>(), 156);
        assert_eq!(align_of::<VaSegmentationAv1>(), 4);
        assert_eq!(size_of::<VaFilmGrainAv1>(), 176);
        assert_eq!(align_of::<VaFilmGrainAv1>(), 4);
        assert_eq!(size_of::<VaWarpedMotionAv1>(), 56);
        assert_eq!(align_of::<VaWarpedMotionAv1>(), 4);
        assert_eq!(size_of::<VaDecPictureParameterBufferAv1>(), 1160);
        assert_eq!(align_of::<VaDecPictureParameterBufferAv1>(), 8);
        assert_eq!(size_of::<VaSliceParameterBufferAv1>(), 40);
        assert_eq!(align_of::<VaSliceParameterBufferAv1>(), 4);
    }

    #[test]
    fn image_layout_matches_libva() {
        use super::{VaImage, VaImageFormat};

        assert_eq!(size_of::<VaImageFormat>(), 48);
        assert_eq!(size_of::<VaImage>(), 120);
    }

    #[test]
    fn config_attrib_layout_matches_libva() {
        use super::VaConfigAttrib;

        assert_eq!(size_of::<VaConfigAttrib>(), 8);
    }

    #[test]
    fn encode_layouts_match_libva() {
        use super::{
            VaEncPackedHeaderParameterBuffer, VaEncPictureParameterBufferH264,
            VaEncPictureParameterBufferHevc, VaEncSequenceParameterBufferH264,
            VaEncSequenceParameterBufferHevc, VaEncSliceParameterBufferH264,
            VaEncSliceParameterBufferHevc, VaPictureHevc,
        };

        assert_eq!(size_of::<VaPictureHevc>(), 28);
        assert_eq!(size_of::<VaEncSequenceParameterBufferH264>(), 1132);
        assert_eq!(size_of::<VaEncPictureParameterBufferH264>(), 648);
        assert_eq!(size_of::<VaEncSliceParameterBufferH264>(), 3140);
        assert_eq!(size_of::<VaEncSequenceParameterBufferHevc>(), 116);
        assert_eq!(size_of::<VaEncPictureParameterBufferHevc>(), 576);
        assert_eq!(size_of::<VaEncSliceParameterBufferHevc>(), 1076);
        assert_eq!(size_of::<VaEncPackedHeaderParameterBuffer>(), 28);
    }

    #[test]
    fn export_layouts_match_libva() {
        use super::{
            VaCodedBufferSegment, VaDrmPrimeDescriptor, VaDrmPrimeLayer, VaDrmPrimeObject,
        };

        assert_eq!(size_of::<VaCodedBufferSegment>(), 48);
        assert_eq!(size_of::<VaDrmPrimeObject>(), 16);
        assert_eq!(size_of::<VaDrmPrimeLayer>(), 56);
        assert_eq!(size_of::<VaDrmPrimeDescriptor>(), 312);
    }

    #[test]
    fn vpp_layout_matches_libva() {
        use super::{VaProcColorProperties, VaProcPipelineParameterBuffer};

        assert_eq!(size_of::<VaProcColorProperties>(), 8);
        assert_eq!(size_of::<VaProcPipelineParameterBuffer>(), 224);
    }
}
