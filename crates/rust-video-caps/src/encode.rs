//! Shared encode configuration: rate, structure, quality, profile, level.
//!
//! Codec-agnostic vocabulary over the per-codec profiles and levels in
//! [`h264`], [`h265`], [`av1`], and [`vp9`];
//! VP8 carries an implicit profile and level.

use std::fmt;

use crate::av1;
use crate::color::{
    ColorConfig, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
};
use crate::common::ChromaSampling;
use crate::h264;
use crate::h265;
use crate::vp9;

pub use rust_video_core::Codec;

/// Encode profile: per-codec profile plus the VP8 implicit profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// H.264 encode profile.
    H264(h264::EncodeProfile),
    /// H.265 encode profile.
    H265(h265::EncodeProfile),
    /// AV1 encode profile.
    Av1(av1::EncodeProfile),
    /// VP9 encode profile.
    Vp9(vp9::EncodeProfile),
    /// VP8 implicit profile: 8-bit 4:2:0 only.
    Vp8,
}

impl Profile {
    /// Codec family.
    #[must_use]
    pub fn codec(self) -> Codec {
        match self {
            Self::H264(_) => Codec::H264,
            Self::H265(_) => Codec::H265,
            Self::Av1(_) => Codec::Av1,
            Self::Vp9(_) => Codec::Vp9,
            Self::Vp8 => Codec::Vp8,
        }
    }

    /// Maximum bit depth.
    #[must_use]
    pub fn max_bit_depth(self) -> u8 {
        match self {
            Self::H264(profile) => profile.max_bit_depth(),
            Self::H265(profile) => profile.max_bit_depth(),
            Self::Av1(profile) => profile.max_bit_depth(),
            Self::Vp9(profile) => profile.max_bit_depth(),
            Self::Vp8 => 8,
        }
    }

    /// Depth and chroma allowed together.
    #[must_use]
    pub fn allows(self, depth: u8, chroma: ChromaSampling) -> bool {
        match self {
            Self::H264(profile) => profile.allows(depth, chroma),
            Self::H265(profile) => profile.allows(depth, chroma),
            Self::Av1(profile) => profile.allows(depth, chroma),
            Self::Vp9(profile) => profile.allows(depth, chroma),
            Self::Vp8 => depth == 8 && chroma == ChromaSampling::Yuv420,
        }
    }
}

impl Default for Profile {
    /// H.264 Main: universal realtime baseline.
    fn default() -> Self {
        Self::H264(h264::EncodeProfile::Main)
    }
}

/// Encode level: per-codec level plus the VP8 implicit level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// H.264 encode level.
    H264(h264::EncodeLevel),
    /// H.265 encode level.
    H265(h265::EncodeLevel),
    /// AV1 encode level.
    Av1(av1::EncodeLevel),
    /// VP9 encode level.
    Vp9(vp9::EncodeLevel),
    /// VP8 implicit level: dimension cap only.
    Vp8,
}

impl Level {
    /// Codec family.
    #[must_use]
    pub fn codec(self) -> Codec {
        match self {
            Self::H264(_) => Codec::H264,
            Self::H265(_) => Codec::H265,
            Self::Av1(_) => Codec::Av1,
            Self::Vp9(_) => Codec::Vp9,
            Self::Vp8 => Codec::Vp8,
        }
    }

    /// True for tier-picked levels.
    #[must_use]
    pub fn is_auto(self) -> bool {
        match self {
            Self::H264(level) => level.level().is_none(),
            Self::H265(level) => level.level().is_none(),
            Self::Av1(level) => level.level().is_none(),
            Self::Vp9(level) => level.level().is_none(),
            Self::Vp8 => true,
        }
    }
}

impl Default for Level {
    /// H.264 auto: tier picks from size and rate.
    fn default() -> Self {
        Self::H264(h264::EncodeLevel::Auto)
    }
}

