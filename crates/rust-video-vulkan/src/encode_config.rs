//! Shared encode config mapped onto Vulkan session parameters.
//!
//! Pure mapping over the negotiated caps; `open` runs it before touching the device.

use ash::vk;

use rust_video_caps::color::ColorRange;
use rust_video_caps::common::ChromaSampling;
use rust_video_caps::encode::{Decline, EncodeConfig, Level, Profile, RateControl, SpeedPreset};
use rust_video_caps::{h264, h265};
use rust_video_core::Gop;

use crate::error::VulkanError;
use crate::ladder::{CodecOp, EncodeFacts};

/// Mapped color VUI signalled in the sequence set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MappedVui {
    /// Coded `colour_primaries` value.
    pub(crate) primaries: u8,
    /// Coded `transfer_characteristics` value.
    pub(crate) transfer: u8,
    /// Coded `matrix_coefficients` value.
    pub(crate) matrix: u8,
    /// Full-range levels, else studio range.
    pub(crate) full_range: bool,
}

/// Encode config mapped onto one Vulkan session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MappedEncode {
    /// Engine IDR cadence.
    pub(crate) gop: Gop,
    /// Mapped rate control behind the session.
    pub(crate) rate: MappedRate,
    /// Standard profile idc behind the sets, within the negotiated profile.
    pub(crate) profile: u32,
    /// Wire level idc behind the sets.
    pub(crate) level: u8,
    /// Color VUI behind the sequence set, absent under legacy control.
    pub(crate) vui: Option<MappedVui>,
    /// Driver quality level behind the preset, absent under legacy control.
    pub(crate) quality: Option<u32>,
}

/// Rate control mapped onto one encode layer.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct MappedRate {
    /// Rate control mode behind the session.
    pub(crate) mode: vk::VideoEncodeRateControlModeFlagsKHR,
    /// Layer average bitrate in bits per second.
    pub(crate) average: u64,
    /// Layer peak bitrate in bits per second.
    pub(crate) max: u64,
    /// Layer frame rate numerator and denominator.
    pub(crate) fps: (u32, u32),
    /// Virtual buffer window in milliseconds.
    pub(crate) virtual_ms: u32,
    /// Initial virtual buffer fullness in milliseconds.
    pub(crate) initial_ms: u32,
    /// Constant QP behind PPS init, slice constant, and the layer clamp.
    pub(crate) qp: Option<u8>,
}

impl std::fmt::Debug for MappedRate {
    /// Mapped rate with the mode flags as raw bits.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedRate")
            .field("mode", &self.mode.as_raw())
            .field("average", &self.average)
            .field("max", &self.max)
            .field("fps", &self.fps)
            .field("virtual_ms", &self.virtual_ms)
            .field("initial_ms", &self.initial_ms)
            .field("qp", &self.qp)
            .finish()
    }
}

impl MappedEncode {
    /// Config mapped for `op` at `width`x`height` under `facts`.
    ///
    /// `Auto` rate control keeps the tier legacy recipe: an IDR every
    /// 30 pictures, extent-derived CBR at 60 fps, the negotiated
    /// profile, the extent-derived level, and no VUI or quality
    /// override. Explicit modes map every knob; the unmappable ones
    /// decline typed.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a structurally invalid config, a profile or
    /// level outside the negotiated session, or a knob the tier lacks.
    pub(crate) fn map(
        op: CodecOp,
        facts: &EncodeFacts,
        width: u32,
        height: u32,
        config: &EncodeConfig,
    ) -> Result<Self, VulkanError> {
        config.validate()?;
        let family = match (op, config.profile, config.level) {
            (CodecOp::H264Encode, Profile::H264(_), Level::H264(_)) => Family::H264,
            (CodecOp::H265Encode, Profile::H265(_), Level::H265(_)) => Family::H265,
            _ => {
                return Err(Decline::ProfileUnsupported {
                    profile: config.profile,
                }
                .into());
            }
        };
        if config.bit_depth != 8 {
            return Err(Decline::BitDepthUnsupported {
                actual: config.bit_depth,
                max: 8,
            }
            .into());
        }
        if config.chroma != ChromaSampling::Yuv420 {
            return Err(Decline::ChromaUnsupported {
                chroma: config.chroma,
            }
            .into());
        }
        if config.rate_control == RateControl::Auto {
            return Self::legacy(family, facts, width, height);
        }
        let rate = Self::rate(facts, width, height, config)?;
        let gop = Self::gop(config)?;
        let profile = Self::profile(family, facts, config.profile)?;
        let level = Self::level(family, width, height, config.level)?;
        let vui = Self::vui(config)?;
        let quality = Self::quality(facts, config.quality.preset);
        Ok(Self {
            gop,
            rate,
            profile,
            level,
            vui: Some(vui),
            quality: Some(quality),
        })
    }

