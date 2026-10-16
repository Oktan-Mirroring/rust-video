//! Encode session core: owned GPU resources shared by both codecs.
//!
//! IDR-only encode: every picture codes without references, so one DPB
//! slot and no reference lists. The driver emits parameter sets from the
//! session sets below, and the feedback query reports the coded size.

use std::sync::Arc;

use ash::vk::native::{
    __BindgenBitfieldUnit, StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_ENABLED,
    StdVideoH264PictureParameterSet, StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_I,
    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH, StdVideoH264SequenceParameterSet,
    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420, StdVideoH265DecPicBufMgr,
    StdVideoH265PictureParameterSet, StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR,
    StdVideoH265ProfileTierLevel, StdVideoH265SequenceParameterSet, StdVideoH265ShortTermRefPicSet,
    StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I, StdVideoH265VideoParameterSet,
};
use ash::{khr, vk};

use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::raw::{
    CommandPool, DeviceExport, EncodeFeedback, EncodeQueue, ExportPair, GpuBuffer, GpuImage,
    GpuView, ImportPair, SessionParameters, VideoSession, VkFence,
};
use crate::session::decode::{
    layout_barrier, nv12_copy_regions, picture_resource, plane_copy, reference_slot,
};
use crate::session::geometry::{h264_level_to_std, h265_level_to_std};
use crate::session::params::{
    h264_encode_params_add, h264_encode_params_create, h265_encode_params_add,
    h265_encode_params_create, H264EncodeProfileChain, H265EncodeProfileChain,
    H264_ENCODE_STD_NAME, H265_ENCODE_STD_NAME,
};

/// Bitstream staging size in bytes: the largest coded picture the session takes.
pub(crate) const BITSTREAM_BYTES: u64 = 16 << 20;

/// Rate-control frame rate numerator in frames per second.
const RC_FPS: u32 = 30;

/// Rate-control frame rate denominator.
const RC_FPS_DENOM: u32 = 1;

/// Encode codec behind one session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EncodeCodec {
    /// H.264 encode.
    H264,
    /// H.265 encode.
    H265,
}

/// IDR stamp of one coded picture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct IdrStamp {
    /// IDR id wrapping per picture.
    idr_id: u16,
    /// Picture order count.
    poc: i32,
}

impl IdrStamp {
    /// Stamp with `idr_id` and `poc`.
    #[must_use]
    pub fn new(idr_id: u16, poc: i32) -> Self {
        Self { idr_id, poc }
    }
}

/// CBR rate control for one extent.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RateControl {
    /// Average bitrate in bits per second.
    average_bitrate: u64,
    /// Peak bitrate in bits per second.
    max_bitrate: u64,
}

impl RateControl {
    /// CBR control for `extent` at [`RC_FPS`] frames per second.
    #[must_use]
    pub fn cbr_for(extent: vk::Extent2D) -> Self {
        let frame_bits = u64::from(extent.width) * u64::from(extent.height) * 3 / 2 * 8;
        let average = (frame_bits * u64::from(RC_FPS) / 10).max(64_000);
        Self {
            average_bitrate: average,
            max_bitrate: average,
        }
    }
}

/// Standard parameter sets of one encode session.
pub(crate) enum EncodeSets {
    /// H.264 sets without heap pointers.
    H264 {
        /// Sequence set.
        sps: StdVideoH264SequenceParameterSet,
        /// Picture set.
        pps: StdVideoH264PictureParameterSet,
    },
    /// H.265 sets with owned profile and DPB storage.
    H265 {
        /// Profile tier level behind the set views.
        ptl: Box<StdVideoH265ProfileTierLevel>,
        /// DPB manager behind the set views.
        dpb: Box<StdVideoH265DecPicBufMgr>,
        /// Lone empty short-term reference set behind the sequence view.
        rps: Box<[StdVideoH265ShortTermRefPicSet; 1]>,
        /// Padded extent in luma samples.
        padded: (u32, u32),
        /// Conformance window flag.
        conformance: bool,
        /// Conformance right offset in chroma samples.
        crop_right: u32,
        /// Conformance bottom offset in chroma samples.
        crop_bottom: u32,
    },
}