/// Rate control mode: all quantities are integers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RateControl {
    /// Tier legacy recipe: keep the tier's current rate settings.
    Auto,
    /// Constant quantization parameter.
    Cqp {
        /// Quantization parameter.
        qp: u8,
    },
    /// Constant bitrate with a VBV cap and buffer.
    Cbr {
        /// Target bitrate in bps.
        bitrate: u32,
        /// Maximum bitrate in bps.
        maxrate: u32,
        /// Buffer size in bits.
        bufsize: u32,
    },
    /// Variable bitrate with a peak cap.
    Vbr {
        /// Target bitrate in bps.
        bitrate: u32,
        /// Maximum bitrate in bps.
        maxrate: u32,
    },
    /// Constant rate factor.
    Crf {
        /// Rate factor value.
        value: u8,
    },
}

impl Default for RateControl {
    /// Capped CBR: 5 Mbps target and cap, 1s buffer.
    fn default() -> Self {
        Self::Cbr {
            bitrate: 5_000_000,
            maxrate: 5_000_000,
            bufsize: 5_000_000,
        }
    }
}

/// Group of pictures: keyframe cadence plus prediction budget.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Gop {
    /// Maximum keyframe interval in frames.
    pub keyint: u32,
    /// Minimum keyframe interval in frames.
    pub min_keyint: u32,
    /// Consecutive B-frames.
    pub b_frames: u8,
    /// Reference frames.
    pub refs: u8,
}

impl Default for Gop {
    /// Fixed short IP GOP: 60-frame cadence, zero B-frames, single ref.
    fn default() -> Self {
        Self {
            keyint: 60,
            min_keyint: 60,
            b_frames: 0,
            refs: 1,
        }
    }
}

/// Speed preset: encode effort against compression.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpeedPreset {
    /// Lowest latency effort.
    Realtime,
    /// Balanced effort.
    Balanced,
    /// Highest compression effort.
    Quality,
}

impl Default for SpeedPreset {
    /// Realtime: lowest latency effort.
    fn default() -> Self {
        Self::Realtime
    }
}

/// Quality target: speed preset selecting encode effort.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Quality {
    /// Speed preset.
    pub preset: SpeedPreset,
}

impl Default for Quality {
    /// Realtime preset.
    fn default() -> Self {
        Self {
            preset: SpeedPreset::Realtime,
        }
    }
}

/// Frame rate: integer frames per `den` seconds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FrameRate {
    /// Frames per period.
    pub num: u32,
    /// Period in seconds.
    pub den: u32,
}

impl Default for FrameRate {
    /// 30 fps.
    fn default() -> Self {
        Self { num: 30, den: 1 }
    }
}

/// Shared encode configuration: rate, structure, quality, profile, level,
/// depth, chroma, frame rate, color.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EncodeConfig {
    /// Rate control.
    pub rate_control: RateControl,
    /// Group of pictures.
    pub gop: Gop,
    /// Quality target.
    pub quality: Quality,
    /// Encode profile.
    pub profile: Profile,
    /// Encode level.
    pub level: Level,
    /// Bit depth in bits per sample.
    pub bit_depth: u8,
    /// Chroma sampling.
    pub chroma: ChromaSampling,
    /// Frame rate.
    pub frame_rate: FrameRate,
    /// Color description.
    pub color: ColorConfig,
}

impl EncodeConfig {
    /// Codec from the profile; the level must name the same codec.
    #[must_use]
    pub fn codec(&self) -> Codec {
        self.profile.codec()
    }

    /// True with HDR mastering or light metadata attached.
    #[must_use]
    pub fn is_hdr(&self) -> bool {
        self.color.is_hdr()
    }

