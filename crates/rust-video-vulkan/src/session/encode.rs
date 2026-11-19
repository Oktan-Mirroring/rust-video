//! Encode session core: owned GPU resources shared by both codecs.
//!
//! GOP encode: IDR pictures code without references, P pictures predict
//! from the single live reference. The driver emits parameter sets from
//! the session sets below, and the feedback query reports the coded size.

use std::sync::Arc;

use ash::vk;
use ash::vk::native::{
    __BindgenBitfieldUnit, StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_ENABLED,
    StdVideoH264PictureParameterSet, StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR,
    StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P,
    StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
    StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH, StdVideoH264SequenceParameterSet,
    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I,
    StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P,
    StdVideoH264WeightedBipredIdc_STD_VIDEO_H264_WEIGHTED_BIPRED_IDC_DEFAULT,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420, StdVideoH265DecPicBufMgr,
    StdVideoH265PictureParameterSet, StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR,
    StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_P, StdVideoH265ProfileTierLevel,
    StdVideoH265SequenceParameterSet, StdVideoH265ShortTermRefPicSet,
    StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I,
    StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_P, StdVideoH265VideoParameterSet,
};
use rust_video_caps::transcode::ShareMechanism;

use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::parse::refs::{ReferenceId, SlotBook};
use crate::raw::{
    DeviceExport, EncodeFeedback, EncodeQueue, ExportPair, ExtDevice, GpuBuffer, GpuImage, GpuView,
    ImportPair, SessionParameters, VideoSession, IMPORT_MECHANISM,
};
use crate::session::decode::{
    follow_barrier, layout_barrier, narrow_u32, nv12_copy_regions, picture_resource, plane_copy,
    reference_slot,
};
use crate::session::geometry::{h264_level_to_std, h265_level_to_std};
use crate::session::params::{
    h264_encode_params_add, h264_encode_params_create, h265_encode_params_add,
    h265_encode_params_create, H264EncodeProfileChain, H265EncodeProfileChain,
    H264_ENCODE_STD_NAME, H265_ENCODE_STD_NAME,
};
use crate::session::slots::{SubmitSlots, SubmitStats, SUBMIT_SLOTS};

/// Bitstream staging size in bytes: the largest coded picture the session takes.
pub(crate) const BITSTREAM_BYTES: u64 = 16 << 20;

/// Rate-control frame rate numerator in frames per second.
const RC_FPS: u32 = 60;

/// Rate-control frame rate denominator.
const RC_FPS_DENOM: u32 = 1;

/// DPB slots of a P-capable encode session: one reference plus one reconstructed.
const ENCODE_DPB_SLOTS: u32 = 2;

/// Reference-list entry meaning no reference picture.
///
/// `STD_VIDEO_H264_NO_REFERENCE_PICTURE` and
/// `STD_VIDEO_H265_NO_REFERENCE_PICTURE` share the value; ash exports neither.
const NO_REF_PICTURE: u8 = 0xFF;

/// Encode codec behind one session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum EncodeCodec {
    /// H.264 encode.
    H264,
    /// H.265 encode.
    H265,
}

/// Coding stamp of one picture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PictureStamp {
    /// IDR id wrapping per IDR picture.
    idr_id: u16,
    /// Picture order count.
    poc: i32,
    /// Frame number, H.264 only.
    frame_num: u16,
    /// Picture codes as IDR.
    is_idr: bool,
}

impl PictureStamp {
    /// Stamp with `idr_id`, `poc`, `frame_num`, and `is_idr`.
    #[must_use]
    pub fn new(idr_id: u16, poc: i32, frame_num: u16, is_idr: bool) -> Self {
        Self {
            idr_id,
            poc,
            frame_num,
            is_idr,
        }
    }

    /// True when the picture codes as IDR.
    #[must_use]
    pub(crate) fn is_idr(&self) -> bool {
        self.is_idr
    }
}

/// Reconstructed picture behind one DPB slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EncodeMeta {
    /// Picture order count.
    poc: i32,
    /// Frame number, H.264 only.
    frame_num: u16,
    /// Picture coded as IDR.
    idr: bool,
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
        let average = (frame_bits * u64::from(RC_FPS) / 20).max(64_000);
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

/// Encode feedback plus CBR rate control behind `facts`.
///
/// # Errors
///
/// Returns `Err` when the adapter reports neither the coded-size
/// feedback flag nor the CBR control mode.
fn check_caps(facts: &crate::ladder::EncodeFacts) -> Result<(), VulkanError> {
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

/// Encode-rate-control bit of the coding control flags.
///
/// ash 0.38 omits the bit; value from `VkVideoCodingControlFlagBitsKHR`.
/// <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/VkVideoCodingControlFlagBitsKHR.html>
const ENCODE_RATE_CONTROL: vk::VideoCodingControlFlagsKHR =
    vk::VideoCodingControlFlagsKHR::from_raw(0x0000_0002);

/// CBR control over `layer`.
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

/// H.264 single-reference L0 list over `ref_slot`.
fn h264_ref_lists(ref_slot: u8) -> ash::vk::native::StdVideoEncodeH264ReferenceListsInfo {
    let mut l0 = [NO_REF_PICTURE; 32];
    l0[0] = ref_slot;
    ash::vk::native::StdVideoEncodeH264ReferenceListsInfo {
        flags: ash::vk::native::StdVideoEncodeH264ReferenceListsInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        },
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        RefPicList0: l0,
        RefPicList1: [NO_REF_PICTURE; 32],
        refList0ModOpCount: 0,
        refList1ModOpCount: 0,
        refPicMarkingOpCount: 0,
        reserved1: [0; 7],
        pRefList0ModOperations: std::ptr::null(),
        pRefList1ModOperations: std::ptr::null(),
        pRefPicMarkingOperations: std::ptr::null(),
    }
}