impl EncodeSets {
    /// H.264 sets for `width`x`height` under `profile` and `level`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no level covers the extent.
    pub fn h264(width: u32, height: u32, profile: u32, level: u8) -> Result<Self, VulkanError> {
        let padded_w = width.div_ceil(16) * 16;
        let padded_h = height.div_ceil(16) * 16;
        let std_level =
            h264_level_to_std(level, false).ok_or(VulkanError::OverCaps { what: "level" })?;
        let mut flags = ash::vk::native::StdVideoH264SpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 2]),
            __bindgen_padding_0: 0,
        };
        flags.set_direct_8x8_inference_flag(1);
        flags.set_frame_mbs_only_flag(1);
        let cropping = padded_w != width || padded_h != height;
        flags.set_frame_cropping_flag(u32::from(cropping));
        let sps = StdVideoH264SequenceParameterSet {
            flags,
            profile_idc: profile,
            level_idc: std_level,
            chroma_format_idc: StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
            seq_parameter_set_id: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames_in_pic_order_cnt_cycle: 0,
            max_num_ref_frames: 1,
            reserved1: 0,
            pic_width_in_mbs_minus1: padded_w / 16 - 1,
            pic_height_in_map_units_minus1: padded_h / 16 - 1,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: (padded_w - width) / 2,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: (padded_h - height) / 2,
            reserved2: 0,
            pOffsetForRefFrame: std::ptr::null(),
            pScalingLists: std::ptr::null(),
            pSequenceParameterSetVui: std::ptr::null(),
        };
        let mut pps_flags = ash::vk::native::StdVideoH264PpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
            __bindgen_padding_0: [0; 3],
        };
        pps_flags.set_entropy_coding_mode_flag(u32::from(
            profile != StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
        ));
        pps_flags.set_deblocking_filter_control_present_flag(1);
        pps_flags.set_transform_8x8_mode_flag(u32::from(
            profile == StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
        ));
        let pps = StdVideoH264PictureParameterSet {
            flags: pps_flags,
            seq_parameter_set_id: 0,
            pic_parameter_set_id: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_bipred_idc:
                StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pScalingLists: std::ptr::null(),
        };
        Ok(Self::H264 { sps, pps })
    }

    /// H.265 sets for `width`x`height` under `profile` and `level`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no level covers the extent.
    pub fn h265(width: u32, height: u32, profile: u32, level: u8) -> Result<Self, VulkanError> {
        let padded_w = width.div_ceil(8) * 8;
        let padded_h = height.div_ceil(8) * 8;
        let std_level = h265_level_to_std(level).ok_or(VulkanError::OverCaps { what: "level" })?;
        let mut ptl_flags = ash::vk::native::StdVideoH265ProfileTierLevelFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
            __bindgen_padding_0: [0; 3],
        };
        ptl_flags.set_general_progressive_source_flag(1);
        ptl_flags.set_general_non_packed_constraint_flag(1);
        ptl_flags.set_general_frame_only_constraint_flag(1);
        let ptl = Box::new(StdVideoH265ProfileTierLevel {
            flags: ptl_flags,
            general_profile_idc: profile,
            general_level_idc: std_level,
        });
        let dpb = Box::new(StdVideoH265DecPicBufMgr {
            max_latency_increase_plus1: [1, 0, 0, 0, 0, 0, 0],
            max_dec_pic_buffering_minus1: [0; 7],
            max_num_reorder_pics: [0; 7],
        });
        let rps = Box::new([StdVideoH265ShortTermRefPicSet {
            flags: ash::vk::native::StdVideoH265ShortTermRefPicSetFlags {
                _bitfield_align_1: [],
                _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
                __bindgen_padding_0: [0; 3],
            },
            delta_idx_minus1: 0,
            use_delta_flag: 0,
            abs_delta_rps_minus1: 0,
            used_by_curr_pic_flag: 0,
            used_by_curr_pic_s0_flag: 0,
            used_by_curr_pic_s1_flag: 0,
            reserved1: 0,
            reserved2: 0,
            reserved3: 0,
            num_negative_pics: 0,
            num_positive_pics: 0,
            delta_poc_s0_minus1: [0; 16],
            delta_poc_s1_minus1: [0; 16],
        }]);
        Ok(Self::H265 {
            ptl,
            dpb,
            rps,
            padded: (padded_w, padded_h),
            conformance: padded_w != width || padded_h != height,
            crop_right: (padded_w - width) / 2,
            crop_bottom: (padded_h - height) / 2,
        })
    }

    /// H.265 video set view over the owned storage.
    fn h265_vps(&self) -> Option<StdVideoH265VideoParameterSet> {
        let Self::H265 { ptl, dpb, .. } = self else {
            return None;
        };
        let flags = ash::vk::native::StdVideoH265VpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
            __bindgen_padding_0: [0; 3],
        };
        Some(StdVideoH265VideoParameterSet {
            flags,
            vps_video_parameter_set_id: 0,
            vps_max_sub_layers_minus1: 0,
            reserved1: 0,
            reserved2: 0,
            vps_num_units_in_tick: 0,
            vps_time_scale: 0,
            vps_num_ticks_poc_diff_one_minus1: 0,
            reserved3: 0,
            pDecPicBufMgr: std::ptr::from_ref(dpb.as_ref()),
            pHrdParameters: std::ptr::null(),
            pProfileTierLevel: std::ptr::from_ref(ptl.as_ref()),
        })
    }

    /// H.265 sequence set view over the owned storage.
    fn h265_sps(&self) -> Option<StdVideoH265SequenceParameterSet> {
        let Self::H265 {
            ptl,
            dpb,
            rps,
            padded,
            conformance,
            crop_right,
            crop_bottom,
        } = self
        else {
            return None;
        };
        let mut flags = ash::vk::native::StdVideoH265SpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        };
        flags.set_conformance_window_flag(u32::from(*conformance));
        flags.set_amp_enabled_flag(1);
        flags.set_sample_adaptive_offset_enabled_flag(1);
        Some(StdVideoH265SequenceParameterSet {
            flags,
            chroma_format_idc: StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
            pic_width_in_luma_samples: padded.0,
            pic_height_in_luma_samples: padded.1,
            sps_video_parameter_set_id: 0,
            sps_max_sub_layers_minus1: 0,
            sps_seq_parameter_set_id: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            log2_min_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_luma_coding_block_size: 2,
            log2_min_luma_transform_block_size_minus2: 0,
            log2_diff_max_min_luma_transform_block_size: 2,
            max_transform_hierarchy_depth_inter: 1,
            max_transform_hierarchy_depth_intra: 1,
            num_short_term_ref_pic_sets: 1,
            num_long_term_ref_pics_sps: 0,
            pcm_sample_bit_depth_luma_minus1: 0,
            pcm_sample_bit_depth_chroma_minus1: 0,
            log2_min_pcm_luma_coding_block_size_minus3: 0,
            log2_diff_max_min_pcm_luma_coding_block_size: 0,
            reserved1: 0,
            reserved2: 0,
            palette_max_size: 0,
            delta_palette_max_predictor_size: 0,
            motion_vector_resolution_control_idc: 0,
            sps_num_palette_predictor_initializers_minus1: 0,
            conf_win_left_offset: 0,
            conf_win_right_offset: *crop_right,
            conf_win_top_offset: 0,
            conf_win_bottom_offset: *crop_bottom,
            pProfileTierLevel: std::ptr::from_ref(ptl.as_ref()),
            pDecPicBufMgr: std::ptr::from_ref(dpb.as_ref()),
            pScalingLists: std::ptr::null(),
            pShortTermRefPicSet: std::ptr::from_ref(&rps[0]),
            pLongTermRefPicsSps: std::ptr::null(),
            pSequenceParameterSetVui: std::ptr::null(),
            pPredictorPaletteEntries: std::ptr::null(),
        })
    }

    /// H.265 picture set view; no heap pointers.
    fn h265_pps(&self) -> Option<StdVideoH265PictureParameterSet> {
        let Self::H265 { .. } = self else {
            return None;
        };
        let flags = ash::vk::native::StdVideoH265PpsFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        };
        Some(StdVideoH265PictureParameterSet {
            flags,
            pps_pic_parameter_set_id: 0,
            pps_seq_parameter_set_id: 0,
            sps_video_parameter_set_id: 0,
            num_extra_slice_header_bits: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            pps_beta_offset_div2: 0,
            pps_tc_offset_div2: 0,
            log2_parallel_merge_level_minus2: 2,
            log2_max_transform_skip_block_size_minus2: 0,
            diff_cu_chroma_qp_offset_depth: 0,
            chroma_qp_offset_list_len_minus1: 0,
            cb_qp_offset_list: [0; 6],
            cr_qp_offset_list: [0; 6],
            log2_sao_offset_scale_luma: 0,
            log2_sao_offset_scale_chroma: 0,
            pps_act_y_qp_offset_plus5: 5,
            pps_act_cb_qp_offset_plus5: 5,
            pps_act_cr_qp_offset_plus3: 3,
            pps_num_palette_predictor_initializers: 0,
            luma_bit_depth_entry_minus8: 0,
            chroma_bit_depth_entry_minus8: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            reserved1: 0,
            reserved2: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            reserved3: 0,
            pScalingLists: std::ptr::null(),
            pPredictorPaletteEntries: std::ptr::null(),
        })
    }
}