    /// Check profile/level codec match, depth/chroma, GOP, rate, frame rate.
    ///
    /// # Errors
    ///
    /// Returns the first [`Decline`] found.
    pub fn validate(&self) -> Result<(), Decline> {
        if self.profile.codec() != self.level.codec() {
            return Err(Decline::ProfileLevelMismatch {
                profile: self.profile,
                level: self.level,
            });
        }
        let max = self.profile.max_bit_depth();
        if self.bit_depth > max {
            return Err(Decline::BitDepthMismatch {
                actual: self.bit_depth,
                max,
            });
        }
        if self.bit_depth < 8 {
            return Err(Decline::BitDepthMismatch {
                actual: self.bit_depth,
                max,
            });
        }
        if !self.profile.allows(self.bit_depth, self.chroma) {
            return Err(Decline::ChromaUnsupported {
                chroma: self.chroma,
            });
        }
        if self.gop.keyint == 0 || self.gop.min_keyint == 0 || self.gop.min_keyint > self.gop.keyint
        {
            return Err(Decline::GopInvalid { gop: self.gop });
        }
        match self.rate_control {
            RateControl::Auto | RateControl::Cqp { .. } | RateControl::Crf { .. } => {}
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => {
                if bitrate == 0 || maxrate == 0 || bufsize == 0 || maxrate < bitrate {
                    return Err(Decline::RateControlInvalid {
                        mode: self.rate_control,
                    });
                }
            }
            RateControl::Vbr { bitrate, maxrate } => {
                if bitrate == 0 || maxrate == 0 || maxrate < bitrate {
                    return Err(Decline::RateControlInvalid {
                        mode: self.rate_control,
                    });
                }
            }
        }
        if self.frame_rate.num == 0 || self.frame_rate.den == 0 {
            return Err(Decline::FrameRateInvalid {
                rate: self.frame_rate,
            });
        }
        Ok(())
    }
}

impl Default for EncodeConfig {
    /// Realtime-sane: capped CBR, fixed IP GOP, H.264 Main auto, 8-bit
    /// 4:2:0, 30 fps, BT.709 SDR.
    fn default() -> Self {
        Self {
            rate_control: RateControl::default(),
            gop: Gop::default(),
            quality: Quality::default(),
            profile: Profile::default(),
            level: Level::default(),
            bit_depth: 8,
            chroma: ChromaSampling::Yuv420,
            frame_rate: FrameRate::default(),
            color: ColorConfig::default(),
        }
    }
}

/// Encode decline: loud typed reason a config cannot open.
///
/// Caps validation returns the structural reasons; tiers return the
/// support reasons when a valid config exceeds the backend.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Decline {
    /// Profile and level name different codecs.
    ProfileLevelMismatch {
        /// Requested profile.
        profile: Profile,
        /// Requested level.
        level: Level,
    },
    /// Bit depth outside the 8-bit floor or the profile maximum.
    BitDepthMismatch {
        /// Requested bits.
        actual: u8,
        /// Profile maximum bits.
        max: u8,
    },
    /// Chroma sampling the profile forbids or the tier lacks.
    ChromaUnsupported {
        /// Requested sampling.
        chroma: ChromaSampling,
    },
    /// Keyframe cadence with a zero bound or a minimum above the maximum.
    GopInvalid {
        /// Requested GOP.
        gop: Gop,
    },
    /// B-frames above the tier maximum.
    BFramesUnsupported {
        /// Requested B-frames.
        actual: u8,
        /// Tier maximum B-frames.
        max: u8,
    },
    /// References above the tier maximum.
    RefsUnsupported {
        /// Requested references.
        actual: u8,
        /// Tier maximum references.
        max: u8,
    },
    /// Rate control with a zero bound or a cap below the target.
    RateControlInvalid {
        /// Requested mode.
        mode: RateControl,
    },
    /// Rate control mode the tier lacks.
    RateControlUnsupported {
        /// Requested mode.
        mode: RateControl,
    },
    /// Speed preset the tier lacks.
    PresetUnsupported {
        /// Requested preset.
        preset: SpeedPreset,
    },
    /// Encode profile the tier lacks.
    ProfileUnsupported {
        /// Requested profile.
        profile: Profile,
    },
    /// Encode level the tier lacks.
    LevelUnsupported {
        /// Requested level.
        level: Level,
    },
    /// Bit depth above the tier maximum.
    BitDepthUnsupported {
        /// Requested bits.
        actual: u8,
        /// Tier maximum bits.
        max: u8,
    },
    /// Frame rate with a zero numerator or denominator.
    FrameRateInvalid {
        /// Requested rate.
        rate: FrameRate,
    },
    /// Frame rate the tier lacks.
    FrameRateUnsupported {
        /// Requested rate.
        rate: FrameRate,
    },
    /// Color primaries the tier cannot signal.
    PrimariesUnsupported {
        /// Requested primaries.
        primaries: ColorPrimaries,
    },
    /// Transfer characteristics the tier cannot signal.
    TransferUnsupported {
        /// Requested transfer.
        transfer: TransferCharacteristics,
    },
    /// Matrix coefficients the tier cannot signal.
    MatrixUnsupported {
        /// Requested matrix.
        matrix: MatrixCoefficients,
    },
    /// Color range the tier cannot signal.
    RangeUnsupported {
        /// Requested range.
        range: ColorRange,
    },
    /// HDR mastering or light metadata on an SDR tier.
    HdrUnsupported,
    /// Keyframe request the tier cannot honor.
    KeyframeUnsupported,
}