/// H.265 single-reference L0 list over `ref_slot`.
fn h265_ref_lists(ref_slot: u8) -> ash::vk::native::StdVideoEncodeH265ReferenceListsInfo {
    let mut l0 = [NO_REF_PICTURE; 15];
    l0[0] = ref_slot;
    ash::vk::native::StdVideoEncodeH265ReferenceListsInfo {
        flags: ash::vk::native::StdVideoEncodeH265ReferenceListsInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        },
        num_ref_idx_l0_active_minus1: 0,
        num_ref_idx_l1_active_minus1: 0,
        RefPicList0: l0,
        RefPicList1: [NO_REF_PICTURE; 15],
        list_entry_l0: [0; 15],
        list_entry_l1: [0; 15],
    }
}

/// H.265 short-term set holding one negative picture `delta_minus1` back.
fn h265_rps(delta_minus1: u16) -> StdVideoH265ShortTermRefPicSet {
    let mut s0 = [0_u16; 16];
    s0[0] = delta_minus1;
    StdVideoH265ShortTermRefPicSet {
        flags: ash::vk::native::StdVideoH265ShortTermRefPicSetFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 1]),
            __bindgen_padding_0: [0; 3],
        },
        delta_idx_minus1: 0,
        use_delta_flag: 0,
        abs_delta_rps_minus1: 0,
        used_by_curr_pic_flag: 0,
        used_by_curr_pic_s0_flag: 1,
        used_by_curr_pic_s1_flag: 0,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        num_negative_pics: 1,
        num_positive_pics: 0,
        delta_poc_s0_minus1: s0,
        delta_poc_s1_minus1: [0; 16],
    }
}

/// H.264 encode reference info of a `pic_type` picture at `frame_num`/`poc`.
fn h264_encode_ref_info(
    pic_type: ash::vk::native::StdVideoH264PictureType,
    frame_num: u32,
    poc: i32,
) -> ash::vk::native::StdVideoEncodeH264ReferenceInfo {
    ash::vk::native::StdVideoEncodeH264ReferenceInfo {
        flags: ash::vk::native::StdVideoEncodeH264ReferenceInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        },
        primary_pic_type: pic_type,
        FrameNum: frame_num,
        PicOrderCnt: poc,
        long_term_pic_num: 0,
        long_term_frame_idx: 0,
        temporal_id: 0,
    }
}

/// H.264 encode picture info, IDR shape when `idr` holds.
fn h264_encode_picture(
    idr: bool,
    idr_pic_id: u16,
    pic_type: ash::vk::native::StdVideoH264PictureType,
    frame_num: u32,
    poc: i32,
    ref_lists: Option<&ash::vk::native::StdVideoEncodeH264ReferenceListsInfo>,
) -> ash::vk::native::StdVideoEncodeH264PictureInfo {
    let mut flags = ash::vk::native::StdVideoEncodeH264PictureInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_IdrPicFlag(u32::from(idr));
    flags.set_is_reference(1);
    ash::vk::native::StdVideoEncodeH264PictureInfo {
        flags,
        seq_parameter_set_id: 0,
        pic_parameter_set_id: 0,
        idr_pic_id,
        primary_pic_type: pic_type,
        frame_num,
        PicOrderCnt: poc,
        temporal_id: 0,
        reserved1: [0; 3],
        pRefLists: ref_lists.map_or(std::ptr::null(), std::ptr::from_ref),
    }
}

/// H.264 encode slice header, P type when `p` holds.
fn h264_encode_slice(p: bool) -> ash::vk::native::StdVideoEncodeH264SliceHeader {
    ash::vk::native::StdVideoEncodeH264SliceHeader {
        flags: ash::vk::native::StdVideoEncodeH264SliceHeaderFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        },
        first_mb_in_slice: 0,
        slice_type: if p {
            StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P
        } else {
            StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I
        },
        slice_alpha_c0_offset_div2: 0,
        slice_beta_offset_div2: 0,
        slice_qp_delta: 0,
        reserved1: 0,
        cabac_init_idc: StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0,
        disable_deblocking_filter_idc:
            StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_ENABLED,
        pWeightTable: std::ptr::null(),
    }
}

/// H.265 encode reference info of a `pic_type` picture at `poc`.
fn h265_encode_ref_info(
    pic_type: ash::vk::native::StdVideoH265PictureType,
    poc: i32,
) -> ash::vk::native::StdVideoEncodeH265ReferenceInfo {
    ash::vk::native::StdVideoEncodeH265ReferenceInfo {
        flags: ash::vk::native::StdVideoEncodeH265ReferenceInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        },
        pic_type,
        PicOrderCntVal: poc,
        TemporalId: 0,
    }
}