/// Even nonzero `width`x`height` inside the caps extent.
///
/// # Errors
///
/// Returns `Err` on a degenerate or over-caps extent.
fn check_extent(
    facts: &crate::ladder::EncodeFacts,
    width: u32,
    height: u32,
) -> Result<(), VulkanError> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(VulkanError::BadExtent { width, height });
    }
    if width > facts.coded_extent.0 || height > facts.coded_extent.1 {
        return Err(VulkanError::OverCaps {
            what: "coded extent",
        });
    }
    Ok(())
}

/// Standard sets of `codec` for `width`x`height` under `facts`.
///
/// # Errors
///
/// Returns `Err` when no level covers the extent.
fn build_sets(
    codec: EncodeCodec,
    facts: &crate::ladder::EncodeFacts,
    width: u32,
    height: u32,
) -> Result<EncodeSets, VulkanError> {
    match codec {
        EncodeCodec::H264 => {
            let mbs = width.div_ceil(16) * height.div_ceil(16);
            let level = h264_level_for(mbs).ok_or(VulkanError::OverCaps {
                what: "coded extent",
            })?;
            EncodeSets::h264(width, height, facts.profile, level)
        }
        EncodeCodec::H265 => {
            let samples = u64::from(width) * u64::from(height);
            let level = h265_level_for(samples).ok_or(VulkanError::OverCaps {
                what: "coded extent",
            })?;
            EncodeSets::h265(width, height, facts.profile, level)
        }
    }
}

/// CBR layer of `rate` at the session frame rate.
fn rc_layer(rate: &RateControl) -> vk::VideoEncodeRateControlLayerInfoKHR<'static> {
    vk::VideoEncodeRateControlLayerInfoKHR::default()
        .average_bitrate(rate.average_bitrate)
        .max_bitrate(rate.max_bitrate)
        .frame_rate_numerator(RC_FPS)
        .frame_rate_denominator(RC_FPS_DENOM)
}

/// Virtual buffer window in milliseconds behind CBR control.
const VIRTUAL_BUFFER_MS: u32 = 1_000;

/// Initial virtual buffer fullness in milliseconds behind CBR control.
const INITIAL_BUFFER_MS: u32 = 500;

/// CBR control over `layer`.
///
/// Every begin carries the control: drivers set rate control state
/// from the begin chain, the FFmpeg-compatible shape. Newer
/// validation wants the state preconfigured through a control
/// command instead and flags each begin; the control path fails on
/// drivers, so the begin chain stays.
fn rc_control<'a>(
    layer: &'a vk::VideoEncodeRateControlLayerInfoKHR<'_>,
) -> vk::VideoEncodeRateControlInfoKHR<'a> {
    vk::VideoEncodeRateControlInfoKHR {
        rate_control_mode: vk::VideoEncodeRateControlModeFlagsKHR::CBR,
        layer_count: 1,
        p_layers: std::ptr::from_ref(layer),
        virtual_buffer_size_in_ms: VIRTUAL_BUFFER_MS,
        initial_virtual_buffer_size_in_ms: INITIAL_BUFFER_MS,
        ..Default::default()
    }
}

/// H.264 wire level for `macroblocks`, if any level covers it.
pub(crate) fn h264_level_for(macroblocks: u32) -> Option<u8> {
    const TABLE: [(u32, u8); 10] = [
        (99, 10),
        (396, 20),
        (792, 21),
        (1_620, 22),
        (3_600, 31),
        (5_120, 32),
        (8_192, 40),
        (8_704, 42),
        (22_080, 50),
        (36_864, 52),
    ];
    TABLE
        .iter()
        .find(|(max, _)| macroblocks <= *max)
        .map(|(_, level)| *level)
}

/// H.265 wire level for `samples` luma samples, if any level covers it.
pub(crate) fn h265_level_for(samples: u64) -> Option<u8> {
    const TABLE: [(u64, u8); 8] = [
        (36_864, 30),
        (122_880, 60),
        (245_760, 63),
        (552_960, 90),
        (983_040, 93),
        (2_228_224, 120),
        (8_912_896, 150),
        (35_651_584, 180),
    ];
    TABLE
        .iter()
        .find(|(max, _)| samples <= *max)
        .map(|(_, level)| *level)
}

