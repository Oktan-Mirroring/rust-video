//! Video profile chains and session-parameter assembly.
//!
//! Pure host-side construction: profile chains own their codec tail on the
//! heap, so the base profile's `p_next` stays valid however the chain moves.

use ash::vk;
use ash::vk::native::{
    StdVideoH264PictureParameterSet, StdVideoH264SequenceParameterSet,
    StdVideoH265PictureParameterSet, StdVideoH265SequenceParameterSet,
    StdVideoH265VideoParameterSet,
};

/// H.264 sequence-set id space.
pub(crate) const H264_MAX_SPS: u32 = 32;
/// H.264 picture-set id space.
pub(crate) const H264_MAX_PPS: u32 = 256;
/// H.265 video-set id space.
pub(crate) const H265_MAX_VPS: u32 = 16;
/// H.265 sequence-set id space.
pub(crate) const H265_MAX_SPS: u32 = 16;
/// H.265 picture-set id space.
pub(crate) const H265_MAX_PPS: u32 = 64;

/// H.264 decode Std header extension name.
pub(crate) const H264_DECODE_STD_NAME: &[u8] = b"VK_STD_vulkan_video_codec_h264_decode";
/// H.265 decode Std header extension name.
pub(crate) const H265_DECODE_STD_NAME: &[u8] = b"VK_STD_vulkan_video_codec_h265_decode";

/// Session-parameters template per codec.
pub(crate) enum ParamsTemplate {
    /// H.264 template.
    H264(vk::VideoDecodeH264SessionParametersCreateInfoKHR<'static>),
    /// H.265 template.
    H265(vk::VideoDecodeH265SessionParametersCreateInfoKHR<'static>),
}

/// Owned H.264 decode profile chain.
pub(crate) struct H264ProfileChain {
    /// Codec tail, box-stable; every `base` borrows it.
    codec: Box<vk::VideoDecodeH264ProfileInfoKHR<'static>>,
}

impl H264ProfileChain {
    /// 8-bit 4:2:0 progressive H.264 decode profile for `std_profile_idc`.
    pub(crate) fn new(std_profile_idc: u32) -> Self {
        Self {
            codec: Box::new(vk::VideoDecodeH264ProfileInfoKHR {
                std_profile_idc,
                picture_layout: vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE,
                ..Default::default()
            }),
        }
    }

    /// Base profile for session creation and format queries.
    pub(crate) fn base(&self) -> vk::VideoProfileInfoKHR<'_> {
        vk::VideoProfileInfoKHR {
            video_codec_operation: vk::VideoCodecOperationFlagsKHR::DECODE_H264,
            chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::TYPE_420,
            luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::TYPE_8,
            chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::TYPE_8,
            p_next: std::ptr::from_ref(self.codec.as_ref()).cast(),
            ..Default::default()
        }
    }
}

/// Owned H.265 decode profile chain.
pub(crate) struct H265ProfileChain {
    /// Codec tail, box-stable; every `base` borrows it.
    codec: Box<vk::VideoDecodeH265ProfileInfoKHR<'static>>,
}

impl H265ProfileChain {
    /// 8-bit 4:2:0 H.265 decode profile for `std_profile_idc`.
    pub(crate) fn new(std_profile_idc: u32) -> Self {
        Self {
            codec: Box::new(vk::VideoDecodeH265ProfileInfoKHR {
                std_profile_idc,
                ..Default::default()
            }),
        }
    }

    /// Base profile for session creation and format queries.
    pub(crate) fn base(&self) -> vk::VideoProfileInfoKHR<'_> {
        vk::VideoProfileInfoKHR {
            video_codec_operation: vk::VideoCodecOperationFlagsKHR::DECODE_H265,
            chroma_subsampling: vk::VideoChromaSubsamplingFlagsKHR::TYPE_420,
            luma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::TYPE_8,
            chroma_bit_depth: vk::VideoComponentBitDepthFlagsKHR::TYPE_8,
            p_next: std::ptr::from_ref(self.codec.as_ref()).cast(),
            ..Default::default()
        }
    }
}

/// Empty H.264 session-parameters template; sets arrive through updates.
pub(crate) fn h264_params_create() -> vk::VideoDecodeH264SessionParametersCreateInfoKHR<'static> {
    vk::VideoDecodeH264SessionParametersCreateInfoKHR {
        max_std_sps_count: H264_MAX_SPS,
        max_std_pps_count: H264_MAX_PPS,
        ..Default::default()
    }
}

