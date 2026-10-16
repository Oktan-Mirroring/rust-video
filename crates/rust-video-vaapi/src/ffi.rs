//! Minimal libva rebind for decode, encode, and video processing over DRM.
//!
//! Layouts mirror `/usr/include/va/va.h` from libva 1.22.0; the byte
//! sizes pinned below are that contract.

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
