//! Shared encode config mapped onto VA-API session parameters.
//!
//! Pure mapping over probed driver caps; `open` runs it before touching the device.

use std::os::raw::c_int;

use crate::error::VaApiError;
use crate::ffi;

use rust_video_caps::common::{ChromaSampling, PictureSize};
use rust_video_caps::encode::{Decline, EncodeConfig, Level, Profile, RateControl};
use rust_video_caps::reason::UnsupportedReason as CapsUnsupported;
use rust_video_caps::transcode::FallbackCause;
use rust_video_caps::{h264, h265};
use rust_video_core::{Codec, Gop};

/// Probed driver encode caps behind one profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DriverCaps {
    /// Encode entrypoint offered.
    pub(crate) entrypoint: bool,
    /// `VA_CONFIG_ATTRIB_RT_FORMAT` value, absent when the query fails.
    pub(crate) rt_format: Option<u32>,
    /// `VA_CONFIG_ATTRIB_RATE_CONTROL` bitmask, absent when the query fails.
    pub(crate) rate_control: Option<u32>,
    /// `VA_CONFIG_ATTRIB_ENC_PACKED_HEADERS` bitmask, absent when the query fails.
    pub(crate) packed_headers: Option<u32>,
}

impl DriverCaps {
    /// Caps offering everything the mapping needs.
    #[cfg(test)]
    pub(crate) fn full() -> Self {
        Self {
            entrypoint: true,
            rt_format: Some(ffi::VA_RT_FORMAT_YUV420),
            rate_control: Some(ffi::VA_RC_CQP | ffi::VA_RC_CBR | ffi::VA_RC_VBR),
            packed_headers: Some(
                ffi::VA_ENC_PACKED_HEADER_SEQUENCE
                    | ffi::VA_ENC_PACKED_HEADER_PICTURE
                    | ffi::VA_ENC_PACKED_HEADER_SLICE,
            ),
        }
    }
}

/// Encode config mapped onto one VA-API session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MappedEncode {
    /// VA-API profile behind the session.
    pub(crate) profile: c_int,
    /// Coded size in luma samples, block-aligned.
    pub(crate) coded: (u32, u32),
    /// Engine IDR cadence.
    pub(crate) gop: Gop,
    /// `VA_RC_*` mode behind the session.
    pub(crate) rate_control: u32,
    /// Constant QP behind PPS init and picture QP.
    pub(crate) qp: u8,
    /// Sequence bitrate in bits per second, zero under CQP.
    pub(crate) bitrate: u32,
    /// Wire level idc behind the sets.
    pub(crate) level: u8,
    /// H.264 VUI timing (`num_units_in_tick`, `time_scale`), zeroed under legacy.
    pub(crate) timing: (u32, u32),
}

impl MappedEncode {
    /// Default constant QP behind legacy and bitrate modes.
    const DEFAULT_QP: u8 = 26;
    /// Maximum constant QP.
    const MAX_QP: u8 = 51;

