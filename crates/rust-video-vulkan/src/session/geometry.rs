//! Session sizing validated against ladder capabilities.

use crate::error::{ParseError, VulkanError};
use crate::ladder::{CodecOp, DecodeFacts};
use crate::parse::{h264, h265};

use ash::vk;

/// `H.264` level to its `StdVideo` `idc`.
pub(crate) fn h264_level_to_std(level_idc: u8, constraint_set3: bool) -> Option<u32> {
    use vk::native::{
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_2,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_3,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_2,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_2,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_2,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_2,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_0,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_1,
        StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_2,
    };
    if level_idc == 11 && constraint_set3 {
        return Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_0);
    }
    match level_idc {
        10 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_0),
        11 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_1),
        12 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_2),
        13 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_1_3),
        20 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_0),
        21 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_1),
        22 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_2_2),
        30 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_0),
        31 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_1),
        32 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_3_2),
        40 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_0),
        41 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1),
        42 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_2),
        50 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_0),
        51 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_1),
        52 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_2),
        60 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_0),
        61 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_1),
        62 => Some(StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_6_2),
        _ => None,
    }
}

/// `StdVideo` `H.264` level `idc` to its wire value.
pub(crate) fn h264_std_to_wire(level: u32) -> u8 {
    const WIRE: [u8; 19] = [
        10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52, 60, 61, 62,
    ];
    usize::try_from(level)
        .ok()
        .and_then(|index| WIRE.get(index).copied())
        .unwrap_or(0)
}

/// `H.264` profile to its `StdVideo` `idc`.
pub(crate) fn h264_profile_to_std(profile_idc: u8) -> Option<u32> {
    use vk::native::{
        StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
        StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH,
        StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH_444_PREDICTIVE,
        StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN,
    };
    match profile_idc {
        66 => Some(StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE),
        77 => Some(StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_MAIN),
        100 => Some(StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH),
        244 => Some(StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH_444_PREDICTIVE),
        _ => None,
    }
}

/// `H.265` level to its `StdVideo` `idc`.
pub(crate) fn h265_level_to_std(level_idc: u8) -> Option<u32> {
    use vk::native::{
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_1_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_1,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_3_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_3_1,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_4_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_4_1,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_2,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_0,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_1,
        StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_2,
    };
    match level_idc {
        30 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_1_0),
        60 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_0),
        63 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_2_1),
        90 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_3_0),
        93 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_3_1),
        120 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_4_0),
        123 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_4_1),
        150 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_0),
        153 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1),
        156 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_2),
        180 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_0),
        183 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_1),
        186 => Some(StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_6_2),
        _ => None,
    }
}

/// `StdVideo` `H.265` level `idc` to its wire value.
pub(crate) fn h265_std_to_wire(level: u32) -> u8 {
    const WIRE: [u8; 13] = [30, 60, 63, 90, 93, 120, 123, 150, 153, 156, 180, 183, 186];
    usize::try_from(level)
        .ok()
        .and_then(|index| WIRE.get(index).copied())
        .unwrap_or(0)
}

/// `H.265` profile to its `StdVideo` `idc`.
pub(crate) fn h265_profile_to_std(profile_idc: u8) -> Option<u32> {
    use vk::native::{
        StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN,
        StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_10,
        StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_STILL_PICTURE,
    };
    match profile_idc {
        1 => Some(StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN),
        2 => Some(StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_10),
        3 => Some(StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_STILL_PICTURE),
        _ => None,
    }
}

/// AV1 level index to its comparison value, identity over 0-23 and 31.
///
/// Index 31 carries no level constraints; 24-30 are reserved.
pub(crate) fn av1_level_to_std(level_idx: u8) -> Option<u32> {
    if level_idx <= 23 || level_idx == 31 {
        Some(u32::from(level_idx))
    } else {
        None
    }
}

/// `StdVideo` AV1 level to its index, clamped to the defined range.
pub(crate) fn av1_std_to_wire(level: u32) -> u8 {
    level.min(23) as u8
}

/// AV1 sequence profile to its `StdVideo` value.
pub(crate) fn av1_profile_to_std(profile: u8) -> Option<u32> {
    use vk::native::{
        StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_HIGH,
        StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_MAIN,
        StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_PROFESSIONAL,
    };
    match profile {
        0 => Some(StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_MAIN),
        1 => Some(StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_HIGH),
        2 => Some(StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_PROFESSIONAL),
        _ => None,
    }
}