impl fmt::Display for Decline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileLevelMismatch { profile, level } => write!(
                f,
                "profile {profile:?} and level {level:?} name different codecs"
            ),
            Self::BitDepthMismatch { actual, max } => {
                write!(f, "bit depth {actual} outside 8-bit floor or maximum {max}")
            }
            Self::ChromaUnsupported { chroma } => {
                write!(f, "chroma {chroma:?} unsupported")
            }
            Self::GopInvalid { gop } => write!(
                f,
                "gop keyint {} min {} b {} refs {} invalid",
                gop.keyint, gop.min_keyint, gop.b_frames, gop.refs
            ),
            Self::BFramesUnsupported { actual, max } => {
                write!(f, "{actual} B-frames above tier maximum {max}")
            }
            Self::RefsUnsupported { actual, max } => {
                write!(f, "{actual} references above tier maximum {max}")
            }
            Self::RateControlInvalid { mode } => {
                write!(f, "rate control {mode:?} invalid")
            }
            Self::RateControlUnsupported { mode } => {
                write!(f, "rate control {mode:?} unsupported")
            }
            Self::PresetUnsupported { preset } => {
                write!(f, "preset {preset:?} unsupported")
            }
            Self::ProfileUnsupported { profile } => {
                write!(f, "profile {profile:?} unsupported")
            }
            Self::LevelUnsupported { level } => {
                write!(f, "level {level:?} unsupported")
            }
            Self::BitDepthUnsupported { actual, max } => {
                write!(f, "bit depth {actual} above tier maximum {max}")
            }
            Self::FrameRateInvalid { rate } => {
                write!(f, "frame rate {}/{} invalid", rate.num, rate.den)
            }
            Self::FrameRateUnsupported { rate } => {
                write!(f, "frame rate {}/{} unsupported", rate.num, rate.den)
            }
            Self::PrimariesUnsupported { primaries } => {
                write!(f, "primaries {primaries:?} unsupported")
            }
            Self::TransferUnsupported { transfer } => {
                write!(f, "transfer {transfer:?} unsupported")
            }
            Self::MatrixUnsupported { matrix } => {
                write!(f, "matrix {matrix:?} unsupported")
            }
            Self::RangeUnsupported { range } => {
                write!(f, "range {range:?} unsupported")
            }
            Self::HdrUnsupported => write!(f, "HDR metadata on SDR tier unsupported"),
            Self::KeyframeUnsupported => write!(f, "keyframe request unsupported"),
        }
    }
}

impl std::error::Error for Decline {}

