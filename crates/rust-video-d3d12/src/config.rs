//! Encode configuration mapping: shared config to session values.
//!
//! Pure mapping behind the encoder; the session applies it.

use rust_video_caps::color::{ColorConfig, MatrixCoefficients};
use rust_video_caps::common::ChromaSampling;
use rust_video_caps::encode::{
    Decline, EncodeConfig, FrameRate, Gop, Level, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::{h264, h265};

use rust_video_core::Codec;

/// Session profile behind the encoder, heap, and headers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MappedProfile {
    /// H.264 Main profile.
    H264Main,
    /// H.264 High profile.
    H264High,
    /// HEVC Main profile.
    HevcMain,
}

impl MappedProfile {
    /// Codec the profile encodes.
    pub(crate) const fn codec(self) -> Codec {
        match self {
            Self::H264Main | Self::H264High => Codec::H264,
            Self::HevcMain => Codec::H265,
        }
    }
}

/// Session level behind the heap and headers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MappedLevel {
    /// Tier derives the level from the coded extent.
    Auto,
    /// Pinned level idc.
    Pinned {
        /// Pinned level idc.
        idc: u8,
    },
}

/// Session rate control behind the sequence control.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MappedRate {
    /// Constant quantizer.
    Cqp {
        /// Quantization parameter.
        qp: u32,
    },
    /// Constant bitrate.
    Cbr {
        /// Starting quantizer.
        initial_qp: u32,
        /// Quantizer floor.
        min_qp: u32,
        /// Quantizer ceiling.
        max_qp: u32,
        /// Single-frame bit cap.
        max_frame_bits: u64,
        /// Target bitrate in bps.
        target_bitrate: u64,
        /// VBV capacity in bits.
        vbv_capacity: u64,
        /// VBV fullness in bits.
        vbv_fullness: u64,
    },
    /// Variable bitrate.
    Vbr {
        /// Starting quantizer.
        initial_qp: u32,
        /// Quantizer floor.
        min_qp: u32,
        /// Quantizer ceiling.
        max_qp: u32,
        /// Single-frame bit cap.
        max_frame_bits: u64,
        /// Target average bitrate in bps.
        target_avg: u64,
        /// Peak bitrate in bps.
        peak: u64,
        /// VBV capacity in bits.
        vbv_capacity: u64,
        /// VBV fullness in bits.
        vbv_fullness: u64,
    },
}

impl MappedRate {
    /// Legacy constant quantizer.
    pub(crate) const LEGACY_QP: u32 = 26;
    /// Open quantizer floor.
    pub(crate) const MIN_QP: u32 = 0;
    /// Codec quantizer ceiling.
    pub(crate) const MAX_QP: u32 = 51;
}

/// Mapped session configuration behind the encoder.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MappedConfig {
    /// Session profile.
    pub(crate) profile: MappedProfile,
    /// Session level.
    pub(crate) level: MappedLevel,
    /// Session rate control.
    pub(crate) rate: MappedRate,
    /// Speed preset; Quality selects the HEVC High tier at L4 or up.
    pub(crate) preset: SpeedPreset,
    /// Target frame rate.
    pub(crate) frame_rate: FrameRate,
    /// VUI color, absent for the legacy header bytes.
    pub(crate) color: Option<ColorConfig>,
}

impl MappedConfig {
    /// Legacy H.264 session values: High profile, auto level, CQP 26.
    #[cfg(test)]
    pub(crate) const LEGACY_H264: Self = Self {
        profile: MappedProfile::H264High,
        level: MappedLevel::Auto,
        rate: MappedRate::Cqp {
            qp: MappedRate::LEGACY_QP,
        },
        preset: SpeedPreset::Realtime,
        frame_rate: FrameRate { num: 30, den: 1 },
        color: None,
    };
    /// Legacy HEVC session values: Main profile, auto level, CQP 26.
    #[cfg(test)]
    pub(crate) const LEGACY_HEVC: Self = Self {
        profile: MappedProfile::HevcMain,
        level: MappedLevel::Auto,
        rate: MappedRate::Cqp {
            qp: MappedRate::LEGACY_QP,
        },
        preset: SpeedPreset::Realtime,
        frame_rate: FrameRate { num: 30, den: 1 },
        color: None,
    };