/// Codec profile pinned to one session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionProfile {
    /// `H.264` profile and level `idc`.
    H264 {
        /// `StdVideo` profile `idc`.
        profile: u32,
        /// `StdVideo` level `idc`.
        level: u32,
    },
    /// `H.265` profile and level `idc`.
    H265 {
        /// `StdVideo` profile `idc`.
        profile: u32,
        /// `StdVideo` level `idc`.
        level: u32,
    },
    /// AV1 profile and level index.
    Av1 {
        /// `StdVideo` profile.
        profile: u32,
        /// Level index, the `StdVideo` value over 0-23.
        level: u32,
    },
}

/// Validated decode session sizing.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct SessionGeometry {
    /// Operation the session runs.
    op: CodecOp,
    /// Codec profile pinned to the session.
    profile: SessionProfile,
    /// Coded picture size in luma samples.
    coded: (u32, u32),
    /// Display picture size in luma samples.
    display: (u32, u32),
    /// DPB slot budget.
    dpb_slots: u32,
    /// Active reference picture budget.
    active_refs: u32,
    /// Picture format.
    format: vk::Format,
    /// Video Std header spec version the session pins.
    std_version: u32,
    /// Bitstream range granularity in bytes.
    bitstream_align: u64,
}

impl std::fmt::Debug for SessionGeometry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGeometry")
            .field("op", &self.op)
            .field("profile", &self.profile)
            .field("coded", &self.coded)
            .field("display", &self.display)
            .field("dpb_slots", &self.dpb_slots)
            .field("active_refs", &self.active_refs)
            .field("format", &self.format.as_raw())
            .field("std_version", &self.std_version)
            .field("bitstream_align", &self.bitstream_align)
            .finish()
    }
}

impl SessionGeometry {
    /// Coded picture size in luma samples.
    #[must_use]
    pub fn coded(&self) -> (u32, u32) {
        self.coded
    }

    /// Display picture size in luma samples.
    #[must_use]
    pub fn display(&self) -> (u32, u32) {
        self.display
    }

    /// DPB slot budget.
    #[must_use]
    pub fn dpb_slots(&self) -> u32 {
        self.dpb_slots
    }

    /// Active reference picture budget.
    #[must_use]
    pub fn active_refs(&self) -> u32 {
        self.active_refs
    }

    /// Picture format.
    #[must_use]
    pub fn format(&self) -> vk::Format {
        self.format
    }

    /// Bitstream range granularity in bytes.
    #[must_use]
    pub fn bitstream_align(&self) -> u64 {
        self.bitstream_align
    }