#[cfg(test)]
mod tests {
    use crate::color::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics};
    use crate::common::ChromaSampling;

    use super::{
        Codec, Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl,
        SpeedPreset,
    };
    use crate::{av1, h264, h265, vp9};

    #[test]
    fn default_is_realtime_sane() {
        let config = EncodeConfig::default();
        assert_eq!(
            config.rate_control,
            RateControl::Cbr {
                bitrate: 5_000_000,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            }
        );
        assert_eq!(config.gop.keyint, 60);
        assert_eq!(config.gop.min_keyint, 60);
        assert_eq!(config.gop.b_frames, 0);
        assert_eq!(config.gop.refs, 1);
        assert_eq!(config.quality.preset, SpeedPreset::Realtime);
        assert_eq!(config.profile, Profile::H264(h264::EncodeProfile::Main));
        assert_eq!(config.level, Level::H264(h264::EncodeLevel::Auto));
        assert_eq!(config.bit_depth, 8);
        assert_eq!(config.chroma, ChromaSampling::Yuv420);
        assert_eq!(config.frame_rate, FrameRate { num: 30, den: 1 });
        assert_eq!(config.color.primaries, ColorPrimaries::Bt709);
        assert_eq!(config.color.transfer, TransferCharacteristics::Bt709);
        assert_eq!(config.color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(config.color.range, ColorRange::Limited);
        assert_eq!(config.codec(), Codec::H264);
        assert!(!config.is_hdr());
    }

    #[test]
    fn default_validates() {
        EncodeConfig::default().validate().expect("valid");
    }

    #[test]
    fn auto_rate_preserves_legacy() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            ..Default::default()
        };
        config.validate().expect("auto passes");
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..Default::default()
        };
        config.validate().expect("cqp passes");
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 23 },
            ..Default::default()
        };
        config.validate().expect("crf passes");
    }

    #[test]
    fn profile_level_codec_mismatch_declines() {
        let config = EncodeConfig {
            level: Level::H265(h265::EncodeLevel::Auto),
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(Decline::ProfileLevelMismatch { .. })
        ));
    }

    #[test]
    fn bit_depth_mismatch_declines() {
        let config = EncodeConfig {
            bit_depth: 10,
            ..Default::default()
        };
        assert_eq!(
            config.validate(),
            Err(Decline::BitDepthMismatch { actual: 10, max: 8 })
        );
        let config = EncodeConfig {
            bit_depth: 7,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(Decline::BitDepthMismatch { .. })
        ));
    }

    #[test]
    fn chroma_mismatch_declines() {
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..Default::default()
        };
        assert_eq!(
            config.validate(),
            Err(Decline::ChromaUnsupported {
                chroma: ChromaSampling::Yuv444,
            })
        );
    }

    #[test]
    fn high10_allows_ten_bit() {
        let config = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::High10),
            bit_depth: 10,
            ..Default::default()
        };
        config.validate().expect("high10 ten-bit");
    }

    #[test]
    fn vp9_profile_matrix() {
        let config = EncodeConfig {
            profile: Profile::Vp9(vp9::EncodeProfile::P0),
            level: Level::Vp9(vp9::EncodeLevel::Auto),
            ..Default::default()
        };
        config.validate().expect("p0 8-bit 420");
        let config = EncodeConfig {
            profile: Profile::Vp9(vp9::EncodeProfile::P0),
            level: Level::Vp9(vp9::EncodeLevel::Auto),
            bit_depth: 10,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(Decline::BitDepthMismatch { .. })
        ));
        let config = EncodeConfig {
            profile: Profile::Vp9(vp9::EncodeProfile::P2),
            level: Level::Vp9(vp9::EncodeLevel::Auto),
            bit_depth: 10,
            ..Default::default()
        };
        config.validate().expect("p2 ten-bit");
    }

    #[test]
    fn av1_professional_allows_twelve_bit() {
        let config = EncodeConfig {
            profile: Profile::Av1(av1::EncodeProfile::Professional),
            level: Level::Av1(av1::EncodeLevel::Auto),
            bit_depth: 12,
            ..Default::default()
        };
        config.validate().expect("professional twelve-bit");
        let config = EncodeConfig {
            profile: Profile::Av1(av1::EncodeProfile::Main),
            level: Level::Av1(av1::EncodeLevel::Auto),
            bit_depth: 12,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(Decline::BitDepthMismatch { .. })
        ));
    }

    #[test]
    fn vp8_implicit_profile() {
        let config = EncodeConfig {
            profile: Profile::Vp8,
            level: Level::Vp8,
            ..Default::default()
        };
        assert_eq!(config.codec(), Codec::Vp8);
        assert!(config.level.is_auto());
        config.validate().expect("vp8 8-bit 420");
        let config = EncodeConfig {
            profile: Profile::Vp8,
            level: Level::Vp8,
            bit_depth: 10,
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(Decline::BitDepthMismatch { .. })
        ));
    }

    #[test]
    fn gop_invalid_declines() {
        for gop in [
            Gop {
                keyint: 0,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
            Gop {
                keyint: 60,
                min_keyint: 0,
                b_frames: 0,
                refs: 1,
            },
            Gop {
                keyint: 30,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
        ] {
            let config = EncodeConfig {
                gop,
                ..Default::default()
            };
            assert_eq!(config.validate(), Err(Decline::GopInvalid { gop }));
        }
    }

    #[test]
    fn rate_invalid_declines() {
        for mode in [
            RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            RateControl::Cbr {
                bitrate: 5_000_000,
                maxrate: 4_000_000,
                bufsize: 5_000_000,
            },
            RateControl::Cbr {
                bitrate: 5_000_000,
                maxrate: 5_000_000,
                bufsize: 0,
            },
            RateControl::Vbr {
                bitrate: 0,
                maxrate: 5_000_000,
            },
            RateControl::Vbr {
                bitrate: 6_000_000,
                maxrate: 5_000_000,
            },
        ] {
            let config = EncodeConfig {
                rate_control: mode,
                ..Default::default()
            };
            assert_eq!(config.validate(), Err(Decline::RateControlInvalid { mode }));
        }
    }

    #[test]
    fn frame_rate_invalid_declines() {
        for rate in [FrameRate { num: 0, den: 1 }, FrameRate { num: 30, den: 0 }] {
            let config = EncodeConfig {
                frame_rate: rate,
                ..Default::default()
            };
            assert_eq!(config.validate(), Err(Decline::FrameRateInvalid { rate }));
        }
    }

    #[test]
    fn pinned_level_matches_codec() {
        let config = EncodeConfig {
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L41)),
            ..Default::default()
        };
        assert!(!config.level.is_auto());
        config.validate().expect("pinned h264");
    }

    #[test]
    fn quality_presets_cover_effort() {
        assert_eq!(Quality::default().preset, SpeedPreset::Realtime);
        assert_eq!(SpeedPreset::default(), SpeedPreset::Realtime);
        for preset in [
            SpeedPreset::Realtime,
            SpeedPreset::Balanced,
            SpeedPreset::Quality,
        ] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..Default::default()
            };
            config.validate().expect("preset passes");
        }
    }

    #[test]
    fn decline_display_names_reasons() {
        assert_eq!(
            Decline::HdrUnsupported.to_string(),
            "HDR metadata on SDR tier unsupported"
        );
        assert_eq!(
            Decline::KeyframeUnsupported.to_string(),
            "keyframe request unsupported"
        );
        assert_eq!(
            Decline::BFramesUnsupported { actual: 2, max: 0 }.to_string(),
            "2 B-frames above tier maximum 0"
        );
        assert_eq!(
            Decline::FrameRateInvalid {
                rate: FrameRate { num: 30, den: 0 },
            }
            .to_string(),
            "frame rate 30/0 invalid"
        );
        assert_eq!(
            Decline::ChromaUnsupported {
                chroma: ChromaSampling::Yuv444,
            }
            .to_string(),
            "chroma Yuv444 unsupported"
        );
    }
}