/// H.265 encode picture info, IDR shape when `idr` holds.
fn h265_encode_picture(
    idr: bool,
    pic_type: ash::vk::native::StdVideoH265PictureType,
    poc: i32,
    ref_lists: Option<&ash::vk::native::StdVideoEncodeH265ReferenceListsInfo>,
    rps: Option<&StdVideoH265ShortTermRefPicSet>,
) -> ash::vk::native::StdVideoEncodeH265PictureInfo {
    let mut flags = ash::vk::native::StdVideoEncodeH265PictureInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_is_reference(1);
    flags.set_IrapPicFlag(u32::from(idr));
    flags.set_short_term_ref_pic_set_sps_flag(u32::from(idr));
    flags.set_pic_output_flag(1);
    ash::vk::native::StdVideoEncodeH265PictureInfo {
        flags,
        pic_type,
        sps_video_parameter_set_id: 0,
        pps_seq_parameter_set_id: 0,
        pps_pic_parameter_set_id: 0,
        short_term_ref_pic_set_idx: 0,
        PicOrderCntVal: poc,
        TemporalId: 0,
        reserved1: [0; 7],
        pRefLists: ref_lists.map_or(std::ptr::null(), std::ptr::from_ref),
        pShortTermRefPicSet: rps.map_or(std::ptr::null(), std::ptr::from_ref),
        pLongTermRefPics: std::ptr::null(),
    }
}

/// H.265 encode slice header, P type when `p` holds.
fn h265_encode_slice(p: bool) -> ash::vk::native::StdVideoEncodeH265SliceSegmentHeader {
    let mut flags = ash::vk::native::StdVideoEncodeH265SliceSegmentHeaderFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_first_slice_segment_in_pic_flag(1);
    ash::vk::native::StdVideoEncodeH265SliceSegmentHeader {
        flags,
        slice_type: if p {
            StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_P
        } else {
            StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I
        },
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
    }
}

/// Checked narrowing of an index to `i32`.
fn narrow_i32(value: usize, syntax: &'static str) -> Result<i32, VulkanError> {
    i32::try_from(value).map_err(|_| crate::error::ParseError::OutOfRange { syntax }.into())
}

/// Narrowed reference slot of `plan`, if the picture predicts.
fn plan_ref_slot(plan: &EncodePlan) -> Result<Option<u8>, VulkanError> {
    plan.ref_slot
        .map(|slot| {
            u8::try_from(slot)
                .map_err(|_| crate::error::ParseError::OutOfRange { syntax: "ref slot" })
        })
        .transpose()
        .map_err(VulkanError::from)
}

/// Encode info over `src` into `buffer`, reconstructing into `slot`.
fn encode_info<'a>(
    buffer: vk::Buffer,
    src: vk::VideoPictureResourceInfoKHR<'a>,
    slot: &'a vk::VideoReferenceSlotInfoKHR<'a>,
    refs: &'a [vk::VideoReferenceSlotInfoKHR<'a>],
) -> Result<vk::VideoEncodeInfoKHR<'a>, VulkanError> {
    Ok(vk::VideoEncodeInfoKHR {
        dst_buffer: buffer,
        dst_buffer_offset: 0,
        dst_buffer_range: BITSTREAM_BYTES,
        src_picture_resource: src,
        p_setup_reference_slot: std::ptr::from_ref(slot),
        reference_slot_count: narrow_u32(refs.len(), "encode refs")?,
        p_reference_slots: if refs.is_empty() {
            std::ptr::null()
        } else {
            refs.as_ptr()
        },
        ..Default::default()
    })
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

/// Images, buffers, and feedback queries of one encode session.
struct SessionResources {
    /// Source image view; drops before `input` below.
    input_view: GpuView,
    /// Source image for uploaded pictures.
    input: GpuImage,
    /// One view per DPB layer; drops before `dpb` below.
    dpb_views: Vec<GpuView>,
    /// DPB array image, one layer per slot.
    dpb: GpuImage,
    /// NV12 upload staging buffer.
    staging: GpuBuffer,
    /// Coded bitstream buffer.
    bitstream: GpuBuffer,
    /// Coded-size feedback pool.
    feedback: EncodeFeedback,
    /// Encode queue functions.
    encode: EncodeQueue,
}

/// Owned encode session over a negotiated device.
///
/// Field order is the drop order: the submit ring idles the device
/// before dependents free, parameters free before the session they
/// belong to, and GPU objects free before the device clone.
pub(crate) struct EncodeSession {
    /// Submit-slot ring; drops first, idling the device.
    submit: SubmitSlots,
    /// Logical device.
    device: ash::Device,
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
    /// Memory properties behind plane import.
    props: vk::PhysicalDeviceMemoryProperties,
    /// Platform import functions, absent without the extensions.
    ext: Option<ExtDevice>,
    /// Import of the last encode, reused while the pair holds.
    import: Option<(Arc<ExportPair>, ImportPair)>,
    /// Share mechanism behind texture import, absent when import is not live.
    import_share: Option<ShareMechanism>,
    /// Source image view; drops before `input` below.
    input_view: GpuView,
    /// Source image for uploaded pictures.
    input: GpuImage,
    /// Tracked source layout.
    input_layout: vk::ImageLayout,
    /// One view per DPB layer; drops before `dpb` below.
    dpb_views: Vec<GpuView>,
    /// DPB array image, one layer per slot.
    dpb: GpuImage,
    /// Tracked layout per DPB layer.
    dpb_layouts: Vec<vk::ImageLayout>,
    /// Reference-id to slot map.
    slots: SlotBook,
    /// Reconstructed picture per slot.
    meta: Vec<Option<EncodeMeta>>,
    /// Next reference identity.
    next_id: ReferenceId,
    /// Two DPB slots back P pictures; one slot stays IDR-only.
    p_capable: bool,
    /// NV12 upload staging buffer.
    staging: GpuBuffer,
    /// Coded bitstream buffer.
    bitstream: GpuBuffer,
    /// Coded-size feedback pool.
    feedback: EncodeFeedback,
    /// CBR rate control.
    rate: RateControl,
    /// Rate control configured on the session.
    rc_configured: bool,
    /// Frames between IDR pictures.
    idr_period: u32,
    /// Driver-encoded parameter sets prepended to keyframe packets.
    headers: Vec<u8>,
}