/// H.264 session-parameters addition over `sps` and `pps`.
///
/// # Errors
///
/// Returns `Err` when a set count exceeds its field width.
pub(crate) fn h264_params_add<'a>(
    sps: &'a [StdVideoH264SequenceParameterSet],
    pps: &'a [StdVideoH264PictureParameterSet],
) -> Result<vk::VideoDecodeH264SessionParametersAddInfoKHR<'a>, crate::error::ParseError> {
    Ok(vk::VideoDecodeH264SessionParametersAddInfoKHR {
        std_sps_count: count_of(sps.len(), "params sps")?,
        p_std_sp_ss: sps.as_ptr(),
        std_pps_count: count_of(pps.len(), "params pps")?,
        p_std_pp_ss: pps.as_ptr(),
        ..Default::default()
    })
}

/// Empty H.265 session-parameters template; sets arrive through updates.
pub(crate) fn h265_params_create() -> vk::VideoDecodeH265SessionParametersCreateInfoKHR<'static> {
    vk::VideoDecodeH265SessionParametersCreateInfoKHR {
        max_std_vps_count: H265_MAX_VPS,
        max_std_sps_count: H265_MAX_SPS,
        max_std_pps_count: H265_MAX_PPS,
        ..Default::default()
    }
}

/// H.265 session-parameters addition over `vps`, `sps`, and `pps`.
///
/// # Errors
///
/// Returns `Err` when a set count exceeds its field width.
pub(crate) fn h265_params_add<'a>(
    vps: &'a [StdVideoH265VideoParameterSet],
    sps: &'a [StdVideoH265SequenceParameterSet],
    pps: &'a [StdVideoH265PictureParameterSet],
) -> Result<vk::VideoDecodeH265SessionParametersAddInfoKHR<'a>, crate::error::ParseError> {
    Ok(vk::VideoDecodeH265SessionParametersAddInfoKHR {
        std_vps_count: count_of(vps.len(), "params vps")?,
        p_std_vp_ss: vps.as_ptr(),
        std_sps_count: count_of(sps.len(), "params sps")?,
        p_std_sp_ss: sps.as_ptr(),
        std_pps_count: count_of(pps.len(), "params pps")?,
        p_std_pp_ss: pps.as_ptr(),
        ..Default::default()
    })
}

/// Checked narrowing of a set count.
fn count_of(len: usize, syntax: &'static str) -> Result<u32, crate::error::ParseError> {
    u32::try_from(len).map_err(|_| crate::error::ParseError::OutOfRange { syntax })
}

#[cfg(test)]
mod tests {
    #[test]
    fn h264_chain_carries_decode_profile() {
        let chain = super::H264ProfileChain::new(1);
        let base = chain.base();
        assert!(base.video_codec_operation == ash::vk::VideoCodecOperationFlagsKHR::DECODE_H264);
        assert!(base.chroma_subsampling == ash::vk::VideoChromaSubsamplingFlagsKHR::TYPE_420);
        assert!(base.luma_bit_depth == ash::vk::VideoComponentBitDepthFlagsKHR::TYPE_8);
        assert!(!base.p_next.is_null());
        let codec = std::ptr::from_ref(chain.codec.as_ref()).cast();
        assert_eq!(base.p_next, codec);
        assert_eq!(chain.codec.std_profile_idc, 1);
        assert!(
            chain.codec.picture_layout
                == ash::vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE
        );
    }

    #[test]
    fn h265_chain_carries_decode_profile() {
        let chain = super::H265ProfileChain::new(1);
        let base = chain.base();
        assert!(base.video_codec_operation == ash::vk::VideoCodecOperationFlagsKHR::DECODE_H265);
        assert!(base.chroma_subsampling == ash::vk::VideoChromaSubsamplingFlagsKHR::TYPE_420);
        assert!(!base.p_next.is_null());
        let codec = std::ptr::from_ref(chain.codec.as_ref()).cast();
        assert_eq!(base.p_next, codec);
        assert_eq!(chain.codec.std_profile_idc, 1);
    }

    #[test]
    fn params_templates_cover_the_id_spaces() {
        let h264 = super::h264_params_create();
        assert_eq!(h264.max_std_sps_count, super::H264_MAX_SPS);
        assert_eq!(h264.max_std_pps_count, super::H264_MAX_PPS);
        assert!(h264.p_parameters_add_info.is_null());
        let h265 = super::h265_params_create();
        assert_eq!(h265.max_std_vps_count, super::H265_MAX_VPS);
        assert_eq!(h265.max_std_sps_count, super::H265_MAX_SPS);
        assert_eq!(h265.max_std_pps_count, super::H265_MAX_PPS);
        assert!(h265.p_parameters_add_info.is_null());
    }
}