    /// Config mapped for `codec` at `visible` under `caps`.
    ///
    /// `Auto` rate control keeps the tier legacy recipe: CQP 26,
    /// all-intra, the fixed profile, the extent-derived level, zero
    /// bitrate and timing. Explicit modes map every knob; the
    /// unmappable ones decline typed.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a structurally invalid config, a knob the
    /// tier lacks, or a driver gap behind the session.
    pub(crate) fn map(
        codec: Codec,
        visible: PictureSize,
        config: &EncodeConfig,
        caps: &DriverCaps,
    ) -> Result<Self, VaApiError> {
        config.validate()?;
        let family = match (codec, config.profile, config.level) {
            (Codec::H264, Profile::H264(_), Level::H264(_)) => Family::H264,
            (Codec::H265, Profile::H265(_), Level::H265(_)) => Family::H265,
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
        Self::require_driver(codec, caps)?;
        let coded_size = Self::coded(family, visible);
        if config.rate_control == RateControl::Auto {
            return Self::legacy(family, coded_size, caps);
        }
        let rate = Self::rate(caps, config.rate_control)?;
        let gop = Self::gop(config)?;
        let profile = Self::profile(family, config.profile)?;
        let level = Self::level(family, coded_size, config.level)?;
        Self::color(config)?;
        // Preset accepted without a knob: VA-API exposes no speed control.
        Ok(Self {
            profile,
            coded: coded_size,
            gop,
            rate_control: rate.mode,
            qp: rate.qp,
            bitrate: rate.bitrate,
            level,
            timing: Self::timing(config),
        })
    }

    /// Entrypoint, NV12 target, and packed headers behind `codec`.
    fn require_driver(codec: Codec, caps: &DriverCaps) -> Result<(), VaApiError> {
        if !caps.entrypoint {
            return Err(VaApiError::Unsupported(CapsUnsupported::Fallback {
                cause: FallbackCause::EncoderCpuOnly,
            }));
        }
        if caps
            .rt_format
            .is_none_or(|value| value & ffi::VA_RT_FORMAT_YUV420 == 0)
        {
            return Err(Decline::ChromaUnsupported {
                chroma: ChromaSampling::Yuv420,
            }
            .into());
        }
        let need = ffi::VA_ENC_PACKED_HEADER_SEQUENCE
            | ffi::VA_ENC_PACKED_HEADER_PICTURE
            | ffi::VA_ENC_PACKED_HEADER_SLICE;
        if caps.packed_headers.is_none_or(|value| value & need != need) {
            return Err(VaApiError::NoPackedHeaders { codec });
        }
        Ok(())
    }

    /// Legacy recipe: CQP 26, all-intra, fixed profile, derived level.
    fn legacy(family: Family, coded: (u32, u32), caps: &DriverCaps) -> Result<Self, VaApiError> {
        Self::require_mode(caps, ffi::VA_RC_CQP, RateControl::Auto)?;
        Ok(Self {
            profile: family.va_profile(),
            coded,
            gop: Gop::all_intra(),
            rate_control: ffi::VA_RC_CQP,
            qp: Self::DEFAULT_QP,
            bitrate: 0,
            level: Self::auto_level(family, coded),
            timing: (0, 0),
        })
    }

    /// Rate mode, QP, and sequence bitrate behind `mode`.
    fn rate(caps: &DriverCaps, mode: RateControl) -> Result<MappedRate, VaApiError> {
        match mode {
            RateControl::Auto => unreachable!("legacy path maps Auto"),
            RateControl::Cqp { qp } => {
                if qp > Self::MAX_QP {
                    return Err(Decline::RateControlInvalid { mode }.into());
                }
                Self::require_mode(caps, ffi::VA_RC_CQP, mode)?;
                Ok(MappedRate {
                    mode: ffi::VA_RC_CQP,
                    qp,
                    bitrate: 0,
                })
            }
            RateControl::Cbr { bitrate, .. } => {
                Self::require_mode(caps, ffi::VA_RC_CBR, mode)?;
                Ok(MappedRate {
                    mode: ffi::VA_RC_CBR,
                    qp: Self::DEFAULT_QP,
                    bitrate,
                })
            }
            RateControl::Vbr { bitrate, .. } => {
                Self::require_mode(caps, ffi::VA_RC_VBR, mode)?;
                Ok(MappedRate {
                    mode: ffi::VA_RC_VBR,
                    qp: Self::DEFAULT_QP,
                    bitrate,
                })
            }
            RateControl::Crf { .. } => Err(Decline::RateControlUnsupported { mode }.into()),
        }
    }

    /// `bit` present in the driver mask, else a typed decline.
    fn require_mode(caps: &DriverCaps, bit: u32, requested: RateControl) -> Result<(), VaApiError> {
        if caps.rate_control.is_some_and(|value| value & bit != 0) {
            Ok(())
        } else {
            Err(Decline::RateControlUnsupported { mode: requested }.into())
        }
    }

    /// Fixed IDR cadence over `keyint`, B-frames and refs checked.
    fn gop(config: &EncodeConfig) -> Result<Gop, VaApiError> {
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

    /// VA-API profile behind `profile`; the tier writes High and Main only.
    fn profile(family: Family, profile: Profile) -> Result<c_int, VaApiError> {
        match (family, profile) {
            (Family::H264, Profile::H264(h264::EncodeProfile::High)) => {
                Ok(ffi::VA_PROFILE_H264_HIGH)
            }
            (Family::H265, Profile::H265(h265::EncodeProfile::Main)) => {
                Ok(ffi::VA_PROFILE_HEVC_MAIN)
            }
            _ => Err(Decline::ProfileUnsupported { profile }.into()),
        }
    }

    /// Wire level idc: derived under auto, coverage-checked when pinned.
    fn level(family: Family, coded: (u32, u32), level: Level) -> Result<u8, VaApiError> {
        match (family, level) {
            (Family::H264, Level::H264(h264::EncodeLevel::Auto))
            | (Family::H265, Level::H265(h265::EncodeLevel::Auto)) => {
                Ok(Self::auto_level(family, coded))
            }
            (Family::H264, Level::H264(h264::EncodeLevel::Pinned(pinned))) => {
                if pinned == h264::Level::L1b {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                let mbs = u64::from(coded.0.div_ceil(16)) * u64::from(coded.1.div_ceil(16));
                let covers = pinned
                    .caps()
                    .is_some_and(|caps| mbs <= u64::from(caps.fs_mb));
                if !covers {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                Ok(pinned.idc())
            }
            (Family::H265, Level::H265(h265::EncodeLevel::Pinned(pinned))) => {
                let samples = u64::from(coded.0) * u64::from(coded.1);
                let covers = pinned
                    .caps()
                    .is_some_and(|caps| samples <= u64::from(caps.ps));
                if !covers {
                    return Err(Decline::LevelUnsupported { level }.into());
                }
                u8::try_from(pinned.idc()).map_err(|_| Decline::LevelUnsupported { level }.into())
            }
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// Color validated without signalling; the sets carry no VUI color.
    fn color(config: &EncodeConfig) -> Result<(), VaApiError> {
        use rust_video_caps::color::{
            ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
        };

        if config.is_hdr() {
            return Err(Decline::HdrUnsupported.into());
        }
        if !matches!(
            config.color.primaries,
            ColorPrimaries::Bt709 | ColorPrimaries::Unspecified
        ) {
            return Err(Decline::PrimariesUnsupported {
                primaries: config.color.primaries,
            }
            .into());
        }
        if !matches!(
            config.color.transfer,
            TransferCharacteristics::Bt709 | TransferCharacteristics::Unspecified
        ) {
            return Err(Decline::TransferUnsupported {
                transfer: config.color.transfer,
            }
            .into());
        }
        if !matches!(
            config.color.matrix,
            MatrixCoefficients::Bt709 | MatrixCoefficients::Unspecified
        ) {
            return Err(Decline::MatrixUnsupported {
                matrix: config.color.matrix,
            }
            .into());
        }
        if config.color.range != ColorRange::Limited {
            return Err(Decline::RangeUnsupported {
                range: config.color.range,
            }
            .into());
        }
        Ok(())
    }

    /// H.264 VUI timing behind the frame rate.
    fn timing(config: &EncodeConfig) -> (u32, u32) {
        let rate = config.frame_rate;
        (rate.den, rate.num.saturating_mul(2))
    }

    /// Coded size over the family block alignment.
    fn coded(family: Family, visible: PictureSize) -> (u32, u32) {
        let block = match family {
            Family::H264 => 16,
            Family::H265 => 8,
        };
        (
            visible.width().div_ceil(block) * block,
            visible.height().div_ceil(block) * block,
        )
    }

    /// Level idc covering `coded` for `family`.
    fn auto_level(family: Family, coded: (u32, u32)) -> u8 {
        let samples = u64::from(coded.0) * u64::from(coded.1);
        match family {
            Family::H264 => {
                let mbs = samples / 256;
                if mbs <= 396 {
                    22
                } else if mbs <= 1620 {
                    30
                } else if mbs <= 3600 {
                    31
                } else if mbs <= 5120 {
                    32
                } else if mbs <= 8192 {
                    41
                } else if mbs <= 22_080 {
                    50
                } else {
                    51
                }
            }
            Family::H265 => {
                if samples <= 36_864 {
                    30
                } else if samples <= 122_880 {
                    60
                } else if samples <= 245_760 {
                    63
                } else if samples <= 552_960 {
                    90
                } else if samples <= 983_040 {
                    93
                } else if samples <= 2_228_224 {
                    120
                } else if samples <= 8_912_896 {
                    150
                } else {
                    153
                }
            }
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

impl Family {
    /// VA-API profile behind the family.
    fn va_profile(self) -> c_int {
        match self {
            Self::H264 => ffi::VA_PROFILE_H264_HIGH,
            Self::H265 => ffi::VA_PROFILE_HEVC_MAIN,
        }
    }
}

/// Rate mode, QP, and sequence bitrate behind one session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MappedRate {
    /// `VA_RC_*` mode.
    mode: u32,
    /// Constant QP behind PPS init and picture QP.
    qp: u8,
    /// Sequence bitrate in bits per second, zero under CQP.
    bitrate: u32,
}

#[cfg(test)]
mod tests {
    use super::{DriverCaps, MappedEncode};
    use crate::ffi;

    use rust_video_caps::color::{
        ColorConfig, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
    };
    use rust_video_caps::common::{ChromaSampling, PictureSize};
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop as ConfigGop, Level, Profile, Quality, RateControl,
        SpeedPreset,
    };
    use rust_video_caps::reason::UnsupportedReason as CapsUnsupported;
    use rust_video_caps::transcode::FallbackCause;
    use rust_video_caps::{h264, h265};
    use rust_video_core::Codec;

    /// Visible test size.
    fn size(width: u32, height: u32) -> PictureSize {
        PictureSize::new(width, height).expect("nonzero extent")
    }

    /// H.264 High config with `rate`.
    fn h264(rate: RateControl) -> EncodeConfig {
        EncodeConfig {
            rate_control: rate,
            profile: Profile::H264(h264::EncodeProfile::High),
            level: Level::H264(h264::EncodeLevel::Auto),
            ..EncodeConfig::default()
        }
    }

    /// H.265 Main config with `rate`.
    fn h265(rate: RateControl) -> EncodeConfig {
        EncodeConfig {
            rate_control: rate,
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..EncodeConfig::default()
        }
    }

    #[test]
    fn auto_keeps_legacy_recipe() {
        let mapped = MappedEncode::map(
            Codec::H264,
            size(320, 240),
            &h264(RateControl::Auto),
            &DriverCaps::full(),
        )
        .expect("legacy maps");
        assert_eq!(mapped.profile, ffi::VA_PROFILE_H264_HIGH);
        assert_eq!(mapped.coded, (320, 240));
        assert_eq!(mapped.gop, rust_video_core::Gop::all_intra());
        assert_eq!(mapped.rate_control, ffi::VA_RC_CQP);
        assert_eq!(mapped.qp, 26);
        assert_eq!(mapped.bitrate, 0);
        assert_eq!(mapped.level, 22);
        assert_eq!(mapped.timing, (0, 0));
    }

    #[test]
    fn auto_ignores_gop_and_color_requests() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            gop: ConfigGop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
            ..h264(RateControl::Auto)
        };
        let mapped = MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
            .expect("legacy maps");
        assert_eq!(mapped.gop.period(), 1);
        assert_eq!(mapped.qp, 26);
    }

    #[test]
    fn auto_requires_cqp_bit() {
        let caps = DriverCaps {
            rate_control: Some(ffi::VA_RC_CBR),
            ..DriverCaps::full()
        };
        assert_eq!(
            MappedEncode::map(Codec::H264, size(320, 240), &h264(RateControl::Auto), &caps)
                .unwrap_err()
                .to_string(),
            "encode declined: rate control Auto unsupported"
        );
    }

    #[test]
    fn cqp_maps_qp_and_gop() {
        let config = EncodeConfig {
            gop: ConfigGop {
                keyint: 30,
                min_keyint: 30,
                b_frames: 0,
                refs: 1,
            },
            ..h264(RateControl::Cqp { qp: 30 })
        };
        let mapped = MappedEncode::map(Codec::H264, size(640, 480), &config, &DriverCaps::full())
            .expect("cqp maps");
        assert_eq!(mapped.rate_control, ffi::VA_RC_CQP);
        assert_eq!(mapped.qp, 30);
        assert_eq!(mapped.bitrate, 0);
        assert_eq!(mapped.gop.period(), 30);
        assert_eq!(mapped.timing, (1, 60));
    }

    #[test]
    fn cqp_rejects_qp_above_51() {
        let mode = RateControl::Cqp { qp: 52 };
        assert!(matches!(
            MappedEncode::map(
                Codec::H264,
                size(320, 240),
                &h264(mode),
                &DriverCaps::full()
            ),
            Err(crate::error::VaApiError::Declined(
                Decline::RateControlInvalid { .. }
            ))
        ));
    }

    #[test]
    fn cbr_maps_bitrate_behind_driver_bit() {
        let mode = RateControl::Cbr {
            bitrate: 2_000_000,
            maxrate: 2_000_000,
            bufsize: 2_000_000,
        };
        let mapped = MappedEncode::map(
            Codec::H264,
            size(320, 240),
            &h264(mode),
            &DriverCaps::full(),
        )
        .expect("cbr maps");
        assert_eq!(mapped.rate_control, ffi::VA_RC_CBR);
        assert_eq!(mapped.bitrate, 2_000_000);
        assert_eq!(mapped.qp, 26);
        let caps = DriverCaps {
            rate_control: Some(ffi::VA_RC_CQP),
            ..DriverCaps::full()
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &h264(mode), &caps),
            Err(crate::error::VaApiError::Declined(
                Decline::RateControlUnsupported { .. }
            ))
        ));
    }

    #[test]
    fn vbr_maps_bitrate_behind_driver_bit() {
        let mode = RateControl::Vbr {
            bitrate: 3_000_000,
            maxrate: 6_000_000,
        };
        let mapped = MappedEncode::map(
            Codec::H265,
            size(320, 240),
            &h265(mode),
            &DriverCaps::full(),
        )
        .expect("vbr maps");
        assert_eq!(mapped.rate_control, ffi::VA_RC_VBR);
        assert_eq!(mapped.bitrate, 3_000_000);
        let caps = DriverCaps {
            rate_control: Some(ffi::VA_RC_CQP),
            ..DriverCaps::full()
        };
        assert!(matches!(
            MappedEncode::map(Codec::H265, size(320, 240), &h265(mode), &caps),
            Err(crate::error::VaApiError::Declined(
                Decline::RateControlUnsupported { .. }
            ))
        ));
    }

    #[test]
    fn crf_declines() {
        let mode = RateControl::Crf { value: 23 };
        assert!(matches!(
            MappedEncode::map(
                Codec::H264,
                size(320, 240),
                &h264(mode),
                &DriverCaps::full()
            ),
            Err(crate::error::VaApiError::Declined(
                Decline::RateControlUnsupported { .. }
            ))
        ));
    }

    #[test]
    fn gop_declines_b_frames_refs_and_ref_zero() {
        for gop in [
            ConfigGop {
                keyint: 30,
                min_keyint: 30,
                b_frames: 2,
                refs: 1,
            },
            ConfigGop {
                keyint: 30,
                min_keyint: 30,
                b_frames: 0,
                refs: 2,
            },
            ConfigGop {
                keyint: 30,
                min_keyint: 30,
                b_frames: 0,
                refs: 0,
            },
        ] {
            let config = EncodeConfig {
                gop,
                ..h264(RateControl::Cqp { qp: 26 })
            };
            assert!(
                MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
                    .is_err(),
                "gop {gop:?} declines"
            );
        }
    }

    #[test]
    fn profile_accepts_high_and_main_only() {
        let mapped = MappedEncode::map(
            Codec::H264,
            size(320, 240),
            &h264(RateControl::Cqp { qp: 26 }),
            &DriverCaps::full(),
        )
        .expect("high maps");
        assert_eq!(mapped.profile, ffi::VA_PROFILE_H264_HIGH);
        let mapped = MappedEncode::map(
            Codec::H265,
            size(320, 240),
            &h265(RateControl::Cqp { qp: 26 }),
            &DriverCaps::full(),
        )
        .expect("main maps");
        assert_eq!(mapped.profile, ffi::VA_PROFILE_HEVC_MAIN);
        for profile in [
            Profile::H264(h264::EncodeProfile::Baseline),
            Profile::H264(h264::EncodeProfile::Main),
            Profile::H264(h264::EncodeProfile::High10),
        ] {
            let config = EncodeConfig {
                profile,
                ..h264(RateControl::Cqp { qp: 26 })
            };
            assert!(matches!(
                MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
                Err(crate::error::VaApiError::Declined(
                    Decline::ProfileUnsupported { .. }
                ))
            ));
        }
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main10),
            level: Level::H265(h265::EncodeLevel::Auto),
            bit_depth: 10,
            ..h265(RateControl::Cqp { qp: 26 })
        };
        assert!(
            MappedEncode::map(Codec::H265, size(320, 240), &config, &DriverCaps::full()).is_err()
        );
    }

    #[test]
    fn level_auto_derives_and_pinned_checks_coverage() {
        let mapped = MappedEncode::map(
            Codec::H264,
            size(1920, 1080),
            &h264(RateControl::Cqp { qp: 26 }),
            &DriverCaps::full(),
        )
        .expect("auto maps");
        assert_eq!(mapped.level, 41);
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L41)),
            ..h264(RateControl::Cqp { qp: 26 })
        };
        let mapped = MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
            .expect("covering pin maps");
        assert_eq!(mapped.level, 41);
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1)),
            ..h264(RateControl::Cqp { qp: 26 })
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(1920, 1080), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::LevelUnsupported { .. }
            ))
        ));
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1b)),
            ..h264(RateControl::Cqp { qp: 26 })
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::LevelUnsupported { .. }
            ))
        ));
    }

    #[test]
    fn depth_and_chroma_decline() {
        let config = EncodeConfig {
            bit_depth: 10,
            profile: Profile::H264(h264::EncodeProfile::High10),
            ..h264(RateControl::Cqp { qp: 26 })
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::BitDepthUnsupported { .. }
            ))
        ));
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..h264(RateControl::Cqp { qp: 26 })
        };
        assert!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()).is_err()
        );
    }

    #[test]
    fn color_accepts_default_and_unspecified_only() {
        let config = h264(RateControl::Cqp { qp: 26 });
        MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
            .expect("default color maps");
        let config = EncodeConfig {
            color: ColorConfig {
                primaries: ColorPrimaries::Unspecified,
                transfer: TransferCharacteristics::Unspecified,
                matrix: MatrixCoefficients::Unspecified,
                ..ColorConfig::default()
            },
            ..h264(RateControl::Cqp { qp: 26 })
        };
        MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
            .expect("unspecified maps");
        for color in [
            ColorConfig {
                primaries: ColorPrimaries::Bt2020,
                ..ColorConfig::default()
            },
            ColorConfig {
                transfer: TransferCharacteristics::Hlg,
                ..ColorConfig::default()
            },
            ColorConfig {
                matrix: MatrixCoefficients::Smpte170M,
                ..ColorConfig::default()
            },
            ColorConfig {
                range: ColorRange::Full,
                ..ColorConfig::default()
            },
        ] {
            let config = EncodeConfig {
                color,
                ..h264(RateControl::Cqp { qp: 26 })
            };
            assert!(
                MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
                    .is_err(),
                "color {color:?} declines"
            );
        }
    }

    #[test]
    fn presets_all_map() {
        for preset in [
            SpeedPreset::Realtime,
            SpeedPreset::Balanced,
            SpeedPreset::Quality,
        ] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..h264(RateControl::Cqp { qp: 26 })
            };
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
                .expect("preset maps");
        }
    }

    #[test]
    fn timing_maps_frame_rate() {
        let config = EncodeConfig {
            frame_rate: FrameRate { num: 60, den: 1 },
            ..h264(RateControl::Cqp { qp: 26 })
        };
        let mapped = MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full())
            .expect("timing maps");
        assert_eq!(mapped.timing, (1, 120));
    }

    #[test]
    fn coded_aligns_to_block() {
        let mapped = MappedEncode::map(
            Codec::H264,
            size(321, 241),
            &h264(RateControl::Cqp { qp: 26 }),
            &DriverCaps::full(),
        )
        .expect("aligns");
        assert_eq!(mapped.coded, (336, 256));
        let mapped = MappedEncode::map(
            Codec::H265,
            size(321, 241),
            &h265(RateControl::Cqp { qp: 26 }),
            &DriverCaps::full(),
        )
        .expect("aligns");
        assert_eq!(mapped.coded, (328, 248));
    }

    #[test]
    fn driver_gaps_decline_typed() {
        let caps = DriverCaps {
            entrypoint: false,
            ..DriverCaps::full()
        };
        assert_eq!(
            MappedEncode::map(Codec::H264, size(320, 240), &h264(RateControl::Auto), &caps)
                .unwrap_err()
                .to_string(),
            "unsupported stream: encoder without gpu import"
        );
        let caps = DriverCaps {
            rt_format: Some(0),
            ..DriverCaps::full()
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &h264(RateControl::Auto), &caps),
            Err(crate::error::VaApiError::Declined(
                Decline::ChromaUnsupported { .. }
            ))
        ));
        let caps = DriverCaps {
            packed_headers: Some(0),
            ..DriverCaps::full()
        };
        assert_eq!(
            MappedEncode::map(Codec::H264, size(320, 240), &h264(RateControl::Auto), &caps)
                .unwrap_err()
                .to_string(),
            "h264: no packed headers"
        );
    }

    #[test]
    fn codec_mismatch_declines() {
        let config = h265(RateControl::Cqp { qp: 26 });
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::ProfileUnsupported { .. }
            ))
        ));
        let mut config = h264(RateControl::Cqp { qp: 26 });
        config.profile = Profile::Vp9(rust_video_caps::vp9::EncodeProfile::P0);
        config.level = Level::Vp9(rust_video_caps::vp9::EncodeLevel::Auto);
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::ProfileUnsupported { .. }
            ))
        ));
    }

    #[test]
    fn invalid_config_passthrough() {
        let config = EncodeConfig {
            gop: ConfigGop {
                keyint: 0,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
            ..h264(RateControl::Cqp { qp: 26 })
        };
        assert!(matches!(
            MappedEncode::map(Codec::H264, size(320, 240), &config, &DriverCaps::full()),
            Err(crate::error::VaApiError::Declined(
                Decline::GopInvalid { .. }
            ))
        ));
    }

    #[test]
    fn fallback_wraps_cause() {
        let error = crate::error::VaApiError::Unsupported(CapsUnsupported::Fallback {
            cause: FallbackCause::DeviceMismatch,
        });
        assert_eq!(
            error.to_string(),
            "unsupported stream: decode and encode devices differ"
        );
    }
}