    /// Geometry from an H.264 sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` on unsupported sampling, unknown profile or level,
    /// level or size above the caps, or no 4:2:0 format.
    pub fn h264(
        sps: &h264::Sps,
        caps: &DecodeFacts,
        formats: &[vk::VideoFormatPropertiesKHR<'_>],
    ) -> Result<Self, VulkanError> {
        if sps.chroma_format_idc() != 1
            || sps.bit_depth_luma_minus8() != 0
            || sps.bit_depth_chroma_minus8() != 0
        {
            return Err(ParseError::Unsupported {
                what: "h264 sampling",
            }
            .into());
        }
        let Some(profile) = h264_profile_to_std(sps.profile_idc()) else {
            return Err(ParseError::Unsupported {
                what: "h264 profile",
            }
            .into());
        };
        let Some(level) = h264_level_to_std(sps.level_idc(), sps.constraint_set3()) else {
            return Err(ParseError::Unsupported { what: "h264 level" }.into());
        };
        if sps.level_idc() > caps.max_level {
            return Err(VulkanError::UnsupportedLevel {
                stream: sps.level_idc(),
                max: caps.max_level,
            });
        }
        Self::sized(
            CodecOp::H264Decode,
            SessionProfile::H264 { profile, level },
            sps.coded_size(),
            sps.display_size(),
            sps.dpb_slots(),
            caps,
            formats,
        )
    }

    /// Geometry from an H.265 sequence set.
    ///
    /// # Errors
    ///
    /// Returns `Err` on unsupported sampling, unknown profile or level,
    /// level or size above the caps, or no 4:2:0 format.
    pub fn h265(
        sps: &h265::Sps,
        caps: &DecodeFacts,
        formats: &[vk::VideoFormatPropertiesKHR<'_>],
    ) -> Result<Self, VulkanError> {
        if sps.chroma_format_idc() != 1
            || sps.bit_depth_luma_minus8() != 0
            || sps.bit_depth_chroma_minus8() != 0
        {
            return Err(ParseError::Unsupported {
                what: "h265 sampling",
            }
            .into());
        }
        let Some(profile) = h265_profile_to_std(sps.ptl().profile_idc()) else {
            return Err(ParseError::Unsupported {
                what: "h265 profile",
            }
            .into());
        };
        let Some(level) = h265_level_to_std(sps.ptl().level_idc()) else {
            return Err(ParseError::Unsupported { what: "h265 level" }.into());
        };
        if sps.ptl().level_idc() > caps.max_level {
            return Err(VulkanError::UnsupportedLevel {
                stream: sps.ptl().level_idc(),
                max: caps.max_level,
            });
        }
        Self::sized(
            CodecOp::H265Decode,
            SessionProfile::H265 { profile, level },
            sps.coded_size(),
            sps.display_size(),
            sps.dpb_slots(),
            caps,
            formats,
        )
    }

    /// Geometry from an AV1 sequence header and the frame display size.
    ///
    /// # Errors
    ///
    /// Returns `Err` on unsupported sampling, unknown profile or level,
    /// level or size above the caps, or no 4:2:0 format.
    pub fn av1(
        seq: &crate::parse::av1::SequenceHeader,
        display: (u32, u32),
        caps: &DecodeFacts,
        formats: &[vk::VideoFormatPropertiesKHR<'_>],
    ) -> Result<Self, VulkanError> {
        if seq.mono_chrome()
            || seq.subsampling_x() != 1
            || seq.subsampling_y() != 1
            || seq.bit_depth() != 8
        {
            return Err(ParseError::Unsupported {
                what: "av1 sampling",
            }
            .into());
        }
        let Some(profile) = av1_profile_to_std(seq.profile()) else {
            return Err(ParseError::Unsupported {
                what: "av1 profile",
            }
            .into());
        };
        if profile != vk::native::StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_MAIN {
            return Err(ParseError::Unsupported {
                what: "av1 profile",
            }
            .into());
        }
        let Some(level) = av1_level_to_std(seq.level_idx()) else {
            return Err(ParseError::Unsupported { what: "av1 level" }.into());
        };
        if seq.level_idx() != 31 && seq.level_idx() > caps.max_level {
            return Err(VulkanError::UnsupportedLevel {
                stream: seq.level_idx(),
                max: caps.max_level,
            });
        }
        let coded = seq.coded_size();
        if display.0 > coded.0 || display.1 > coded.1 {
            return Err(VulkanError::OverCaps {
                what: "coded extent",
            });
        }
        if coded.0 % 2 == 1 || coded.1 % 2 == 1 || display.0 % 2 == 1 || display.1 % 2 == 1 {
            return Err(ParseError::Unsupported {
                what: "av1 sampling",
            }
            .into());
        }
        Self::sized(
            CodecOp::Av1Decode,
            SessionProfile::Av1 { profile, level },
            coded,
            display,
            crate::parse::av1::DPB_SLOTS,
            caps,
            formats,
        )
    }

    /// Geometry from validated sizes against the caps.
    fn sized(
        op: CodecOp,
        profile: SessionProfile,
        coded: (u32, u32),
        display: (u32, u32),
        dpb_slots: u32,
        caps: &DecodeFacts,
        formats: &[vk::VideoFormatPropertiesKHR<'_>],
    ) -> Result<Self, VulkanError> {
        if coded.0 == 0 || coded.1 == 0 {
            return Err(VulkanError::BadExtent {
                width: coded.0,
                height: coded.1,
            });
        }
        if coded.0 > caps.coded_extent.0 || coded.1 > caps.coded_extent.1 {
            return Err(VulkanError::OverCaps {
                what: "coded extent",
            });
        }
        if dpb_slots > caps.dpb_slots {
            return Err(VulkanError::OverCaps { what: "dpb slots" });
        }
        let Some(format) = crate::raw::pick_video_format(formats) else {
            return Err(VulkanError::NoVideoFormat);
        };
        Ok(Self {
            op,
            profile,
            coded,
            display,
            dpb_slots,
            active_refs: caps.active_refs.min(dpb_slots),
            format,
            std_version: caps.std_version,
            bitstream_align: caps.min_bitstream_align.max(1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        h264_level_to_std, h264_profile_to_std, h264_std_to_wire, h265_level_to_std,
        h265_profile_to_std, h265_std_to_wire,
    };
    use crate::ladder::DecodeFacts;
    use ash::vk;

    /// Caps covering the fork demo vectors.
    fn caps() -> DecodeFacts {
        DecodeFacts {
            coded_extent: (1920, 1080),
            dpb_slots: 16,
            active_refs: 8,
            max_level: 52,
            std_version: 1,
            min_bitstream_align: 256,
        }
    }

    /// Format list with two-plane NV12.
    fn formats() -> Vec<vk::VideoFormatPropertiesKHR<'static>> {
        vec![vk::VideoFormatPropertiesKHR::default().format(vk::Format::G8_B8R8_2PLANE_420_UNORM)]
    }

    #[test]
    fn level_maps_round_trip() {
        for (wire, std) in [(10, 0), (13, 3), (31, 8), (42, 12), (52, 15), (62, 18)] {
            assert_eq!(h264_level_to_std(wire, false), Some(std));
            assert_eq!(h264_std_to_wire(std), wire);
        }
        assert_eq!(h264_level_to_std(11, true), Some(0));
        assert_eq!(h264_level_to_std(11, false), Some(1));
        assert_eq!(h264_level_to_std(9, false), None);
        assert_eq!(h264_std_to_wire(19), 0);
        for (wire, std) in [(30, 0), (63, 2), (123, 6), (156, 9), (186, 12)] {
            assert_eq!(h265_level_to_std(wire), Some(std));
            assert_eq!(h265_std_to_wire(std), wire);
        }
        assert_eq!(h265_level_to_std(31), None);
        assert_eq!(h265_std_to_wire(13), 0);
    }

    #[test]
    fn profile_maps_known_only() {
        assert!(h264_profile_to_std(66).is_some());
        assert!(h264_profile_to_std(77).is_some());
        assert!(h264_profile_to_std(100).is_some());
        assert!(h264_profile_to_std(244).is_some());
        assert_eq!(h264_profile_to_std(110), None);
        assert_eq!(h264_profile_to_std(118), None);
        assert!(h265_profile_to_std(1).is_some());
        assert!(h265_profile_to_std(2).is_some());
        assert_eq!(h265_profile_to_std(4), None);
    }

    #[test]
    fn demo_sps_sizes_session() {
        let units = crate::parse::test_support::split(&[crate::parse::test_support::hex(
            "6764000dacb20283f60220000003002000000641e2854900",
        )]);
        let mut parser = crate::parse::h264::H264Parser::new();
        let crate::parse::h264::ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let geometry = super::SessionGeometry::h264(&sps, &caps(), &formats()).expect("geometry");
        assert_eq!(geometry.coded(), (320, 240));
        assert_eq!(geometry.dpb_slots(), 4);
        assert!(geometry.format() == vk::Format::G8_B8R8_2PLANE_420_UNORM);
    }

    #[test]
    fn av1_level_profile_maps_cover_defined_values() {
        for level in 0..=23 {
            assert_eq!(super::av1_level_to_std(level), Some(u32::from(level)));
            assert_eq!(super::av1_std_to_wire(u32::from(level)), level);
        }
        assert_eq!(super::av1_level_to_std(24), None);
        assert_eq!(super::av1_level_to_std(30), None);
        assert_eq!(super::av1_level_to_std(31), Some(31));
        assert_eq!(super::av1_std_to_wire(31), 23);
        assert_eq!(super::av1_profile_to_std(0), Some(0));
        assert_eq!(super::av1_profile_to_std(1), Some(1));
        assert_eq!(super::av1_profile_to_std(2), Some(2));
        assert_eq!(super::av1_profile_to_std(3), None);
    }

    #[test]
    fn av1_sequence_sizes_session() {
        let seq = crate::parse::av1::SequenceHeader::test_main_420();
        let mut capped = caps();
        capped.max_level = 4;
        let geometry =
            super::SessionGeometry::av1(&seq, (320, 240), &capped, &formats()).expect("geometry");
        assert_eq!(geometry.coded(), (320, 240));
        assert_eq!(geometry.display(), (320, 240));
        assert_eq!(geometry.dpb_slots(), 9);
        assert!(matches!(
            super::SessionGeometry::av1(&seq, (321, 240), &capped, &formats()).unwrap_err(),
            crate::error::VulkanError::OverCaps { .. }
        ));
        let mut low = capped;
        low.max_level = 3;
        assert!(matches!(
            super::SessionGeometry::av1(&seq, (320, 240), &low, &formats()).unwrap_err(),
            crate::error::VulkanError::UnsupportedLevel { stream: 4, max: 3 }
        ));
    }

    #[test]
    fn level_above_caps_errors() {
        let units = crate::parse::test_support::split(&[crate::parse::test_support::hex(
            "6764000dacb20283f60220000003002000000641e2854900",
        )]);
        let mut parser = crate::parse::h264::H264Parser::new();
        let crate::parse::h264::ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let mut capped = caps();
        capped.max_level = 12;
        assert!(matches!(
            super::SessionGeometry::h264(&sps, &capped, &formats()).unwrap_err(),
            crate::error::VulkanError::UnsupportedLevel {
                stream: 13,
                max: 12
            }
        ));
        assert!(matches!(
            super::SessionGeometry::h264(&sps, &caps(), &[]).unwrap_err(),
            crate::error::VulkanError::NoVideoFormat
        ));
        let mut small = caps();
        small.coded_extent = (16, 16);
        assert!(matches!(
            super::SessionGeometry::h264(&sps, &small, &formats()).unwrap_err(),
            crate::error::VulkanError::OverCaps { .. }
        ));
    }
}