    /// Session values for `config`.
    ///
    /// Auto rate control pins the legacy recipe: CQP 26, all-intra, 30 fps,
    /// Main tier, headers without VUI; the GOP, preset, frame rate, and color
    /// ride ignored. Explicit modes map every field and decline the rest
    /// typed.
    ///
    /// # Errors
    ///
    /// Returns the [`Decline`] when `config` is structurally invalid or
    /// exceeds the tier.
    pub(crate) fn map(config: EncodeConfig) -> Result<Self, Decline> {
        config.validate()?;
        let profile = Self::map_profile(config.profile)?;
        let level = Self::map_level(config.level)?;
        Self::map_depth_chroma(config.bit_depth, config.chroma)?;
        let legacy = matches!(config.rate_control, RateControl::Auto);
        if !legacy {
            Self::map_gop(config.gop)?;
        }
        let rate = Self::map_rate(config.rate_control)?;
        let preset = if legacy {
            SpeedPreset::Realtime
        } else {
            config.quality.preset
        };
        let frame_rate = if legacy {
            FrameRate { num: 30, den: 1 }
        } else {
            config.frame_rate
        };
        let color = if legacy {
            None
        } else {
            Some(Self::map_color(config.color)?)
        };
        Ok(Self {
            profile,
            level,
            rate,
            preset,
            frame_rate,
            color,
        })
    }

    /// True for High-tier HEVC: Quality preset at `level_idc` L4 or up.
    pub(crate) fn high_tier(&self, level_idc: u8) -> bool {
        matches!(self.profile, MappedProfile::HevcMain)
            && self.preset == SpeedPreset::Quality
            && level_idc >= 120
    }

    /// Session profile for `profile`.
    fn map_profile(profile: Profile) -> Result<MappedProfile, Decline> {
        match profile {
            Profile::H264(h264::EncodeProfile::Main) => Ok(MappedProfile::H264Main),
            Profile::H264(h264::EncodeProfile::High) => Ok(MappedProfile::H264High),
            Profile::H265(h265::EncodeProfile::Main) => Ok(MappedProfile::HevcMain),
            unsupported => Err(Decline::ProfileUnsupported {
                profile: unsupported,
            }),
        }
    }

    /// Session level for `level`.
    fn map_level(level: Level) -> Result<MappedLevel, Decline> {
        match level {
            Level::H264(h264::EncodeLevel::Auto) | Level::H265(h265::EncodeLevel::Auto) => {
                Ok(MappedLevel::Auto)
            }
            Level::H264(h264::EncodeLevel::Pinned(pinned)) => {
                Ok(MappedLevel::Pinned { idc: pinned.idc() })
            }
            Level::H265(h265::EncodeLevel::Pinned(pinned)) => {
                let idc = match pinned {
                    h265::Level::L1 => 30,
                    h265::Level::L2 => 60,
                    h265::Level::L21 => 63,
                    h265::Level::L3 => 90,
                    h265::Level::L31 => 93,
                    h265::Level::L4 => 120,
                    h265::Level::L41 => 123,
                    h265::Level::L5 => 150,
                    h265::Level::L51 => 153,
                    h265::Level::L52 => 156,
                    h265::Level::L6 => 180,
                    h265::Level::L61 => 183,
                    h265::Level::L62 => 186,
                    h265::Level::L63 | h265::Level::L7 | h265::Level::L71 | h265::Level::L72 => {
                        return Err(Decline::LevelUnsupported { level })
                    }
                };
                Ok(MappedLevel::Pinned { idc })
            }
            unsupported => Err(Decline::LevelUnsupported { level: unsupported }),
        }
    }

    /// Tier depth and chroma behind `depth` and `chroma`.
    fn map_depth_chroma(depth: u8, chroma: ChromaSampling) -> Result<(), Decline> {
        if depth != 8 {
            return Err(Decline::BitDepthUnsupported {
                actual: depth,
                max: 8,
            });
        }
        if chroma != ChromaSampling::Yuv420 {
            return Err(Decline::ChromaUnsupported { chroma });
        }
        Ok(())
    }