/// Images, buffers, pool, and fence of one encode session.
struct SessionResources {
    /// Source image view; drops before `input` below.
    input_view: GpuView,
    /// Source image for uploaded pictures.
    input: GpuImage,
    /// Reconstructed layer view; drops before `dpb` below.
    dpb_view: GpuView,
    /// Single-layer reconstructed image.
    dpb: GpuImage,
    /// NV12 upload staging buffer.
    staging: GpuBuffer,
    /// Coded bitstream buffer.
    bitstream: GpuBuffer,
    /// Coded-size feedback pool.
    feedback: EncodeFeedback,
    /// Command pool behind the submit slot.
    pool: CommandPool,
    /// Command buffer, allocated once and reset per submit.
    cmd: vk::CommandBuffer,
    /// Submission fence, reset per submit.
    fence: VkFence,
    /// Encode queue functions.
    encode: EncodeQueue,
}

/// Owned encode session over a negotiated device.
///
/// Field order is the drop order: parameters free before the session they
/// belong to, and GPU objects free before the pool and device clones.
pub(crate) struct EncodeSession {
    /// Logical device.
    device: ash::Device,
    /// Video queue.
    queue: vk::Queue,
    /// Queue submission mutex, shared with the device.
    queue_lock: std::sync::Arc<std::sync::Mutex<()>>,
    /// Encode queue functions.
    encode: EncodeQueue,
    /// Session parameters; drops before `video` below.
    params: SessionParameters,
    /// Video session.
    video: VideoSession,
    /// Encode codec behind the session.
    codec: EncodeCodec,
    /// Coded extent in luma samples.
    extent: vk::Extent2D,
    /// Source picture format.
    format: vk::Format,
    /// Memory properties behind DMA-BUF import.
    props: vk::PhysicalDeviceMemoryProperties,
    /// DMA-BUF import functions, absent without the extensions.
    ext: Option<khr::external_memory_fd::Device>,
    /// Import of the last encode, reused while the pair holds.
    import: Option<(Arc<ExportPair>, ImportPair)>,
    /// Source image view; drops before `input` below.
    input_view: GpuView,
    /// Source image for uploaded pictures.
    input: GpuImage,
    /// Tracked source layout.
    input_layout: vk::ImageLayout,
    /// Reconstructed layer view; drops before `dpb` below.
    dpb_view: GpuView,
    /// Single-layer reconstructed image.
    dpb: GpuImage,
    /// Tracked reconstructed layout.
    dpb_layout: vk::ImageLayout,
    /// NV12 upload staging buffer.
    staging: GpuBuffer,
    /// Coded bitstream buffer.
    bitstream: GpuBuffer,
    /// Coded-size feedback pool.
    feedback: EncodeFeedback,
    /// Command pool behind the submit slot.
    pool: CommandPool,
    /// Command buffer, allocated once and reset per submit.
    cmd: vk::CommandBuffer,
    /// Submission fence, reset per submit.
    fence: VkFence,
    /// True after a submit failure; the fence state is unknown.
    wedged: bool,
    /// CBR rate control.
    rate: RateControl,
    /// Coding scope opened at least once; the slot holds the last IDR.
    /// Stamp of the picture in the slot, if any.
    prev: Option<IdrStamp>,
    /// Driver-encoded parameter sets prepended to every packet.
    headers: Vec<u8>,
}

impl EncodeSession {
    /// Session over `device` for `codec` at `width`x`height`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the device negotiated decode, the extent
    /// escapes the adapter caps, or session creation fails.
    pub(crate) fn open(
        device: &VulkanDevice,
        codec: EncodeCodec,
        width: u32,
        height: u32,
    ) -> Result<Self, VulkanError> {
        let crate::ladder::OpCaps::Encode(facts) = device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        check_extent(&facts, width, height)?;
        if !facts
            .feedback
            .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
        {
            return Err(VulkanError::OverCaps {
                what: "encode feedback",
            });
        }
        if !facts
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
        {
            return Err(VulkanError::OverCaps {
                what: "rate control",
            });
        }
        let raw = device.raw().device();
        let properties =
            crate::raw::RawDevice::memory_properties(device.backend(), device.adapter());
        let extent = vk::Extent2D { width, height };
        let sets = build_sets(codec, &facts, width, height)?;
        let h264_chain = H264EncodeProfileChain::new(facts.profile);
        let h265_chain = H265EncodeProfileChain::new(facts.profile);
        let profile = match codec {
            EncodeCodec::H264 => h264_chain.base(),
            EncodeCodec::H265 => h265_chain.base(),
        };
        let std_version = match codec {
            EncodeCodec::H264 => {
                crate::raw::std_header_version(H264_ENCODE_STD_NAME, facts.std_version)
            }
            EncodeCodec::H265 => {
                crate::raw::std_header_version(H265_ENCODE_STD_NAME, facts.std_version)
            }
        };
        let usage =
            vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR;
        let formats = device
            .backend()
            .video_formats(device.adapter(), &profile, usage)?;
        let format = crate::raw::pick_video_format(&formats).ok_or(VulkanError::NoVideoFormat)?;
        let resources = Self::open_resources(device, &properties, format, extent, &profile)?;
        let desc = crate::raw::VideoSessionDesc {
            profile: &profile,
            queue_family: device.queue_family(),
            format,
            extent,
            dpb_slots: 1,
            active_refs: 1,
            std_version: &std_version,
        };
        let session = VideoSession::create(raw, device.backend().instance(), &properties, &desc)?;
        let mut template = match codec {
            EncodeCodec::H264 => {
                crate::session::params::ParamsTemplate::H264Encode(h264_encode_params_create())
            }
            EncodeCodec::H265 => {
                crate::session::params::ParamsTemplate::H265Encode(h265_encode_params_create())
            }
        };
        let mut params = Self::create_params(&session, &mut template)?;
        Self::register_sets(&sets, &mut params)?;
        let headers = match codec {
            EncodeCodec::H264 => resources.encode.encoded_parameters_h264(params.params())?,
            EncodeCodec::H265 => resources.encode.encoded_parameters_h265(params.params())?,
        };
        Ok(Self {
            device: raw.clone(),
            queue: device.queue(),
            queue_lock: device.queue_lock().clone(),
            encode: resources.encode,
            params,
            video: session,
            codec,
            extent,
            format,
            props: properties,
            ext: device.external_fd(),
            import: None,
            input_view: resources.input_view,
            input: resources.input,
            input_layout: vk::ImageLayout::UNDEFINED,
            dpb_view: resources.dpb_view,
            dpb: resources.dpb,
            dpb_layout: vk::ImageLayout::UNDEFINED,
            staging: resources.staging,
            bitstream: resources.bitstream,
            feedback: resources.feedback,
            pool: resources.pool,
            cmd: resources.cmd,
            fence: resources.fence,
            wedged: false,
            rate: RateControl::cbr_for(extent),
            prev: None,
            headers,
        })
    }