    /// Legacy recipe: IDR30, extent CBR at 60 fps, negotiated profile.
    fn legacy(
        family: Family,
        facts: &EncodeFacts,
        width: u32,
        height: u32,
    ) -> Result<Self, VulkanError> {
        if !facts
            .rate_control_modes
            .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
        {
            return Err(VulkanError::OverCaps {
                what: "rate control",
            });
        }
        let extent = vk::Extent2D { width, height };
        let recipe = crate::session::encode::RateControl::cbr_for(extent);
        let level = match family {
            Family::H264 => {
                let mbs = width.div_ceil(16) * height.div_ceil(16);
                crate::session::encode::h264_level_for(mbs)
            }
            Family::H265 => {
                let samples = u64::from(width) * u64::from(height);
                crate::session::encode::h265_level_for(samples)
            }
        }
        .ok_or(VulkanError::OverCaps {
            what: "coded extent",
        })?;
        Ok(Self {
            gop: Gop::interval(30),
            rate: MappedRate {
                mode: vk::VideoEncodeRateControlModeFlagsKHR::CBR,
                average: recipe.average(),
                max: recipe.peak(),
                fps: (
                    crate::session::encode::RC_FPS,
                    crate::session::encode::RC_FPS_DENOM,
                ),
                virtual_ms: crate::session::encode::VIRTUAL_BUFFER_MS,
                initial_ms: crate::session::encode::INITIAL_BUFFER_MS,
                qp: None,
            },
            profile: facts.profile,
            level,
            vui: None,
            quality: None,
        })
    }

    /// Rate mode, layer bitrates, frame rate, buffer window, and QP.
    fn rate(
        facts: &EncodeFacts,
        width: u32,
        height: u32,
        config: &EncodeConfig,
    ) -> Result<MappedRate, VulkanError> {
        let extent = vk::Extent2D { width, height };
        let fps = (config.frame_rate.num, config.frame_rate.den);
        match config.rate_control {
            RateControl::Auto => unreachable!("legacy path maps Auto"),
            RateControl::Cqp { qp } => {
                if qp > 51 {
                    return Err(Decline::RateControlInvalid {
                        mode: config.rate_control,
                    }
                    .into());
                }
                Self::require_mode(
                    facts,
                    vk::VideoEncodeRateControlModeFlagsKHR::DISABLED,
                    config.rate_control,
                )?;
                let filler = crate::session::encode::RateControl::cbr_for(extent);
                Ok(MappedRate {
                    mode: vk::VideoEncodeRateControlModeFlagsKHR::DISABLED,
                    average: filler.average(),
                    max: filler.peak(),
                    fps,
                    virtual_ms: crate::session::encode::VIRTUAL_BUFFER_MS,
                    initial_ms: crate::session::encode::INITIAL_BUFFER_MS,
                    qp: Some(qp),
                })
            }
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => {
                Self::require_mode(
                    facts,
                    vk::VideoEncodeRateControlModeFlagsKHR::CBR,
                    config.rate_control,
                )?;
                Self::require_bitrate(facts, maxrate, config.rate_control)?;
                let window = u64::from(bufsize) * 1000 / u64::from(maxrate);
                let virtual_ms = u32::try_from(window).unwrap_or(u32::MAX).max(1);
                Ok(MappedRate {
                    mode: vk::VideoEncodeRateControlModeFlagsKHR::CBR,
                    average: u64::from(bitrate),
                    max: u64::from(maxrate),
                    fps,
                    virtual_ms,
                    initial_ms: virtual_ms / 2,
                    qp: None,
                })
            }
            RateControl::Vbr { bitrate, maxrate } => {
                Self::require_mode(
                    facts,
                    vk::VideoEncodeRateControlModeFlagsKHR::VBR,
                    config.rate_control,
                )?;
                Self::require_bitrate(facts, maxrate, config.rate_control)?;
                Ok(MappedRate {
                    mode: vk::VideoEncodeRateControlModeFlagsKHR::VBR,
                    average: u64::from(bitrate),
                    max: u64::from(maxrate),
                    fps,
                    virtual_ms: crate::session::encode::VIRTUAL_BUFFER_MS,
                    initial_ms: crate::session::encode::INITIAL_BUFFER_MS,
                    qp: None,
                })
            }
            RateControl::Crf { .. } => Err(Decline::RateControlUnsupported {
                mode: config.rate_control,
            }
            .into()),
        }
    }