    /// All-intra GOP behind `gop`.
    ///
    /// The tier emits IDR every picture, so `keyint` reads as a satisfied
    /// maximum while `min_keyint` above 1, B-frames, and references above
    /// the single-ref budget decline.
    fn map_gop(gop: Gop) -> Result<(), Decline> {
        if gop.b_frames > 0 {
            return Err(Decline::BFramesUnsupported {
                actual: gop.b_frames,
                max: 0,
            });
        }
        if gop.refs > 1 {
            return Err(Decline::RefsUnsupported {
                actual: gop.refs,
                max: 1,
            });
        }
        if gop.min_keyint > 1 {
            return Err(Decline::GopInvalid { gop });
        }
        Ok(())
    }

    /// Session rate control for `mode`.
    fn map_rate(mode: RateControl) -> Result<MappedRate, Decline> {
        match mode {
            RateControl::Auto => Ok(MappedRate::Cqp {
                qp: MappedRate::LEGACY_QP,
            }),
            RateControl::Cqp { qp } => {
                if qp > 51 {
                    return Err(Decline::RateControlInvalid { mode });
                }
                Ok(MappedRate::Cqp { qp: u32::from(qp) })
            }
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => Ok(MappedRate::Cbr {
                initial_qp: MappedRate::LEGACY_QP,
                min_qp: MappedRate::MIN_QP,
                max_qp: MappedRate::MAX_QP,
                max_frame_bits: u64::from(maxrate),
                target_bitrate: u64::from(bitrate),
                vbv_capacity: u64::from(bufsize),
                vbv_fullness: u64::from(bufsize),
            }),
            RateControl::Vbr { bitrate, maxrate } => Ok(MappedRate::Vbr {
                initial_qp: MappedRate::LEGACY_QP,
                min_qp: MappedRate::MIN_QP,
                max_qp: MappedRate::MAX_QP,
                max_frame_bits: u64::from(maxrate),
                target_avg: u64::from(bitrate),
                peak: u64::from(maxrate),
                vbv_capacity: u64::from(maxrate),
                vbv_fullness: u64::from(maxrate),
            }),
            RateControl::Crf { .. } => Err(Decline::RateControlUnsupported { mode }),
        }
    }

    /// VUI color for `color`.
    fn map_color(color: ColorConfig) -> Result<ColorConfig, Decline> {
        if color.is_hdr() {
            return Err(Decline::HdrUnsupported);
        }
        if matches!(
            color.matrix,
            MatrixCoefficients::Gbr | MatrixCoefficients::Ycgco
        ) {
            return Err(Decline::MatrixUnsupported {
                matrix: color.matrix,
            });
        }
        Ok(color)
    }
}

#[cfg(test)]
mod tests {
    use rust_video_caps::color::{
        ColorConfig, ContentLight, MatrixCoefficients, TransferCharacteristics,
    };
    use rust_video_caps::common::ChromaSampling;
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::{av1, h264, h265, vp9};
    use rust_video_core::Codec;

    use super::{MappedConfig, MappedLevel, MappedProfile, MappedRate};

    /// All-intra GOP: keyframe every picture.
    fn intra_gop() -> Gop {
        Gop {
            keyint: 1,
            min_keyint: 1,
            b_frames: 0,
            refs: 1,
        }
    }