    /// Images, buffers, pool, and fence for `profile` at `extent`.
    fn open_resources(
        device: &VulkanDevice,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        profile: &vk::VideoProfileInfoKHR<'_>,
    ) -> Result<SessionResources, VulkanError> {
        let raw = device.raw().device();
        let profiles = std::slice::from_ref(profile);
        let input = Self::create_image(
            raw,
            properties,
            format,
            extent,
            vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
            profiles,
        )?;
        let input_view = input.layer_view(0)?;
        let dpb = Self::create_image(
            raw,
            properties,
            format,
            extent,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            profiles,
        )?;
        let dpb_view = dpb.layer_view(0)?;
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let staging = GpuBuffer::create(
            raw,
            properties,
            crate::session::decode::nv12_bytes(extent),
            vk::BufferUsageFlags::TRANSFER_SRC,
            host,
            &[],
        )?;
        let bitstream = GpuBuffer::create(
            raw,
            properties,
            BITSTREAM_BYTES,
            vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR,
            host,
            profiles,
        )?;
        let feedback = EncodeFeedback::create(
            raw,
            1,
            vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
            profile,
        )?;
        let pool = CommandPool::create(raw, device.queue_family())?;
        let cmd = pool.allocate()?;
        let fence = VkFence::new(raw, false)?;
        let encode = EncodeQueue::new(device.backend().instance(), raw);
        Ok(SessionResources {
            input_view,
            input,
            dpb_view,
            dpb,
            staging,
            bitstream,
            feedback,
            pool,
            cmd,
            fence,
            encode,
        })
    }