    /// `mode` present in the driver flags, else a typed decline.
    fn require_mode(
        facts: &EncodeFacts,
        mode: vk::VideoEncodeRateControlModeFlagsKHR,
        requested: RateControl,
    ) -> Result<(), VulkanError> {
        if facts.rate_control_modes.contains(mode) {
            Ok(())
        } else {
            Err(Decline::RateControlUnsupported { mode: requested }.into())
        }
    }

    /// `maxrate` within the driver peak, else a typed decline.
    fn require_bitrate(
        facts: &EncodeFacts,
        maxrate: u32,
        requested: RateControl,
    ) -> Result<(), VulkanError> {
        if facts.max_bitrate == 0 || u64::from(maxrate) <= facts.max_bitrate {
            Ok(())
        } else {
            Err(Decline::RateControlUnsupported { mode: requested }.into())
        }
    }

    /// Fixed IDR cadence over `keyint`, B-frames and refs checked.
    fn gop(config: &EncodeConfig) -> Result<Gop, VulkanError> {
        let gop = config.gop;
        if gop.b_frames > 0 {
            return Err(Decline::BFramesUnsupported {
                actual: gop.b_frames,
                max: 0,
            }
            .into());
        }
        if gop.refs > 1 {
            return Err(Decline::RefsUnsupported {
                actual: gop.refs,
                max: 1,
            }
            .into());
        }
        if gop.refs == 0 && gop.keyint > 1 {
            return Err(Decline::GopInvalid { gop }.into());
        }
        Ok(Gop::interval(gop.keyint))
    }

    /// Requested profile idc within the negotiated session profile.
    fn profile(family: Family, facts: &EncodeFacts, profile: Profile) -> Result<u32, VulkanError> {
        let (rank, idc) = match (family, profile) {
            (Family::H264, Profile::H264(h264::EncodeProfile::Baseline)) => (0, 66),
            (Family::H264, Profile::H264(h264::EncodeProfile::Main)) => (1, 77),
            (Family::H264, Profile::H264(h264::EncodeProfile::High)) => (2, 100),
            (Family::H264, Profile::H264(h264::EncodeProfile::High10)) => (3, 110),
            (Family::H265, Profile::H265(h265::EncodeProfile::Main)) => (0, 1),
            (Family::H265, Profile::H265(h265::EncodeProfile::Main10)) => (1, 2),
            _ => {
                return Err(Decline::ProfileUnsupported { profile }.into());
            }
        };
        let negotiated = match family {
            Family::H264 => match facts.profile {
                66 => Some(0),
                77 => Some(1),
                100 => Some(2),
                _ => None,
            },
            Family::H265 => match facts.profile {
                1 => Some(0),
                2 => Some(1),
                _ => None,
            },
        };
        match negotiated {
            Some(cap) if rank <= cap => Ok(idc),
            None if idc == facts.profile => Ok(idc),
            _ => Err(Decline::ProfileUnsupported { profile }.into()),
        }
    }