/// Claimed slots and reference of one picture.
struct EncodePlan {
    /// Slot receiving the reconstructed picture.
    setup_slot: usize,
    /// Slot holding the live reference, if the picture predicts.
    ref_slot: Option<usize>,
    /// Live reference behind `ref_slot`.
    ref_meta: Option<EncodeMeta>,
    /// H.265 POC gap minus 1 behind the explicit short-term set.
    rps_delta_minus1: u16,
    /// Picture codes as IDR.
    coded_idr: bool,
    /// Identity leaving the DPB with this picture.
    old_id: Option<ReferenceId>,
}

impl EncodeSession {
    /// Session over `device` for `codec` at `width`x`height`.
    ///
    /// `idr_period` counts frames between IDR pictures; 0 clamps to 1.
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
        idr_period: u32,
    ) -> Result<Self, VulkanError> {
        let crate::ladder::OpCaps::Encode(facts) = device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        check_extent(&facts, width, height)?;
        check_caps(&facts)?;
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
        let p_capable = facts.dpb_slots >= ENCODE_DPB_SLOTS && facts.active_refs >= 1;
        let dpb_count = if p_capable { ENCODE_DPB_SLOTS } else { 1 };
        let idr_period = if p_capable { idr_period.max(1) } else { 1 };
        let resources =
            Self::open_resources(device, &properties, format, extent, &profile, dpb_count)?;
        let desc = crate::raw::VideoSessionDesc {
            profile: &profile,
            queue_family: device.queue_family(),
            format,
            extent,
            dpb_slots: dpb_count,
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
        let submit = SubmitSlots::open(
            raw,
            device.queue_family(),
            device.queue(),
            device.queue_lock().clone(),
        )?;
        Ok(Self {
            submit,
            device: raw.clone(),
            encode: resources.encode,
            params,
            video: session,
            codec,
            extent,
            format,
            props: properties,
            ext: device.ext(),
            import: None,
            import_share: if device.surface_import() {
                device.share_mechanism()
            } else {
                None
            },
            input_view: resources.input_view,
            input: resources.input,
            input_layout: vk::ImageLayout::UNDEFINED,
            dpb_views: resources.dpb_views,
            dpb: resources.dpb,
            dpb_layouts: vec![vk::ImageLayout::UNDEFINED; dpb_count as usize],
            slots: SlotBook::with_slots(dpb_count as usize),
            meta: vec![None; dpb_count as usize],
            next_id: ReferenceId::default(),
            p_capable,
            staging: resources.staging,
            bitstream: resources.bitstream,
            feedback: resources.feedback,
            rate: RateControl::cbr_for(extent),
            rc_configured: false,
            idr_period,
            headers,
        })
    }

    /// Images, buffers, and feedback queries for `profile` at `extent`.
    fn open_resources(
        device: &VulkanDevice,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        profile: &vk::VideoProfileInfoKHR<'_>,
        dpb_layers: u32,
    ) -> Result<SessionResources, VulkanError> {
        let raw = device.raw().device();
        let profiles = std::slice::from_ref(profile);
        let input = Self::create_image(
            raw,
            properties,
            format,
            extent,
            1,
            vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
            profiles,
        )?;
        let input_view = input.layer_view(0)?;
        let dpb = Self::create_image(
            raw,
            properties,
            format,
            extent,
            dpb_layers,
            vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
            profiles,
        )?;
        let mut dpb_views = Vec::with_capacity(dpb_layers as usize);
        for layer in 0..dpb_layers {
            dpb_views.push(dpb.layer_view(layer)?);
        }
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let staging = GpuBuffer::create(
            raw,
            properties,
            crate::session::decode::nv12_bytes(extent),
            vk::BufferUsageFlags::TRANSFER_SRC,
            host,
            &[],
        )?;
        let bitstream = GpuBuffer::create_readback(
            raw,
            properties,
            BITSTREAM_BYTES,
            vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR,
            host,
            profiles,
        )?;
        let slots =
            u32::try_from(SUBMIT_SLOTS).map_err(|_| crate::error::ParseError::OutOfRange {
                syntax: "submit slots",
            })?;
        let feedback = EncodeFeedback::create(
            raw,
            slots,
            vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
            profile,
        )?;
        let encode = EncodeQueue::new(device.backend().instance(), raw);
        Ok(SessionResources {
            input_view,
            input,
            dpb_views,
            dpb,
            staging,
            bitstream,
            feedback,
            encode,
        })
    }

    /// Image of `format` at `extent` with `layers` and `usage` over `profiles`.
    fn create_image(
        raw: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        layers: u32,
        usage: vk::ImageUsageFlags,
        profiles: &[vk::VideoProfileInfoKHR<'_>],
    ) -> Result<GpuImage, VulkanError> {
        GpuImage::create(
            raw,
            properties,
            &crate::raw::GpuImageDesc {
                format,
                extent,
                layers,
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
        template: &mut crate::session::params::ParamsTemplate<'_>,
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
            | crate::session::params::ParamsTemplate::H265(_)
            | crate::session::params::ParamsTemplate::Av1(_) => {
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

    /// Submit-core gauges snapshot.
    #[must_use]
    pub(crate) fn submit_stats(&self) -> SubmitStats {
        self.submit.stats()
    }

    /// Fail the next submit once, exercising the reset path.
    pub(crate) fn inject_fault(&mut self) {
        self.submit.inject_fault();
    }

    /// True when two DPB slots back P pictures.
    pub(crate) fn supports_p_frames(&self) -> bool {
        self.p_capable
    }

    /// Share mechanism behind texture import, absent when import is not live.
    pub(crate) fn import_share(&self) -> Option<ShareMechanism> {
        self.import_share
    }

    /// Forget layouts and references after a submit-core reset.
    fn reset_state(&mut self) {
        self.input_layout = vk::ImageLayout::UNDEFINED;
        self.dpb_layouts.fill(vk::ImageLayout::UNDEFINED);
        self.slots.release_all();
        self.meta.fill(None);
        self.rc_configured = false;
    }

    /// Claimed setup slot and live reference for `stamp`.
    ///
    /// An IDR flushes the DPB; a P without a live reference codes as
    /// IDR instead, so a reset never emits a dangling prediction.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no slot is free or the POC gap escapes `u16`.
    fn plan_picture(&mut self, stamp: PictureStamp) -> Result<EncodePlan, VulkanError> {
        if stamp.is_idr {
            self.slots.release_all();
            self.meta.fill(None);
        }
        let live = if stamp.is_idr || !self.p_capable {
            None
        } else {
            self.live_reference(stamp.poc)
        };
        let coded_idr = stamp.is_idr || live.is_none();
        let (ref_slot, ref_meta, rps_delta_minus1, old_id) = match live {
            Some((slot, meta, id)) => {
                let delta = u16::try_from(stamp.poc - meta.poc - 1).map_err(|_| {
                    crate::error::ParseError::OutOfRange {
                        syntax: "poc delta",
                    }
                })?;
                (Some(slot), Some(meta), delta, Some(id))
            }
            None => (None, None, 0, None),
        };
        let setup_slot = self
            .slots
            .claim(self.next_id.next())
            .map_err(|_| VulkanError::NoFreeSlot)?;
        Ok(EncodePlan {
            setup_slot,
            ref_slot,
            ref_meta,
            rps_delta_minus1,
            coded_idr,
            old_id,
        })
    }

    /// Slot, picture, and identity of the live reference behind `poc`.
    fn live_reference(&self, poc: i32) -> Option<(usize, EncodeMeta, ReferenceId)> {
        let (slot, meta) = self
            .meta
            .iter()
            .enumerate()
            .filter_map(|(slot, meta)| meta.map(|meta| (slot, meta)))
            .max_by_key(|(_, meta)| meta.poc)?;
        if meta.poc >= poc {
            return None;
        }
        Some((slot, meta, self.slots.identity_of(slot)?))
    }

    /// Retire the replaced reference and store the new reconstructed picture.
    fn commit_plan(&mut self, plan: &EncodePlan, stamp: PictureStamp) {
        if let Some(old) = plan.old_id {
            if let Some(slot) = self.slots.slot_of(old) {
                self.meta[slot] = None;
            }
            self.slots.release(old);
        }
        self.meta[plan.setup_slot] = Some(EncodeMeta {
            poc: stamp.poc,
            frame_num: stamp.frame_num,
            idr: plan.coded_idr,
        });
    }

    /// Encode tight NV12 `picture` stamped `stamp` into `out`.
    ///
    /// Returns whether the picture coded as a keyframe.
    ///
    /// # Errors
    ///
    /// Returns `Err` when staging, recording, submission, or the fence
    /// wait fails.
    pub(crate) fn encode_into(
        &mut self,
        picture: &[u8],
        stamp: PictureStamp,
        out: &mut Vec<u8>,
    ) -> Result<bool, VulkanError> {
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
        let index = self.submit.acquire().inspect_err(|_| self.reset_state())?;
        if let Err(error) = self.staging.memory().fill(picture) {
            self.submit.release(index);
            return Err(error);
        }
        let plan = match self.plan_picture(stamp) {
            Ok(plan) => plan,
            Err(error) => {
                self.submit.release(index);
                return Err(error);
            }
        };
        if let Err(error) = self.record(index, stamp, &plan) {
            self.submit.release(index);
            self.reset_state();
            return Err(error);
        }
        if let Err(error) = self.submit.submit(index) {
            self.reset_state();
            return Err(error);
        }
        self.commit_plan(&plan, stamp);
        self.collect(index, out)?;
        Ok(plan.coded_idr)
    }

    /// Encode the device `export` stamped `stamp` into `out`.
    ///
    /// Returns whether the picture coded as a keyframe.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the export misses the session format or extent,
    /// the import fails, or recording, submission, or the fence wait fails.
    pub(crate) fn encode_import(
        &mut self,
        export: &DeviceExport,
        stamp: PictureStamp,
        out: &mut Vec<u8>,
    ) -> Result<bool, VulkanError> {
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
                    what: IMPORT_MECHANISM.to_owned(),
                });
            };
            let import = ImportPair::import(&self.device, &ext, &self.props, export)?;
            self.import = Some((export.pair().clone(), import));
        }
        let Some((_, import)) = self.import.as_ref() else {
            return Err(VulkanError::Unsupported {
                what: IMPORT_MECHANISM.to_owned(),
            });
        };
        let (y, uv) = (import.y_image(), import.uv_image());
        let index = self.submit.acquire().inspect_err(|_| self.reset_state())?;
        let plan = match self.plan_picture(stamp) {
            Ok(plan) => plan,
            Err(error) => {
                self.submit.release(index);
                return Err(error);
            }
        };
        if let Err(error) = self.record_import(index, stamp, &plan, y, uv) {
            self.submit.release(index);
            self.reset_state();
            return Err(error);
        }
        if let Err(error) = self.submit.submit(index) {
            self.reset_state();
            return Err(error);
        }
        self.commit_plan(&plan, stamp);
        self.collect(index, out)?;
        Ok(plan.coded_idr)
    }

    /// Record upload, encode, and feedback commands.
    fn record(
        &mut self,
        index: usize,
        stamp: PictureStamp,
        plan: &EncodePlan,
    ) -> Result<(), VulkanError> {
        let cmd = self.submit.cmd(index);
        self.submit.pool().begin_buffer(cmd)?;
        self.record_upload_head(cmd);
        self.record_tail(index, cmd, stamp, plan)?;
        self.submit.pool().end_buffer(cmd)?;
        Ok(())
    }

    /// Record import, encode, and feedback commands.
    fn record_import(
        &mut self,
        index: usize,
        stamp: PictureStamp,
        plan: &EncodePlan,
        y: vk::Image,
        uv: vk::Image,
    ) -> Result<(), VulkanError> {
        let cmd = self.submit.cmd(index);
        self.submit.pool().begin_buffer(cmd)?;
        self.record_import_head(cmd, y, uv);
        self.record_tail(index, cmd, stamp, plan)?;
        self.submit.pool().end_buffer(cmd)?;
        Ok(())
    }

    /// Record the staging upload into the source image.
    fn record_upload_head(&mut self, cmd: vk::CommandBuffer) {
        let recorder = self.submit.pool().recorder(cmd);
        if self.input_layout != vk::ImageLayout::TRANSFER_DST_OPTIMAL {
            let barrier = layout_barrier(
                self.input.image(),
                self.input_layout,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[barrier]));
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
    fn record_import_head(&mut self, cmd: vk::CommandBuffer, y: vk::Image, uv: vk::Image) {
        let recorder = self.submit.pool().recorder(cmd);
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
        if self.input_layout != vk::ImageLayout::TRANSFER_DST_OPTIMAL {
            let barrier = layout_barrier(
                self.input.image(),
                self.input_layout,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                0,
                1,
            );
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&[barrier]));
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
    fn record_tail(
        &mut self,
        index: usize,
        cmd: vk::CommandBuffer,
        stamp: PictureStamp,
        plan: &EncodePlan,
    ) -> Result<(), VulkanError> {
        let query = u32::try_from(index).map_err(|_| crate::error::ParseError::OutOfRange {
            syntax: "submit slot",
        })?;
        let recorder = self.submit.pool().recorder(cmd);
        let mut head = Vec::new();
        for slot in [Some(plan.setup_slot), plan.ref_slot].into_iter().flatten() {
            let target = vk::ImageLayout::VIDEO_ENCODE_DPB_KHR;
            let layer = u32::try_from(slot).map_err(|_| crate::error::ParseError::OutOfRange {
                syntax: "setup slot",
            })?;
            if self.dpb_layouts[slot] == target {
                head.push(follow_barrier(
                    self.dpb.image(),
                    target,
                    vk::ImageAspectFlags::COLOR,
                    layer,
                    vk::PipelineStageFlags2::VIDEO_ENCODE_KHR,
                    vk::AccessFlags2::VIDEO_ENCODE_WRITE_KHR
                        | vk::AccessFlags2::VIDEO_ENCODE_READ_KHR,
                ));
            } else {
                head.push(layout_barrier(
                    self.dpb.image(),
                    self.dpb_layouts[slot],
                    target,
                    vk::ImageAspectFlags::COLOR,
                    layer,
                    1,
                ));
                self.dpb_layouts[slot] = target;
            }
        }
        if !head.is_empty() {
            recorder.barrier(&vk::DependencyInfo::default().image_memory_barriers(&head));
        }
        recorder.reset_queries(self.feedback.pool(), query, 1);
        let mut h264_layer = vk::VideoEncodeH264RateControlLayerInfoKHR::default();
        let mut h265_layer = vk::VideoEncodeH265RateControlLayerInfoKHR::default();
        let mut layer = rc_layer(&self.rate);
        match self.codec {
            EncodeCodec::H264 => {
                layer.p_next = std::ptr::from_mut(&mut h264_layer).cast();
            }
            EncodeCodec::H265 => {
                layer.p_next = std::ptr::from_mut(&mut h265_layer).cast();
            }
        }
        let mut control = rc_control(&layer);
        let mut h264_rc = vk::VideoEncodeH264RateControlInfoKHR {
            gop_frame_count: self.idr_period,
            idr_period: self.idr_period,
            consecutive_b_frame_count: 0,
            temporal_layer_count: 1,
            ..Default::default()
        };
        let mut h265_rc = vk::VideoEncodeH265RateControlInfoKHR {
            gop_frame_count: self.idr_period,
            idr_period: self.idr_period,
            consecutive_b_frame_count: 0,
            sub_layer_count: 1,
            ..Default::default()
        };
        match self.codec {
            EncodeCodec::H264 => {
                control.p_next = std::ptr::from_mut(&mut h264_rc).cast();
            }
            EncodeCodec::H265 => {
                control.p_next = std::ptr::from_mut(&mut h265_rc).cast();
            }
        }
        let first = !self.rc_configured;
        match self.codec {
            EncodeCodec::H264 => self.record_h264(cmd, query, stamp, plan, &mut control, first)?,
            EncodeCodec::H265 => self.record_h265(cmd, query, stamp, plan, &mut control, first)?,
        }
        self.rc_configured = true;
        Ok(())
    }

    /// Begin one coding scope, configuring rate control on the first.
    ///
    /// The first begin carries no rate control; the control command
    /// configures it. Later begins declare it to match session state.
    /// VUID-vkCmdBeginVideoCodingKHR-pBeginInfo-08254.
    fn begin_scope(
        &self,
        cmd: vk::CommandBuffer,
        begin: vk::VideoBeginCodingInfoKHR<'_>,
        control: &mut vk::VideoEncodeRateControlInfoKHR<'_>,
        first: bool,
    ) {
        if first {
            self.video.begin_coding(cmd, &begin);
            let control_info = vk::VideoCodingControlInfoKHR::default()
                .flags(vk::VideoCodingControlFlagsKHR::RESET | ENCODE_RATE_CONTROL)
                .push_next(control);
            self.video.control_coding(cmd, &control_info);
        } else {
            let begin = begin.push_next(control);
            self.video.begin_coding(cmd, &begin);
        }
    }

    /// Record one H.264 IDR or P encode.
    fn record_h264(
        &self,
        cmd: vk::CommandBuffer,
        query: u32,
        stamp: PictureStamp,
        plan: &EncodePlan,
        control: &mut vk::VideoEncodeRateControlInfoKHR<'_>,
        first: bool,
    ) -> Result<(), VulkanError> {
        let recorder = self.submit.pool().recorder(cmd);
        let idr = plan.coded_idr;
        let pic_type = if idr {
            StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR
        } else {
            StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P
        };
        let frame_num = if idr { 0 } else { u32::from(stamp.frame_num) };
        let recon = h264_encode_ref_info(pic_type, frame_num, stamp.poc);
        let recon_dpb = vk::VideoEncodeH264DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&recon),
            ..Default::default()
        };
        // Per-layer views carry the layer; the resource addresses the view's only layer.
        let dst = picture_resource(self.dpb_views[plan.setup_slot].view(), self.extent, 0);
        let setup_index = narrow_i32(plan.setup_slot, "setup slot")?;
        let mut slot = reference_slot(setup_index, &dst);
        slot.p_next = std::ptr::from_ref(&recon_dpb).cast();
        let bind = reference_slot(-1, &dst);
        let ref_meta = plan.ref_meta.unwrap_or(EncodeMeta {
            poc: 0,
            frame_num: 0,
            idr: true,
        });
        let ref_pic_type = if ref_meta.idr {
            StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR
        } else {
            StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P
        };
        let ref_std =
            h264_encode_ref_info(ref_pic_type, u32::from(ref_meta.frame_num), ref_meta.poc);
        let ref_dpb = vk::VideoEncodeH264DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&ref_std),
            ..Default::default()
        };
        let ref_slot = plan.ref_slot.unwrap_or(0);
        let ref_resource = picture_resource(self.dpb_views[ref_slot].view(), self.extent, 0);
        let ref_index = narrow_i32(ref_slot, "ref slot")?;
        let mut reference = reference_slot(ref_index, &ref_resource);
        reference.p_next = std::ptr::from_ref(&ref_dpb).cast();
        let mut begin_slots = [bind, bind];
        let (begin_list, ref_list): (&[_], &[_]) = if plan.ref_slot.is_some() {
            begin_slots[0] = reference;
            (&begin_slots, &begin_slots[..1])
        } else {
            (&begin_slots[..1], &[])
        };
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: narrow_u32(begin_list.len(), "begin slots")?,
            p_reference_slots: begin_list.as_ptr(),
            ..Default::default()
        };
        self.begin_scope(cmd, begin, control, first);
        recorder.begin_query(self.feedback.pool(), query);
        let ref_lists = plan_ref_slot(plan)?.map(h264_ref_lists);
        let std_picture = h264_encode_picture(
            idr,
            stamp.idr_id,
            pic_type,
            frame_num,
            stamp.poc,
            ref_lists.as_ref(),
        );
        let slice_header = h264_encode_slice(!idr);
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
        let info = encode_info(self.bitstream.buffer(), src, &slot, ref_list)?;
        let info = info.push_next(&mut codec);
        self.encode.encode_video(cmd, &info);
        recorder.end_query(self.feedback.pool(), query);
        self.video
            .end_coding(cmd, &vk::VideoEndCodingInfoKHR::default());
        Ok(())
    }

    /// Record one H.265 IDR or P encode.
    fn record_h265(
        &self,
        cmd: vk::CommandBuffer,
        query: u32,
        stamp: PictureStamp,
        plan: &EncodePlan,
        control: &mut vk::VideoEncodeRateControlInfoKHR<'_>,
        first: bool,
    ) -> Result<(), VulkanError> {
        let recorder = self.submit.pool().recorder(cmd);
        let idr = plan.coded_idr;
        let pic_type = if idr {
            StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR
        } else {
            StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_P
        };
        let recon = h265_encode_ref_info(pic_type, stamp.poc);
        let recon_dpb = vk::VideoEncodeH265DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&recon),
            ..Default::default()
        };
        // Per-layer views carry the layer; the resource addresses the view's only layer.
        let dst = picture_resource(self.dpb_views[plan.setup_slot].view(), self.extent, 0);
        let setup_index = narrow_i32(plan.setup_slot, "setup slot")?;
        let mut slot = reference_slot(setup_index, &dst);
        slot.p_next = std::ptr::from_ref(&recon_dpb).cast();
        let bind = reference_slot(-1, &dst);
        let ref_meta = plan.ref_meta.unwrap_or(EncodeMeta {
            poc: 0,
            frame_num: 0,
            idr: true,
        });
        let ref_pic_type = if ref_meta.idr {
            StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR
        } else {
            StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_P
        };
        let ref_std = h265_encode_ref_info(ref_pic_type, ref_meta.poc);
        let ref_dpb = vk::VideoEncodeH265DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&ref_std),
            ..Default::default()
        };
        let ref_slot = plan.ref_slot.unwrap_or(0);
        let ref_resource = picture_resource(self.dpb_views[ref_slot].view(), self.extent, 0);
        let ref_index = narrow_i32(ref_slot, "ref slot")?;
        let mut reference = reference_slot(ref_index, &ref_resource);
        reference.p_next = std::ptr::from_ref(&ref_dpb).cast();
        let mut begin_slots = [bind, bind];
        let (begin_list, ref_list): (&[_], &[_]) = if plan.ref_slot.is_some() {
            begin_slots[0] = reference;
            (&begin_slots, &begin_slots[..1])
        } else {
            (&begin_slots[..1], &[])
        };
        let begin = vk::VideoBeginCodingInfoKHR {
            video_session: self.video.session(),
            video_session_parameters: self.params.params(),
            reference_slot_count: narrow_u32(begin_list.len(), "begin slots")?,
            p_reference_slots: begin_list.as_ptr(),
            ..Default::default()
        };
        self.begin_scope(cmd, begin, control, first);
        recorder.begin_query(self.feedback.pool(), query);
        let ref_lists = plan_ref_slot(plan)?.map(h265_ref_lists);
        let rps = h265_rps(plan.rps_delta_minus1);
        let rps_ref = if idr { None } else { Some(&rps) };
        let std_picture =
            h265_encode_picture(idr, pic_type, stamp.poc, ref_lists.as_ref(), rps_ref);
        let slice_header = h265_encode_slice(!idr);
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
        let info = encode_info(self.bitstream.buffer(), src, &slot, ref_list)?;
        let info = info.push_next(&mut codec);
        self.encode.encode_video(cmd, &info);
        recorder.end_query(self.feedback.pool(), query);
        self.video
            .end_coding(cmd, &vk::VideoEndCodingInfoKHR::default());
        Ok(())
    }

    /// Wait `index` out, then map the coded prefix into `out`.
    fn collect(&mut self, index: usize, out: &mut Vec<u8>) -> Result<(), VulkanError> {
        self.submit
            .poll(index)
            .inspect_err(|_| self.reset_state())?;
        let query = u32::try_from(index).map_err(|_| crate::error::ParseError::OutOfRange {
            syntax: "submit slot",
        })?;
        let mut written = [0_u64; 1];
        self.feedback.results(query, &mut written)?;
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
    fn ref_lists_point_l0_at_slot() {
        let h264 = super::h264_ref_lists(1);
        assert_eq!(h264.num_ref_idx_l0_active_minus1, 0);
        assert_eq!(h264.num_ref_idx_l1_active_minus1, 0);
        assert_eq!(h264.RefPicList0[0], 1);
        assert!(h264.RefPicList0[1..].iter().all(|entry| *entry == 0xFF));
        assert!(h264.RefPicList1.iter().all(|entry| *entry == 0xFF));
        assert_eq!(h264.refPicMarkingOpCount, 0);
        let h265 = super::h265_ref_lists(1);
        assert_eq!(h265.num_ref_idx_l0_active_minus1, 0);
        assert_eq!(h265.RefPicList0[0], 1);
        assert!(h265.RefPicList0[1..].iter().all(|entry| *entry == 0xFF));
        assert!(h265.RefPicList1.iter().all(|entry| *entry == 0xFF));
    }

    #[test]
    fn rps_holds_one_negative_picture() {
        let rps = super::h265_rps(0);
        assert_eq!(rps.num_negative_pics, 1);
        assert_eq!(rps.num_positive_pics, 0);
        assert_eq!(rps.delta_poc_s0_minus1[0], 0);
        assert_eq!(rps.used_by_curr_pic_s0_flag, 1);
        assert_eq!(rps.use_delta_flag, 0);
    }

    #[test]
    fn rate_control_scales_with_extent() {
        let extent = ash::vk::Extent2D {
            width: 320,
            height: 240,
        };
        let rate = RateControl::cbr_for(extent);
        let frame_bits = 320 * 240 * 3 / 2 * 8;
        assert_eq!(rate.average_bitrate, frame_bits * 60 / 20);
        assert_eq!(rate.max_bitrate, rate.average_bitrate);
    }
}