    /// Explicit CQP config over H.264 Main auto.
    fn explicit() -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cqp { qp: 26 },
            gop: intra_gop(),
            ..Default::default()
        }
    }

    #[test]
    fn auto_pins_legacy_recipe() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            gop: Gop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 2,
                refs: 4,
            },
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            color: ColorConfig {
                light: Some(ContentLight {
                    max_cll: 1000,
                    max_fall: 400,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let mapped = MappedConfig::map(config).expect("auto ignores knobs");
        assert_eq!(mapped.rate, MappedRate::Cqp { qp: 26 });
        assert_eq!(mapped.level, MappedLevel::Auto);
        assert_eq!(mapped.preset, SpeedPreset::Realtime);
        assert_eq!(mapped.frame_rate, FrameRate { num: 30, den: 1 });
        assert_eq!(mapped.color, None);
        assert_eq!(mapped.profile.codec(), Codec::H264);
    }

    #[test]
    fn auto_matches_legacy_consts() {
        let h264 = EncodeConfig {
            rate_control: RateControl::Auto,
            profile: Profile::H264(h264::EncodeProfile::High),
            ..Default::default()
        };
        assert_eq!(
            MappedConfig::map(h264).expect("auto h264"),
            MappedConfig::LEGACY_H264
        );
        let hevc = EncodeConfig {
            rate_control: RateControl::Auto,
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..Default::default()
        };
        assert_eq!(
            MappedConfig::map(hevc).expect("auto hevc"),
            MappedConfig::LEGACY_HEVC
        );
    }

    #[test]
    fn auto_keeps_structural_validation() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            gop: Gop {
                keyint: 0,
                ..intra_gop()
            },
            ..Default::default()
        };
        assert!(matches!(
            MappedConfig::map(config),
            Err(Decline::GopInvalid { .. })
        ));
    }

    #[test]
    fn cqp_maps_quantizer() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            frame_rate: FrameRate { num: 24, den: 1 },
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("cqp maps");
        assert_eq!(mapped.rate, MappedRate::Cqp { qp: 23 });
        assert_eq!(mapped.frame_rate, FrameRate { num: 24, den: 1 });
        assert_eq!(mapped.color, Some(ColorConfig::default()));
    }

    #[test]
    fn cqp_above_51_declines() {
        let mode = RateControl::Cqp { qp: 52 };
        let config = EncodeConfig {
            rate_control: mode,
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::RateControlInvalid { mode })
        );
    }

    #[test]
    fn cbr_maps_budget() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 2_000_000,
                maxrate: 4_000_000,
                bufsize: 4_000_000,
            },
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("cbr maps");
        assert_eq!(
            mapped.rate,
            MappedRate::Cbr {
                initial_qp: 26,
                min_qp: 0,
                max_qp: 51,
                max_frame_bits: 4_000_000,
                target_bitrate: 2_000_000,
                vbv_capacity: 4_000_000,
                vbv_fullness: 4_000_000,
            }
        );
    }

    #[test]
    fn vbr_maps_budget() {
        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 2_000_000,
                maxrate: 4_000_000,
            },
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("vbr maps");
        assert_eq!(
            mapped.rate,
            MappedRate::Vbr {
                initial_qp: 26,
                min_qp: 0,
                max_qp: 51,
                max_frame_bits: 4_000_000,
                target_avg: 2_000_000,
                peak: 4_000_000,
                vbv_capacity: 4_000_000,
                vbv_fullness: 4_000_000,
            }
        );
    }

    #[test]
    fn crf_declines() {
        let mode = RateControl::Crf { value: 23 };
        let config = EncodeConfig {
            rate_control: mode,
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::RateControlUnsupported { mode })
        );
    }

    #[test]
    fn gop_b_frames_decline() {
        let config = EncodeConfig {
            gop: Gop {
                b_frames: 1,
                ..intra_gop()
            },
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::BFramesUnsupported { actual: 1, max: 0 })
        );
    }

    #[test]
    fn gop_refs_decline() {
        let config = EncodeConfig {
            gop: Gop {
                refs: 2,
                ..intra_gop()
            },
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::RefsUnsupported { actual: 2, max: 1 })
        );
    }

    #[test]
    fn gop_min_keyint_declines() {
        let gop = Gop {
            keyint: 60,
            min_keyint: 60,
            ..intra_gop()
        };
        let config = EncodeConfig { gop, ..explicit() };
        assert_eq!(MappedConfig::map(config), Err(Decline::GopInvalid { gop }));
    }

    #[test]
    fn gop_keyint_bound_accepted() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 60,
                min_keyint: 1,
                ..intra_gop()
            },
            ..explicit()
        };
        MappedConfig::map(config).expect("keyint is a satisfied maximum");
    }

    #[test]
    fn profiles_map_or_decline() {
        let main = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::Main),
            ..explicit()
        };
        let mapped = MappedConfig::map(main).expect("main maps");
        assert_eq!(mapped.profile, MappedProfile::H264Main);
        assert_eq!(mapped.profile.codec(), Codec::H264);
        let high = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::High),
            ..explicit()
        };
        let mapped = MappedConfig::map(high).expect("high maps");
        assert_eq!(mapped.profile, MappedProfile::H264High);
        let hevc = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..explicit()
        };
        let mapped = MappedConfig::map(hevc).expect("hevc main maps");
        assert_eq!(mapped.profile, MappedProfile::HevcMain);
        assert_eq!(mapped.profile.codec(), Codec::H265);
        for profile in [
            Profile::H264(h264::EncodeProfile::Baseline),
            Profile::H264(h264::EncodeProfile::High10),
            Profile::H265(h265::EncodeProfile::Main10),
        ] {
            let config = EncodeConfig {
                profile,
                level: if profile.codec() == Codec::H264 {
                    Level::H264(h264::EncodeLevel::Auto)
                } else {
                    Level::H265(h265::EncodeLevel::Auto)
                },
                ..explicit()
            };
            assert_eq!(
                MappedConfig::map(config),
                Err(Decline::ProfileUnsupported { profile })
            );
        }
        let av1 = Profile::Av1(av1::EncodeProfile::Main);
        let config = EncodeConfig {
            profile: av1,
            level: Level::Av1(av1::EncodeLevel::Auto),
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::ProfileUnsupported { profile: av1 })
        );
        let vp9 = Profile::Vp9(vp9::EncodeProfile::P0);
        let config = EncodeConfig {
            profile: vp9,
            level: Level::Vp9(vp9::EncodeLevel::Auto),
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::ProfileUnsupported { profile: vp9 })
        );
    }

    #[test]
    fn levels_map_or_decline() {
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L31)),
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("pinned h264 maps");
        assert_eq!(mapped.level, MappedLevel::Pinned { idc: 31 });
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L1b)),
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("1b conflates to 11");
        assert_eq!(mapped.level, MappedLevel::Pinned { idc: 11 });
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Pinned(h265::Level::L41)),
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("pinned hevc maps");
        assert_eq!(mapped.level, MappedLevel::Pinned { idc: 123 });
        for pinned in [h265::Level::L63, h265::Level::L7] {
            let level = Level::H265(h265::EncodeLevel::Pinned(pinned));
            let config = EncodeConfig {
                profile: Profile::H265(h265::EncodeProfile::Main),
                level,
                ..explicit()
            };
            assert_eq!(
                MappedConfig::map(config),
                Err(Decline::LevelUnsupported { level })
            );
        }
    }

    #[test]
    fn depth_chroma_decline() {
        let profile = Profile::H264(h264::EncodeProfile::High10);
        let config = EncodeConfig {
            profile,
            bit_depth: 10,
            ..explicit()
        };
        assert_eq!(
            MappedConfig::map(config),
            Err(Decline::ProfileUnsupported { profile })
        );
        assert_eq!(
            MappedConfig::map_depth_chroma(10, ChromaSampling::Yuv420),
            Err(Decline::BitDepthUnsupported { actual: 10, max: 8 })
        );
        assert_eq!(
            MappedConfig::map_depth_chroma(8, ChromaSampling::Yuv422),
            Err(Decline::ChromaUnsupported {
                chroma: ChromaSampling::Yuv422,
            })
        );
    }

    #[test]
    fn color_hdr_declines() {
        let config = EncodeConfig {
            color: ColorConfig {
                light: Some(ContentLight {
                    max_cll: 1000,
                    max_fall: 400,
                }),
                ..Default::default()
            },
            ..explicit()
        };
        assert_eq!(MappedConfig::map(config), Err(Decline::HdrUnsupported));
    }

    #[test]
    fn color_matrix_decline() {
        for matrix in [MatrixCoefficients::Gbr, MatrixCoefficients::Ycgco] {
            let config = EncodeConfig {
                color: ColorConfig {
                    matrix,
                    ..Default::default()
                },
                ..explicit()
            };
            assert_eq!(
                MappedConfig::map(config),
                Err(Decline::MatrixUnsupported { matrix })
            );
        }
    }

    #[test]
    fn color_pq_without_metadata_accepted() {
        let color = ColorConfig {
            transfer: TransferCharacteristics::SmpteSt2084,
            ..Default::default()
        };
        let config = EncodeConfig {
            color,
            ..explicit()
        };
        let mapped = MappedConfig::map(config).expect("pq signals in vui");
        assert_eq!(mapped.color, Some(color));
    }

    #[test]
    fn high_tier_rule() {
        let quality = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..explicit()
        };
        let mapped = MappedConfig::map(quality).expect("quality maps");
        assert_eq!(mapped.preset, SpeedPreset::Quality);
        assert!(mapped.high_tier(120));
        assert!(!mapped.high_tier(93));
        let mapped = MappedConfig::map(explicit()).expect("explicit maps");
        assert!(!mapped.high_tier(150));
    }
}