    /// Wire level idc: derived under auto, coverage-checked when pinned.
    fn level(family: Family, width: u32, height: u32, level: Level) -> Result<u8, VulkanError> {
        match (family, level) {
            (Family::H264, Level::H264(h264::EncodeLevel::Auto)) => {
                let mbs = width.div_ceil(16) * height.div_ceil(16);
                crate::session::encode::h264_level_for(mbs).ok_or(VulkanError::OverCaps {
                    what: "coded extent",
                })
            }
            (Family::H265, Level::H265(h265::EncodeLevel::Auto)) => {
                let samples = u64::from(width) * u64::from(height);
                crate::session::encode::h265_level_for(samples).ok_or(VulkanError::OverCaps {
                    what: "coded extent",
                })
            }
            (Family::H264, Level::H264(h264::EncodeLevel::Pinned(pinned))) => {
                if pinned == h264::Level::L1b {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                let mbs = width.div_ceil(16) * height.div_ceil(16);
                let covers = pinned.caps().is_some_and(|caps| mbs <= caps.fs_mb);
                if !covers {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                let idc = pinned.idc();
                if crate::session::geometry::h264_level_to_std(idc, false).is_none() {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                Ok(idc)
            }
            (Family::H265, Level::H265(h265::EncodeLevel::Pinned(pinned))) => {
                let samples = u64::from(width) * u64::from(height);
                let covers = pinned
                    .caps()
                    .is_some_and(|caps| samples <= u64::from(caps.ps));
                if !covers {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                let idc = u8::try_from(pinned.idc()).unwrap_or(u8::MAX);
                if crate::session::geometry::h265_level_to_std(idc).is_none() {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                Ok(idc)
            }
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// Color VUI over the SDR signal; HDR metadata declines.
    fn vui(config: &EncodeConfig) -> Result<MappedVui, VulkanError> {
        if config.is_hdr() {
            return Err(Decline::HdrUnsupported.into());
        }
        Ok(MappedVui {
            primaries: config.color.primaries.idc(),
            transfer: config.color.transfer.idc(),
            matrix: config.color.matrix.idc(),
            full_range: config.color.range == ColorRange::Full,
        })
    }

    /// Driver quality level behind `preset`.
    fn quality(facts: &EncodeFacts, preset: SpeedPreset) -> u32 {
        let top = facts.quality_levels.saturating_sub(1);
        match preset {
            SpeedPreset::Realtime => 0,
            SpeedPreset::Balanced => top / 2,
            SpeedPreset::Quality => top,
        }
    }
}

/// Codec family behind one encode session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Family {
    /// H.264 session.
    H264,
    /// H.265 session.
    H265,
}

#[cfg(test)]
mod tests {
    use ash::vk;
    use rust_video_caps::color::{ColorConfig, ContentLight, MasteringDisplay};
    use rust_video_caps::common::ChromaSampling;
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::{h264, h265};

    use super::MappedEncode;
    use crate::error::VulkanError;
    use crate::ladder::{CodecOp, EncodeFacts};

    /// Negotiated caps with every rate mode, `profile`, and a 20 Mbps peak.
    fn facts(profile: u32) -> EncodeFacts {
        EncodeFacts {
            coded_extent: (1920, 1080),
            dpb_slots: 2,
            active_refs: 1,
            quality_levels: 4,
            max_bitrate: 20_000_000,
            std_version: 0,
            profile,
            feedback: vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
            rate_control_modes: vk::VideoEncodeRateControlModeFlagsKHR::CBR
                | vk::VideoEncodeRateControlModeFlagsKHR::VBR
                | vk::VideoEncodeRateControlModeFlagsKHR::DISABLED,
        }
    }

    /// H.264 config over `rate_control` and `gop`, defaults elsewhere.
    fn h264(rate_control: RateControl, gop: Gop) -> EncodeConfig {
        EncodeConfig {
            rate_control,
            gop,
            ..Default::default()
        }
    }

    /// H.265 config over `rate_control` and `gop`, defaults elsewhere.
    fn h265(rate_control: RateControl, gop: Gop) -> EncodeConfig {
        EncodeConfig {
            rate_control,
            gop,
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..Default::default()
        }
    }

    /// GOP cadence of `keyint` without B-frames or extra refs.
    fn cadence(keyint: u32) -> Gop {
        Gop {
            keyint,
            min_keyint: keyint,
            b_frames: 0,
            refs: 1,
        }
    }

    /// Decline behind a declined map, panicking on any other outcome.
    fn decline(result: Result<MappedEncode, VulkanError>) -> Decline {
        match result.unwrap_err() {
            VulkanError::Declined(decline) => decline,
            error => panic!("want a typed decline, got {error}"),
        }
    }

    #[test]
    fn auto_keeps_legacy_recipe() {
        let facts = facts(100);
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            gop: Gop {
                keyint: 7,
                min_keyint: 2,
                b_frames: 0,
                refs: 1,
            },
            frame_rate: FrameRate { num: 24, den: 1 },
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L5)),
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..Default::default()
        };
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("auto maps");
        assert_eq!(mapped.gop.period(), 30);
        assert_eq!(
            mapped.rate.mode.as_raw(),
            vk::VideoEncodeRateControlModeFlagsKHR::CBR.as_raw()
        );
        assert_eq!(mapped.rate.fps, (60, 1));
        assert_eq!(
            (mapped.rate.virtual_ms, mapped.rate.initial_ms),
            (1000, 500)
        );
        assert_eq!(mapped.rate.qp, None);
        assert_eq!(mapped.profile, 100);
        assert_eq!(mapped.level, 20);
        assert_eq!(mapped.vui, None);
        assert_eq!(mapped.quality, None);
        let recipe = crate::session::encode::RateControl::cbr_for(vk::Extent2D {
            width: 320,
            height: 240,
        });
        assert_eq!(mapped.rate.average, recipe.average());
        assert_eq!(mapped.rate.max, recipe.peak());
    }

    #[test]
    fn auto_rejects_wrong_codec() {
        let facts = facts(100);
        let config = h265(RateControl::Auto, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::ProfileUnsupported {
                profile: config.profile,
            }
        );
    }

    #[test]
    fn auto_rejects_depth() {
        let facts = facts(100);
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            profile: Profile::H264(h264::EncodeProfile::High10),
            bit_depth: 10,
            ..Default::default()
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::BitDepthUnsupported { actual: 10, max: 8 }
        );
    }

    #[test]
    fn auto_missing_cbr_stays_over_caps() {
        let mut facts = facts(100);
        facts.rate_control_modes = vk::VideoEncodeRateControlModeFlagsKHR::VBR;
        let config = h264(RateControl::Auto, cadence(30));
        assert!(
            matches!(
                MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config),
                Err(VulkanError::OverCaps {
                    what: "rate control"
                })
            ),
            "legacy CBR gap stays over caps"
        );
    }

    #[test]
    fn auto_extent_overflow_stays_over_caps() {
        let facts = facts(100);
        let config = h264(RateControl::Auto, cadence(30));
        assert!(
            matches!(
                MappedEncode::map(CodecOp::H264Encode, &facts, 8192, 4320, &config),
                Err(VulkanError::OverCaps {
                    what: "coded extent"
                })
            ),
            "legacy extent gap stays over caps"
        );
    }

    #[test]
    fn keyint_maps_to_interval() {
        let facts = facts(100);
        let config = h264(RateControl::default(), cadence(20));
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("keyint maps");
        assert_eq!(mapped.gop.period(), 20);
    }

    #[test]
    fn min_keyint_accepted() {
        let facts = facts(100);
        let config = h264(
            RateControl::default(),
            Gop {
                keyint: 60,
                min_keyint: 10,
                b_frames: 0,
                refs: 1,
            },
        );
        let mapped = MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config)
            .expect("min keyint maps");
        assert_eq!(mapped.gop.period(), 60);
    }

