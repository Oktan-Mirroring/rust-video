//! Minimal libva rebind for H.264 decode over DRM.
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

/// `VAPictureH264` reference flags.
pub const VA_PICTURE_H264_INVALID: u32 = 0x0000_0001;
/// Short-term reference flag.
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x0000_0008;
/// Long-term reference flag.
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x0000_0010;

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
}