    /// Image of `format` at `extent` with `usage` over `profiles`, memory bound.
    fn create_image(
        raw: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        usage: vk::ImageUsageFlags,
        profiles: &[vk::VideoProfileInfoKHR<'_>],
    ) -> Result<GpuImage, VulkanError> {
        GpuImage::create(
            raw,
            properties,
            &crate::raw::GpuImageDesc {
                format,
                extent,
                layers: 1,
                tiling: vk::ImageTiling::OPTIMAL,
                usage,
                flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            },
            profiles,
        )
    }

    /// Empty session parameters over `template`.
    fn create_params(
        session: &VideoSession,
        template: &mut crate::session::params::ParamsTemplate,
    ) -> Result<SessionParameters, VulkanError> {
        let create = vk::VideoSessionParametersCreateInfoKHR {
            video_session: session.session(),
            ..Default::default()
        };
        match template {
            crate::session::params::ParamsTemplate::H264Encode(codec) => {
                SessionParameters::create(session, &create.push_next(codec))
            }
            crate::session::params::ParamsTemplate::H265Encode(codec) => {
                SessionParameters::create(session, &create.push_next(codec))
            }
            crate::session::params::ParamsTemplate::H264(_)
            | crate::session::params::ParamsTemplate::H265(_) => {
                Err(VulkanError::OverCaps { what: "codec op" })
            }
        }
    }

    /// Register the session sets through one parameters update.
    fn register_sets(sets: &EncodeSets, params: &mut SessionParameters) -> Result<(), VulkanError> {
        match sets {
            EncodeSets::H264 { sps, pps } => {
                let mut add =
                    h264_encode_params_add(std::slice::from_ref(sps), std::slice::from_ref(pps))?;
                let update = vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
                params.update(&update)?;
            }
            EncodeSets::H265 { .. } => {
                let vps = sets
                    .h265_vps()
                    .ok_or(VulkanError::OverCaps { what: "codec op" })?;
                let sps = sets
                    .h265_sps()
                    .ok_or(VulkanError::OverCaps { what: "codec op" })?;
                let pps = sets
                    .h265_pps()
                    .ok_or(VulkanError::OverCaps { what: "codec op" })?;
                let mut add = h265_encode_params_add(
                    std::slice::from_ref(&vps),
                    std::slice::from_ref(&sps),
                    std::slice::from_ref(&pps),
                )?;
                let update = vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
                params.update(&update)?;
            }
        }
        Ok(())
    }

    /// Coded extent in luma samples.
    pub(crate) fn extent(&self) -> (u32, u32) {
        (self.extent.width, self.extent.height)
    }

    /// Driver-encoded parameter sets prepended to every packet.
    pub(crate) fn headers(&self) -> &[u8] {
        &self.headers
    }

    /// Encode tight NV12 `picture` stamped `idr` into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when recording, submission, or the fence wait fails.
    pub(crate) fn encode_into(
        &mut self,
        picture: &[u8],
        idr: IdrStamp,
        out: &mut Vec<u8>,
    ) -> Result<(), VulkanError> {
        let want =
            usize::try_from(crate::session::decode::nv12_bytes(self.extent)).map_err(|_| {
                crate::error::ParseError::OutOfRange {
                    syntax: "encode picture",
                }
            })?;
        if picture.len() != want {
            return Err(VulkanError::ShortReadback {
                expected: want,
                found: picture.len(),
            });
        }
        self.staging.memory().fill(picture)?;
        self.pool.reset_buffer(self.cmd)?;
        self.record(idr)?;
        self.dispatch(out)
    }

    /// Encode the device `export` stamped `idr` into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the export misses the session format or extent,
    /// the import fails, or recording, submission, or the fence wait fails.
    pub(crate) fn encode_import(
        &mut self,
        export: &DeviceExport,
        idr: IdrStamp,
        out: &mut Vec<u8>,
    ) -> Result<(), VulkanError> {
        if export.source() != self.format {
            return Err(VulkanError::Unsupported {
                what: "encode import format".to_owned(),
            });
        }
        if export.extent() != self.extent() {
            return Err(VulkanError::OverCaps {
                what: "encode extent",
            });
        }
        let reuse = self
            .import
            .as_ref()
            .is_some_and(|(held, _)| Arc::ptr_eq(held, export.pair()));
        if !reuse {
            let Some(ext) = self.ext.clone() else {
                return Err(VulkanError::Unsupported {
                    what: "dma-buf import".to_owned(),
                });
            };
            let import = ImportPair::import(&self.device, &ext, &self.props, export)?;
            self.import = Some((export.pair().clone(), import));
        }
        let Some((_, import)) = self.import.as_ref() else {
            return Err(VulkanError::Unsupported {
                what: "dma-buf import".to_owned(),
            });
        };
        let (y, uv) = (import.y_image(), import.uv_image());
        self.pool.reset_buffer(self.cmd)?;
        self.record_import(idr, y, uv)?;
        self.dispatch(out)
    }

    /// Record upload, encode, and feedback commands.
    fn record(&mut self, idr: IdrStamp) -> Result<(), VulkanError> {
        self.pool.begin_buffer(self.cmd)?;
        self.record_upload_head();
        self.record_tail(idr);
        self.pool.end_buffer(self.cmd)?;
        Ok(())
    }

    /// Record import, encode, and feedback commands.
    fn record_import(
        &mut self,
        idr: IdrStamp,
        y: vk::Image,
        uv: vk::Image,
    ) -> Result<(), VulkanError> {
        self.pool.begin_buffer(self.cmd)?;
        self.record_import_head(y, uv);
        self.record_tail(idr);
        self.pool.end_buffer(self.cmd)?;
        Ok(())
    }

    /// Record the staging upload into the source image.
    fn record_upload_head(&mut self) {
        let recorder = self.pool.recorder(self.cmd);
        let mut barriers = Vec::new();
        if self.input_layout != vk::ImageLayout::TRANSFER_DST_OPTIMAL {
            barriers.push(layout_barrier(
                self.input.image(),
                self.input_layout,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            ));
        }
        if !barriers.is_empty() {
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&barriers));
        }
        let regions = nv12_copy_regions(self.extent, 0);
        recorder.copy_buffer_to_image(
            self.staging.buffer(),
            self.input.image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &regions,
        );
        let to_src = layout_barrier(
            self.input.image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
            vk::ImageAspectFlags::COLOR,
            0,
            1,
        );
        recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[to_src]));
        self.input_layout = vk::ImageLayout::VIDEO_ENCODE_SRC_KHR;
    }

    /// Record the imported planes into the source image.
    fn record_import_head(&mut self, y: vk::Image, uv: vk::Image) {
        let recorder = self.pool.recorder(self.cmd);
        for image in [y, uv] {
            let barrier = layout_barrier(
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[barrier]));
        }
        let mut barriers = Vec::new();
        if self.input_layout != vk::ImageLayout::TRANSFER_DST_OPTIMAL {
            barriers.push(layout_barrier(
                self.input.image(),
                self.input_layout,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            ));
        }
        if !barriers.is_empty() {
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&barriers));
        }
        let y_copy = plane_copy(
            vk::ImageAspectFlags::COLOR,
            0,
            vk::ImageAspectFlags::PLANE_0,
            0,
            self.extent,
        );
        recorder.copy_image(
            y,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            self.input.image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[y_copy],
        );
        let chroma = vk::Extent2D {
            width: self.extent.width / 2,
            height: self.extent.height / 2,
        };
        let uv_copy = plane_copy(
            vk::ImageAspectFlags::COLOR,
            0,
            vk::ImageAspectFlags::PLANE_1,
            0,
            chroma,
        );
        recorder.copy_image(
            uv,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            self.input.image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[uv_copy],
        );
        let to_src = layout_barrier(
            self.input.image(),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::VIDEO_ENCODE_SRC_KHR,
            vk::ImageAspectFlags::COLOR,
            0,
            1,
        );
        recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[to_src]));
        self.input_layout = vk::ImageLayout::VIDEO_ENCODE_SRC_KHR;
    }

    /// Record reconstructed setup, encode, and feedback commands.
    fn record_tail(&mut self, idr: IdrStamp) {
        let recorder = self.pool.recorder(self.cmd);
        if self.dpb_layout != vk::ImageLayout::VIDEO_ENCODE_DPB_KHR {
            let to_dpb = layout_barrier(
                self.dpb.image(),
                self.dpb_layout,
                vk::ImageLayout::VIDEO_ENCODE_DPB_KHR,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[to_dpb]));
        }
        self.dpb_layout = vk::ImageLayout::VIDEO_ENCODE_DPB_KHR;
        recorder.reset_queries(self.feedback.pool(), 0, 1);
        let layer = rc_layer(&self.rate);
        let mut control = rc_control(&layer);
        match self.codec {
            EncodeCodec::H264 => self.record_h264(idr, &mut control),
            EncodeCodec::H265 => self.record_h265(idr, &mut control),
        }
        self.prev = Some(idr);
    }

    /// Record one H.264 IDR encode.
    fn record_h264(&self, idr: IdrStamp, control: &mut vk::VideoEncodeRateControlInfoKHR<'_>) {
        use ash::vk::native::{StdVideoEncodeH264PictureInfo, StdVideoEncodeH264ReferenceInfo};
        let recorder = self.pool.recorder(self.cmd);
        let held = self.prev.map_or(idr.poc, |stamp| stamp.poc);
        let recon = StdVideoEncodeH264ReferenceInfo {
            flags: ash::vk::native::StdVideoEncodeH264ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
            },
            primary_pic_type: StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_I,
            FrameNum: 0,
            PicOrderCnt: held,
            long_term_pic_num: 0,
            long_term_frame_idx: 0,
            temporal_id: 0,
        };
        let recon_dpb = vk::VideoEncodeH264DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&recon),
            ..Default::default()
        };
        let dst = picture_resource(self.dpb_view.view(), self.extent, 0);
        let mut slot = reference_slot(0, &dst);
        slot.p_next = std::ptr::from_ref(&recon_dpb).cast();
        let bind = reference_slot(-1, &dst);
        let (ref_count, ref_slots) = (1, std::ptr::from_ref(&bind));
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: ref_count,
            p_reference_slots: ref_slots,
            ..Default::default()
        };
        let begin = begin.push_next(control);
        self.video.begin_coding(self.cmd, &begin);
        recorder.begin_query(self.feedback.pool(), 0);
        let std_picture = StdVideoEncodeH264PictureInfo {
            flags: ash::vk::native::StdVideoEncodeH264PictureInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
            },
            seq_parameter_set_id: 0,
            pic_parameter_set_id: 0,
            idr_pic_id: idr.idr_id,
            primary_pic_type: StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
            frame_num: 0,
            PicOrderCnt: idr.poc,
            temporal_id: 0,
            reserved1: [0; 3],
            pRefLists: std::ptr::null(),
        };
        let slice_header = ash::vk::native::StdVideoEncodeH264SliceHeader {
            flags: ash::vk::native::StdVideoEncodeH264SliceHeaderFlags {
                _bitfield_align_1: [],
                _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
            },
            first_mb_in_slice: 0,
            slice_type: StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            slice_qp_delta: 0,
            reserved1: 0,
            cabac_init_idc: StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
            disable_deblocking_filter_idc:
                StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_ENABLED,
            pWeightTable: std::ptr::null(),
        };
        let slice = vk::VideoEncodeH264NaluSliceInfoKHR {
            constant_qp: 0,
            p_std_slice_header: std::ptr::from_ref(&slice_header),
            ..Default::default()
        };
        let mut codec = vk::VideoEncodeH264PictureInfoKHR {
            nalu_slice_entry_count: 1,
            p_nalu_slice_entries: std::ptr::from_ref(&slice),
            p_std_picture_info: std::ptr::from_ref(&std_picture),
            ..Default::default()
        };
        let src = picture_resource(self.input_view.view(), self.extent, 0);
        let info = vk::VideoEncodeInfoKHR {
            dst_buffer: self.bitstream.buffer(),
            dst_buffer_offset: 0,
            dst_buffer_range: BITSTREAM_BYTES,
            src_picture_resource: src,
            p_setup_reference_slot: std::ptr::from_ref(&slot),
            ..Default::default()
        };
        let info = info.push_next(&mut codec);
        self.encode.encode_video(self.cmd, &info);
        recorder.end_query(self.feedback.pool(), 0);
        self.video
            .end_coding(self.cmd, &vk::VideoEndCodingInfoKHR::default());
    }

    /// Record one H.265 IDR encode.
    fn record_h265(&self, idr: IdrStamp, control: &mut vk::VideoEncodeRateControlInfoKHR<'_>) {
        use ash::vk::native::{StdVideoEncodeH265PictureInfo, StdVideoEncodeH265ReferenceInfo};
        let recorder = self.pool.recorder(self.cmd);
        let mut picture_flags = ash::vk::native::StdVideoEncodeH265PictureInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        };
        picture_flags.set_short_term_ref_pic_set_sps_flag(1);
        picture_flags.set_pic_output_flag(1);
        let held = self.prev.map_or(idr.poc, |stamp| stamp.poc);
        let recon = StdVideoEncodeH265ReferenceInfo {
            flags: ash::vk::native::StdVideoEncodeH265ReferenceInfoFlags {
                _bitfield_align_1: [],
                _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
            },
            pic_type: StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR,
            PicOrderCntVal: held,
            TemporalId: 0,
        };
        let recon_dpb = vk::VideoEncodeH265DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&recon),
            ..Default::default()
        };
        let dst = picture_resource(self.dpb_view.view(), self.extent, 0);
        let mut slot = reference_slot(0, &dst);
        slot.p_next = std::ptr::from_ref(&recon_dpb).cast();
        let bind = reference_slot(-1, &dst);
        let (ref_count, ref_slots) = (1, std::ptr::from_ref(&bind));
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: ref_count,
            p_reference_slots: ref_slots,
            ..Default::default()
        };
        let begin = begin.push_next(control);
        self.video.begin_coding(self.cmd, &begin);
        recorder.begin_query(self.feedback.pool(), 0);
        let std_picture = StdVideoEncodeH265PictureInfo {
            flags: picture_flags,
            pic_type: StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR,
            sps_video_parameter_set_id: 0,
            pps_seq_parameter_set_id: 0,
            pps_pic_parameter_set_id: 0,
            short_term_ref_pic_set_idx: 0,
            PicOrderCntVal: idr.poc,
            TemporalId: 0,
            reserved1: [0; 7],
            pRefLists: std::ptr::null(),
            pShortTermRefPicSet: std::ptr::null(),
            pLongTermRefPics: std::ptr::null(),
        };
        let mut slice_flags = ash::vk::native::StdVideoEncodeH265SliceSegmentHeaderFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        };
        slice_flags.set_first_slice_segment_in_pic_flag(1);
        let slice_header = ash::vk::native::StdVideoEncodeH265SliceSegmentHeader {
            flags: slice_flags,
            slice_type: StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I,
            slice_segment_address: 0,
            collocated_ref_idx: 0,
            MaxNumMergeCand: 5,
            slice_cb_qp_offset: 0,
            slice_cr_qp_offset: 0,
            slice_beta_offset_div2: 0,
            slice_tc_offset_div2: 0,
            slice_act_y_qp_offset: 0,
            slice_act_cb_qp_offset: 0,
            slice_act_cr_qp_offset: 0,
            slice_qp_delta: 0,
            reserved1: 0,
            pWeightTable: std::ptr::null(),
        };
        let slice = vk::VideoEncodeH265NaluSliceSegmentInfoKHR {
            constant_qp: 0,
            p_std_slice_segment_header: std::ptr::from_ref(&slice_header),
            ..Default::default()
        };
        let mut codec = vk::VideoEncodeH265PictureInfoKHR {
            nalu_slice_segment_entry_count: 1,
            p_nalu_slice_segment_entries: std::ptr::from_ref(&slice),
            p_std_picture_info: std::ptr::from_ref(&std_picture),
            ..Default::default()
        };
        let src = picture_resource(self.input_view.view(), self.extent, 0);
        let info = vk::VideoEncodeInfoKHR {
            dst_buffer: self.bitstream.buffer(),
            dst_buffer_offset: 0,
            dst_buffer_range: BITSTREAM_BYTES,
            src_picture_resource: src,
            p_setup_reference_slot: std::ptr::from_ref(&slot),
            ..Default::default()
        };
        let info = info.push_next(&mut codec);
        self.encode.encode_video(self.cmd, &info);
        recorder.end_query(self.feedback.pool(), 0);
        self.video
            .end_coding(self.cmd, &vk::VideoEndCodingInfoKHR::default());
    }

    /// Submit recorded commands, wait, and map the coded prefix.
    ///
    /// A failure wedges the session: the fence may still be submitted,
    /// so later submits fail without touching the queue.
    fn dispatch(&mut self, out: &mut Vec<u8>) -> Result<(), VulkanError> {
        if self.wedged {
            return Err(VulkanError::Timeout);
        }
        if let Err(error) = crate::raw::submit_and_wait(
            &self.device,
            &self.queue_lock,
            self.queue,
            self.cmd,
            &self.fence,
            crate::raw::SUBMIT_TIMEOUT_NS,
        ) {
            self.wedged = true;
            return Err(error);
        }
        let mut written = [0_u64; 1];
        self.feedback.results(0, &mut written)?;
        let len =
            usize::try_from(written[0]).map_err(|_| crate::error::ParseError::OutOfRange {
                syntax: "coded size",
            })?;
        let cap =
            usize::try_from(BITSTREAM_BYTES).map_err(|_| crate::error::ParseError::OutOfRange {
                syntax: "coded size",
            })?;
        if len > cap {
            return Err(VulkanError::ShortReadback {
                expected: cap,
                found: len,
            });
        }
        self.bitstream.memory().read_to_vec(out, len)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{h264_level_for, h265_level_for, EncodeSets, RateControl};

    #[test]
    fn levels_cover_picture_sizes() {
        assert_eq!(h264_level_for(99), Some(10));
        assert_eq!(h264_level_for(8_160), Some(40));
        assert_eq!(h264_level_for(36_864), Some(52));
        assert_eq!(h264_level_for(36_865), None);
        assert_eq!(h265_level_for(36_864), Some(30));
        assert_eq!(h265_level_for(2_073_600), Some(120));
        assert_eq!(h265_level_for(35_651_584), Some(180));
        assert_eq!(h265_level_for(35_651_585), None);
    }

    #[test]
    fn h264_sets_pad_to_macroblocks() {
        let sets = EncodeSets::h264(320, 240, 100, 31).expect("covered");
        let super::EncodeSets::H264 { sps, pps } = &sets else {
            panic!("want h264 sets");
        };
        assert_eq!(sps.pic_width_in_mbs_minus1, 19);
        assert_eq!(sps.pic_height_in_map_units_minus1, 14);
        assert_eq!(sps.frame_crop_right_offset, 0);
        assert_eq!(sps.frame_crop_bottom_offset, 0);
        assert_eq!(pps.seq_parameter_set_id, 0);
        assert_eq!(pps.pic_parameter_set_id, 0);
    }

    #[test]
    fn h264_sets_crop_odd_extents() {
        let sets = EncodeSets::h264(322, 242, 100, 31).expect("covered");
        let super::EncodeSets::H264 { sps, .. } = &sets else {
            panic!("want h264 sets");
        };
        assert_eq!(sps.pic_width_in_mbs_minus1, 20);
        assert_eq!(sps.frame_crop_right_offset, 7);
        assert_eq!(sps.frame_crop_bottom_offset, 7);
    }

    #[test]
    fn h265_sets_pad_to_coding_blocks() {
        let sets = EncodeSets::h265(320, 240, 1, 93).expect("covered");
        let sps = sets.h265_sps().expect("h265 sps");
        assert_eq!(sps.pic_width_in_luma_samples, 320);
        assert_eq!(sps.pic_height_in_luma_samples, 240);
        assert_eq!(sps.conf_win_right_offset, 0);
        let vps = sets.h265_vps().expect("h265 vps");
        assert_eq!(vps.vps_video_parameter_set_id, 0);
        assert!(!vps.pProfileTierLevel.is_null());
        let pps = sets.h265_pps().expect("h265 pps");
        assert_eq!(pps.pps_pic_parameter_set_id, 0);
    }

    #[test]
    fn h265_sets_crop_odd_extents() {
        let sets = EncodeSets::h265(322, 242, 1, 93).expect("covered");
        let sps = sets.h265_sps().expect("h265 sps");
        assert_eq!(sps.pic_width_in_luma_samples, 328);
        assert_eq!(sps.pic_height_in_luma_samples, 248);
        assert_eq!(sps.conf_win_right_offset, 3);
        assert_eq!(sps.conf_win_bottom_offset, 3);
    }

    #[test]
    fn rate_control_scales_with_extent() {
        let extent = ash::vk::Extent2D {
            width: 320,
            height: 240,
        };
        let rate = RateControl::cbr_for(extent);
        let frame_bits = 320 * 240 * 3 / 2 * 8;
        assert_eq!(rate.average_bitrate, frame_bits * 30 / 10);
        assert_eq!(rate.max_bitrate, rate.average_bitrate);
    }
}