    #[test]
    fn b_frames_decline() {
        let facts = facts(100);
        let config = h264(
            RateControl::default(),
            Gop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 2,
                refs: 1,
            },
        );
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::BFramesUnsupported { actual: 2, max: 0 }
        );
    }

    #[test]
    fn refs_decline() {
        let facts = facts(100);
        let config = h264(
            RateControl::default(),
            Gop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 0,
                refs: 4,
            },
        );
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RefsUnsupported { actual: 4, max: 1 }
        );
    }

    #[test]
    fn zero_refs_needs_all_intra() {
        let facts = facts(100);
        let gop = Gop {
            keyint: 4,
            min_keyint: 4,
            b_frames: 0,
            refs: 0,
        };
        let config = h264(RateControl::default(), gop);
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::GopInvalid { gop }
        );
        let all_intra = Gop {
            keyint: 1,
            min_keyint: 1,
            b_frames: 0,
            refs: 0,
        };
        let config = h264(RateControl::default(), all_intra);
        let mapped = MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config)
            .expect("zero refs all-intra maps");
        assert_eq!(mapped.gop.period(), 1);
    }

    #[test]
    fn cqp_maps_disabled_with_qp() {
        let facts = facts(100);
        let mode = RateControl::Cqp { qp: 23 };
        let config = EncodeConfig {
            rate_control: mode,
            frame_rate: FrameRate { num: 24, den: 1 },
            ..h264(mode, cadence(30))
        };
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("cqp maps");
        assert_eq!(
            mapped.rate.mode.as_raw(),
            vk::VideoEncodeRateControlModeFlagsKHR::DISABLED.as_raw()
        );
        assert_eq!(mapped.rate.qp, Some(23));
        assert_eq!(mapped.rate.fps, (24, 1));
        let filler = crate::session::encode::RateControl::cbr_for(vk::Extent2D {
            width: 320,
            height: 240,
        });
        assert_eq!(mapped.rate.average, filler.average());
        assert_eq!(mapped.rate.max, filler.peak());
    }

    #[test]
    fn cqp_range_declines() {
        let facts = facts(100);
        let mode = RateControl::Cqp { qp: 52 };
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlInvalid { mode }
        );
    }

    #[test]
    fn cqp_missing_mode_declines() {
        let mut facts = facts(100);
        facts.rate_control_modes = vk::VideoEncodeRateControlModeFlagsKHR::CBR;
        let mode = RateControl::Cqp { qp: 23 };
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlUnsupported { mode }
        );
    }

    #[test]
    fn cbr_maps_layers() {
        let facts = facts(100);
        let mode = RateControl::Cbr {
            bitrate: 4_000_000,
            maxrate: 5_000_000,
            bufsize: 5_000_000,
        };
        let config = EncodeConfig {
            rate_control: mode,
            frame_rate: FrameRate { num: 24, den: 1 },
            ..h264(mode, cadence(30))
        };
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("cbr maps");
        assert_eq!(
            mapped.rate.mode.as_raw(),
            vk::VideoEncodeRateControlModeFlagsKHR::CBR.as_raw()
        );
        assert_eq!(
            (mapped.rate.average, mapped.rate.max),
            (4_000_000, 5_000_000)
        );
        assert_eq!(mapped.rate.fps, (24, 1));
        assert_eq!(
            (mapped.rate.virtual_ms, mapped.rate.initial_ms),
            (1000, 500)
        );
        assert_eq!(mapped.rate.qp, None);
    }

    #[test]
    fn cbr_buffer_window_math() {
        let facts = facts(100);
        let mode = RateControl::Cbr {
            bitrate: 8_000_000,
            maxrate: 8_000_000,
            bufsize: 2_000_000,
        };
        let config = h264(mode, cadence(30));
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("cbr maps");
        assert_eq!((mapped.rate.virtual_ms, mapped.rate.initial_ms), (250, 125));
    }

    #[test]
    fn cbr_over_driver_peak_declines() {
        let facts = facts(100);
        let mode = RateControl::Cbr {
            bitrate: 30_000_000,
            maxrate: 30_000_000,
            bufsize: 30_000_000,
        };
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlUnsupported { mode }
        );
        let mut unguarded = facts;
        unguarded.max_bitrate = 0;
        MappedEncode::map(CodecOp::H264Encode, &unguarded, 320, 240, &config)
            .expect("zero peak skips the check");
    }

    #[test]
    fn cbr_missing_mode_declines_typed() {
        let mut facts = facts(100);
        facts.rate_control_modes = vk::VideoEncodeRateControlModeFlagsKHR::VBR;
        let mode = RateControl::default();
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlUnsupported { mode }
        );
    }

    #[test]
    fn vbr_maps_layers() {
        let facts = facts(100);
        let mode = RateControl::Vbr {
            bitrate: 4_000_000,
            maxrate: 6_000_000,
        };
        let config = h264(mode, cadence(30));
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("vbr maps");
        assert_eq!(
            mapped.rate.mode.as_raw(),
            vk::VideoEncodeRateControlModeFlagsKHR::VBR.as_raw()
        );
        assert_eq!(
            (mapped.rate.average, mapped.rate.max),
            (4_000_000, 6_000_000)
        );
        assert_eq!(
            (mapped.rate.virtual_ms, mapped.rate.initial_ms),
            (1000, 500)
        );
    }

    #[test]
    fn vbr_missing_mode_declines() {
        let mut facts = facts(100);
        facts.rate_control_modes = vk::VideoEncodeRateControlModeFlagsKHR::CBR;
        let mode = RateControl::Vbr {
            bitrate: 4_000_000,
            maxrate: 6_000_000,
        };
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlUnsupported { mode }
        );
    }

    #[test]
    fn crf_declines() {
        let facts = facts(100);
        let mode = RateControl::Crf { value: 23 };
        let config = h264(mode, cadence(30));
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::RateControlUnsupported { mode }
        );
    }

    #[test]
    fn profile_subset_encodes_requested() {
        let high = facts(100);
        let config = h264(RateControl::default(), cadence(30));
        let mapped = MappedEncode::map(CodecOp::H264Encode, &high, 320, 240, &config)
            .expect("main under high maps");
        assert_eq!(mapped.profile, 77);
        let main = facts(77);
        let config = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::High),
            ..h264(RateControl::default(), cadence(30))
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &main,
                320,
                240,
                &config
            )),
            Decline::ProfileUnsupported {
                profile: config.profile,
            }
        );
        let main10 = facts(2);
        let config = h265(RateControl::default(), cadence(30));
        let mapped = MappedEncode::map(CodecOp::H265Encode, &main10, 320, 240, &config)
            .expect("main under main10 maps");
        assert_eq!(mapped.profile, 1);
    }

    #[test]
    fn level_auto_derives_and_pinned_covers() {
        let high = facts(100);
        let config = h264(RateControl::default(), cadence(30));
        let mapped = MappedEncode::map(CodecOp::H264Encode, &high, 320, 240, &config)
            .expect("auto level maps");
        assert_eq!(mapped.level, 20);
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L4)),
            ..h264(RateControl::default(), cadence(30))
        };
        let mapped = MappedEncode::map(CodecOp::H264Encode, &high, 320, 240, &config)
            .expect("pinned level maps");
        assert_eq!(mapped.level, 40);
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1)),
            ..h264(RateControl::default(), cadence(30))
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &high,
                1920,
                1080,
                &config
            )),
            Decline::LevelUnsupported {
                level: config.level,
            }
        );
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1b)),
            ..h264(RateControl::default(), cadence(30))
        };
        assert!(
            MappedEncode::map(CodecOp::H264Encode, &high, 320, 240, &config).is_err(),
            "1b stays inexpressible"
        );
        let main10 = facts(2);
        let config = EncodeConfig {
            level: Level::H265(h265::EncodeLevel::Pinned(h265::Level::L63)),
            ..h265(RateControl::default(), cadence(30))
        };
        assert!(
            MappedEncode::map(CodecOp::H265Encode, &main10, 320, 240, &config).is_err(),
            "unmapped level declines"
        );
        let config = h265(RateControl::default(), cadence(30));
        let mapped = MappedEncode::map(CodecOp::H265Encode, &main10, 1920, 1080, &config)
            .expect("auto level maps");
        assert_eq!(mapped.level, 120);
    }

    #[test]
    fn preset_maps_quality() {
        let facts = facts(100);
        for (preset, level) in [
            (SpeedPreset::Realtime, 0),
            (SpeedPreset::Balanced, 1),
            (SpeedPreset::Quality, 3),
        ] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..h264(RateControl::default(), cadence(30))
            };
            let mapped = MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config)
                .expect("preset maps");
            assert_eq!(mapped.quality, Some(level), "{preset:?} maps");
        }
        let mut flat = facts;
        flat.quality_levels = 0;
        let config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..h264(RateControl::default(), cadence(30))
        };
        let mapped = MappedEncode::map(CodecOp::H264Encode, &flat, 320, 240, &config)
            .expect("single level maps");
        assert_eq!(mapped.quality, Some(0));
    }

    #[test]
    fn color_maps_vui() {
        let facts = facts(100);
        let config = h264(RateControl::default(), cadence(30));
        let mapped =
            MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config).expect("color maps");
        let vui = mapped.vui.expect("explicit modes carry vui");
        assert_eq!((vui.primaries, vui.transfer, vui.matrix), (1, 1, 1));
        assert!(!vui.full_range);
        let config = EncodeConfig {
            color: ColorConfig {
                range: rust_video_caps::color::ColorRange::Full,
                ..Default::default()
            },
            ..h264(RateControl::default(), cadence(30))
        };
        let mapped = MappedEncode::map(CodecOp::H264Encode, &facts, 320, 240, &config)
            .expect("full range maps");
        assert!(mapped.vui.expect("vui present").full_range);
    }

    #[test]
    fn hdr_declines() {
        let facts = facts(100);
        let config = EncodeConfig {
            color: ColorConfig {
                mastering: Some(MasteringDisplay {
                    red_x: 34_000,
                    red_y: 16_000,
                    green_x: 13_250,
                    green_y: 37_500,
                    blue_x: 7_500,
                    blue_y: 3_000,
                    white_x: 15_625,
                    white_y: 16_450,
                    max_luminance: 10_000_000,
                    min_luminance: 1,
                }),
                light: Some(ContentLight {
                    max_cll: 1000,
                    max_fall: 400,
                }),
                ..Default::default()
            },
            ..h264(RateControl::default(), cadence(30))
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::HdrUnsupported
        );
    }

    #[test]
    fn validate_passthrough() {
        let facts = facts(100);
        let gop = Gop {
            keyint: 0,
            min_keyint: 60,
            b_frames: 0,
            refs: 1,
        };
        let config = h264(RateControl::default(), gop);
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::GopInvalid { gop }
        );
        let config = EncodeConfig {
            level: Level::H265(h265::EncodeLevel::Auto),
            ..h264(RateControl::default(), cadence(30))
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::ProfileLevelMismatch {
                profile: config.profile,
                level: config.level,
            }
        );
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..h264(RateControl::default(), cadence(30))
        };
        assert_eq!(
            decline(MappedEncode::map(
                CodecOp::H264Encode,
                &facts,
                320,
                240,
                &config
            )),
            Decline::ChromaUnsupported {
                chroma: ChromaSampling::Yuv444,
            }
        );
    }
}
