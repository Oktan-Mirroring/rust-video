//! Byte-buffer encode behind the caps `Encoder` trait.

use std::time::Duration;

use crate::error::MediaCodecError;
use crate::format::FormatSpec;
use crate::sdk::{ApiLevel, COLOR_KEYS_SDK, IFRAME_FLOAT_SDK, LEVEL_KEY_SDK, QUALITY_KEY_SDK};
use crate::seam::{CodecSeam, SeamItem};

use bytes::Bytes;
use rust_video_caps::av1;
use rust_video_caps::color::{
    ColorConfig, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
};
use rust_video_caps::common::{ChromaSampling, PictureSize};
use rust_video_caps::encode::{
    Decline, EncodeConfig, FrameRate, Gop, Level, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::h264;
use rust_video_caps::h265;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;
use rust_video_caps::transcode::GpuCaps;
use rust_video_caps::vp9;
use rust_video_core::planes;

/// Codec carried by an encode spec.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
    /// VP8.
    Vp8,
}

impl Codec {
    /// Shared codec identity.
    fn shared(self) -> rust_video_core::Codec {
        match self {
            Self::H264 => rust_video_core::Codec::H264,
            Self::H265 => rust_video_core::Codec::H265,
            Self::Av1 => rust_video_core::Codec::Av1,
            Self::Vp9 => rust_video_core::Codec::Vp9,
            Self::Vp8 => rust_video_core::Codec::Vp8,
        }
    }
}

/// Encoder lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeState {
    /// Created, no format configured.
    Unconfigured,
    /// Format configured, not started.
    Configured,
    /// Started, accepts frames.
    Started,
    /// Released, unusable.
    Released,
}

impl EncodeState {
    /// State after `configure`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the encoder is unconfigured.
    pub fn configured(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Unconfigured => Ok(Self::Configured),
            other => Err(MediaCodecError::InvalidEncodeState {
                op: "configure",
                state: other,
            }),
        }
    }

    /// State after `start`.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless the encoder is configured.
    pub fn started(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Configured => Ok(Self::Started),
            other => Err(MediaCodecError::InvalidEncodeState {
                op: "start",
                state: other,
            }),
        }
    }

    /// State after `release`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the encoder is already released.
    pub fn released(self) -> Result<Self, MediaCodecError> {
        match self {
            Self::Released => Err(MediaCodecError::InvalidEncodeState {
                op: "release",
                state: self,
            }),
            Self::Unconfigured | Self::Configured | Self::Started => Ok(Self::Released),
        }
    }

    /// Rejection unless started.
    ///
    /// # Arguments
    ///
    /// * `op` - operation demanding the started state.
    ///
    /// # Errors
    ///
    /// Returns `Err` naming `op` and the current state.
    pub fn ensure_started(self, op: &'static str) -> Result<(), MediaCodecError> {
        if self == Self::Started {
            Ok(())
        } else {
            Err(MediaCodecError::InvalidEncodeState { op, state: self })
        }
    }
}

/// One encoder format value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FormatValue {
    /// Integer value.
    Int(i32),
    /// Float value.
    Float(f32),
    /// String value.
    Str(&'static str),
}

/// Byte-buffer plane arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// Semi-planar Y plus interleaved UV.
    Nv12,
    /// Planar Y, U, V.
    Planar,
}

/// Encoder input format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodeSpec {
    /// Codec identity.
    codec: Codec,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Requested input `color-format`.
    color_format: i32,
    /// Rate, structure, and color behind the format.
    config: EncodeConfig,
}

impl EncodeSpec {
    /// `MediaFormat#KEY_BIT_RATE`.
    pub const KEY_BIT_RATE: &'static str = "bitrate";
    /// `MediaFormat#KEY_BITRATE_MODE`.
    pub const KEY_BITRATE_MODE: &'static str = "bitrate-mode";
    /// Hidden `MediaFormat#KEY_MAX_BIT_RATE`.
    pub const KEY_MAX_BIT_RATE: &'static str = "max-bitrate";
    /// `MediaFormat#KEY_I_FRAME_INTERVAL`.
    pub const KEY_I_FRAME_INTERVAL: &'static str = "i-frame-interval";
    /// `MediaFormat#KEY_PROFILE`.
    pub const KEY_PROFILE: &'static str = "profile";
    /// `MediaFormat#KEY_LEVEL`.
    pub const KEY_LEVEL: &'static str = "level";
    /// `MediaFormat#KEY_COLOR_STANDARD`.
    pub const KEY_COLOR_STANDARD: &'static str = "color-standard";
    /// `MediaFormat#KEY_COLOR_TRANSFER`.
    pub const KEY_COLOR_TRANSFER: &'static str = "color-transfer";
    /// `MediaFormat#KEY_COLOR_RANGE`.
    pub const KEY_COLOR_RANGE: &'static str = "color-range";
    /// `MediaFormat#KEY_QUALITY`.
    pub const KEY_QUALITY: &'static str = "quality";

    /// Largest dimension the native format takes as `i32`.
    const MAX_DIMENSION: u32 = i32::MAX as u32;
    /// Microseconds per second.
    const MICROS_PER_SECOND: u128 = 1_000_000;
    /// Tier bit depth ceiling: byte-buffer inputs are 8-bit.
    const MAX_BIT_DEPTH: u8 = 8;
    /// Tier B-frame ceiling: IP structure only.
    const MAX_B_FRAMES: u8 = 0;
    /// Tier reference ceiling: single reference.
    const MAX_REFS: u8 = 1;
    /// Legacy bitrate under [`RateControl::Auto`].
    const DEFAULT_BITRATE: u32 = 5_000_000;

    /// H.264 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn h264(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::H264,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// H.265 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn h265(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::H265,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// AV1 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn av1(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::Av1,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// VP9 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn vp9(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::Vp9,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// VP8 encode spec.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width; zero or oversize rejected.
    /// * `height` - coded height; zero or oversize rejected.
    /// * `bitrate` - target bits per second; zero rejected.
    /// * `frame_rate` - declared frames per second; zero rejected.
    /// * `keyframe_interval` - seconds between keyframes.
    #[must_use]
    pub fn vp8(
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        Self::coded(
            Codec::Vp8,
            width,
            height,
            bitrate,
            frame_rate,
            keyframe_interval,
        )
    }

    /// Spec for `codec` over `width` by `height` at `bitrate` bps.
    fn coded(
        codec: Codec,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: u32,
        keyframe_interval: u32,
    ) -> Option<Self> {
        if width == 0
            || height == 0
            || width > Self::MAX_DIMENSION
            || height > Self::MAX_DIMENSION
            || bitrate == 0
            || frame_rate == 0
        {
            return None;
        }
        let frames = u64::from(keyframe_interval) * u64::from(frame_rate);
        let keyint = u32::try_from(frames).ok()?;
        let gop = if keyint == 0 {
            Gop {
                keyint: 1,
                min_keyint: 1,
                b_frames: 0,
                refs: 1,
            }
        } else {
            Gop {
                keyint,
                min_keyint: keyint,
                b_frames: 0,
                refs: 1,
            }
        };
        let (profile, level) = Self::default_profile_level(codec);
        Some(Self {
            codec,
            width,
            height,
            color_format: FormatSpec::COLOR_PLANAR,
            config: EncodeConfig {
                rate_control: RateControl::Cbr {
                    bitrate,
                    maxrate: bitrate,
                    bufsize: bitrate,
                },
                gop,
                profile,
                level,
                frame_rate: FrameRate {
                    num: frame_rate,
                    den: 1,
                },
                ..EncodeConfig::default()
            },
        })
    }

    /// Default profile with tier-picked level for `codec`.
    fn default_profile_level(codec: Codec) -> (Profile, Level) {
        match codec {
            Codec::H264 => (
                Profile::H264(h264::EncodeProfile::Main),
                Level::H264(h264::EncodeLevel::Auto),
            ),
            Codec::H265 => (
                Profile::H265(h265::EncodeProfile::Main),
                Level::H265(h265::EncodeLevel::Auto),
            ),
            Codec::Av1 => (
                Profile::Av1(av1::EncodeProfile::Main),
                Level::Av1(av1::EncodeLevel::Auto),
            ),
            Codec::Vp9 => (
                Profile::Vp9(vp9::EncodeProfile::P0),
                Level::Vp9(vp9::EncodeLevel::Auto),
            ),
            Codec::Vp8 => (Profile::Vp8, Level::Vp8),
        }
    }

    /// Spec requesting input `color-format` `color`.
    #[must_use]
    pub fn with_color_format(mut self, color: i32) -> Self {
        self.color_format = color;
        self
    }

    /// Spec carrying `config` rate and structure.
    #[must_use]
    pub fn with_config(mut self, config: EncodeConfig) -> Self {
        self.config = config;
        self
    }

    /// Codec mime type.
    #[must_use]
    pub fn mime(&self) -> &'static str {
        match self.codec {
            Codec::H264 => FormatSpec::MIME_H264,
            Codec::H265 => FormatSpec::MIME_H265,
            Codec::Av1 => FormatSpec::MIME_AV1,
            Codec::Vp9 => FormatSpec::MIME_VP9,
            Codec::Vp8 => FormatSpec::MIME_VP8,
        }
    }

    /// Lowest device API carrying this codec.
    #[must_use]
    pub fn min_sdk(&self) -> u32 {
        match self.codec {
            Codec::Av1 => FormatSpec::AV1_MIN_SDK,
            Codec::H264 | Codec::H265 | Codec::Vp9 | Codec::Vp8 => crate::sdk::MIN_SDK,
        }
    }

    /// Coded width in luma samples.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Coded height in luma samples.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Requested input `color-format`.
    #[must_use]
    pub fn color_format(&self) -> i32 {
        self.color_format
    }

    /// Encode config behind the format.
    #[must_use]
    pub fn config(&self) -> &EncodeConfig {
        &self.config
    }

    /// Presentation time of frame `index` in microseconds.
    ///
    /// # Arguments
    ///
    /// * `index` - zero-based frame count.
    #[must_use]
    pub fn pts_for(&self, index: u64) -> u64 {
        let rate = self.config.frame_rate;
        if rate.num == 0 {
            return u64::MAX;
        }
        let pts = u128::from(index) * Self::MICROS_PER_SECOND * u128::from(rate.den)
            / u128::from(rate.num);
        u64::try_from(pts).unwrap_or(u64::MAX)
    }

    /// Format entries for `configure` on `level`.
    ///
    /// # Errors
    ///
    /// Returns `ConfigDeclined` when the config is invalid or exceeds the
    /// tier or `level`.
    ///
    /// # Arguments
    ///
    /// * `level` - device level gating keys by availability; `None` on an
    ///   unknown device allows every gated key.
    ///
    /// # Usage Notes
    ///
    /// Entry order is stable: mime, width, height, rate keys, keyframe
    /// interval, profile/level, frame rate, color keys, color format.
    /// Structural faults decline before any tier check; tier checks run
    /// in emission order and the first fault wins. [`RateControl::Auto`]
    /// emits the legacy key set with the default bitrate; tier-picked
    /// levels omit profile and level; default color omits color keys.
    /// The CBR buffer size and the GOP minimum have no format keys and
    /// need none: the encoder manages its own buffer and the tier emits
    /// a fixed cadence at the max bound.
    pub(crate) fn format_entries(
        &self,
        level: Option<ApiLevel>,
    ) -> Result<Vec<FormatEntry>, MediaCodecError> {
        self.entries_inner(level)
            .map_err(|decline| MediaCodecError::ConfigDeclined { decline })
    }

    /// Entries behind [`EncodeSpec::format_entries`], declining typed.
    fn entries_inner(&self, level: Option<ApiLevel>) -> Result<Vec<FormatEntry>, Decline> {
        let config = &self.config;
        config.validate()?;
        let mut entries = vec![
            FormatEntry::str(FormatSpec::KEY_MIME, self.mime()),
            FormatEntry::int(FormatSpec::KEY_WIDTH, Self::saturate(self.width)),
            FormatEntry::int(FormatSpec::KEY_HEIGHT, Self::saturate(self.height)),
        ];
        entries.extend(Self::rate_entries(config.rate_control, level)?);
        entries.push(Self::interval_entry(config.gop, config.frame_rate, level)?);
        Self::check_preset(config.quality.preset)?;
        if config.profile.codec() != self.codec.shared() {
            return Err(Decline::ProfileUnsupported {
                profile: config.profile,
            });
        }
        if config.bit_depth > Self::MAX_BIT_DEPTH {
            return Err(Decline::BitDepthUnsupported {
                actual: config.bit_depth,
                max: Self::MAX_BIT_DEPTH,
            });
        }
        if config.chroma != ChromaSampling::Yuv420 {
            return Err(Decline::ChromaUnsupported {
                chroma: config.chroma,
            });
        }
        if let Some((profile, pinned)) =
            Self::profile_level(self.codec, config.profile, config.level, level)?
        {
            entries.push(FormatEntry::int(Self::KEY_PROFILE, profile));
            entries.push(FormatEntry::int(Self::KEY_LEVEL, pinned));
        }
        entries.push(Self::frame_rate_entry(config.frame_rate));
        entries.extend(Self::color_entries(config.color, level)?);
        entries.push(FormatEntry::int(
            FormatSpec::KEY_COLOR_FORMAT,
            self.color_format,
        ));
        Ok(entries)
    }

    /// `value` narrowed to the native range.
    fn saturate<T>(value: T) -> i32
    where
        i32: TryFrom<T>,
    {
        i32::try_from(value).unwrap_or(i32::MAX)
    }

    /// Rate entries for `mode` on `level`.
    ///
    /// # Errors
    ///
    /// Returns `RateControlUnsupported` for quality modes below the
    /// `quality` key floor.
    ///
    /// # Arguments
    ///
    /// * `mode` - requested rate control.
    /// * `level` - device level gating keys by availability; `None`
    ///   allows gated keys.
    ///
    /// # Usage Notes
    ///
    /// Mode values follow `MediaCodecInfo.EncoderCapabilities`: CQ 0,
    /// VBR 1, CBR 2.
    /// <https://developer.android.com/reference/android/media/MediaCodecInfo.EncoderCapabilities>
    fn rate_entries(
        mode: RateControl,
        level: Option<ApiLevel>,
    ) -> Result<Vec<FormatEntry>, Decline> {
        match mode {
            RateControl::Auto => Ok(vec![FormatEntry::int(
                Self::KEY_BIT_RATE,
                Self::saturate(Self::DEFAULT_BITRATE),
            )]),
            RateControl::Cbr {
                bitrate, maxrate, ..
            } => Ok(vec![
                FormatEntry::int(Self::KEY_BITRATE_MODE, 2),
                FormatEntry::int(Self::KEY_BIT_RATE, Self::saturate(bitrate)),
                FormatEntry::int(Self::KEY_MAX_BIT_RATE, Self::saturate(maxrate)),
            ]),
            RateControl::Vbr { bitrate, maxrate } => Ok(vec![
                FormatEntry::int(Self::KEY_BITRATE_MODE, 1),
                FormatEntry::int(Self::KEY_BIT_RATE, Self::saturate(bitrate)),
                FormatEntry::int(Self::KEY_MAX_BIT_RATE, Self::saturate(maxrate)),
            ]),
            RateControl::Cqp { qp } | RateControl::Crf { value: qp } => {
                if !level.is_none_or(|api| api.at_least(QUALITY_KEY_SDK)) {
                    return Err(Decline::RateControlUnsupported { mode });
                }
                Ok(vec![
                    FormatEntry::int(Self::KEY_BITRATE_MODE, 0),
                    FormatEntry::int(Self::KEY_QUALITY, i32::from(qp)),
                ])
            }
        }
    }

    /// Keyframe interval entry for `gop` at `rate` on `level`.
    ///
    /// # Errors
    ///
    /// Returns `BFramesUnsupported`/`RefsUnsupported` above the tier
    /// maxima, `GopInvalid` for fractional-second cadences below the
    /// float floor.
    ///
    /// # Arguments
    ///
    /// * `gop` - requested keyframe cadence.
    /// * `rate` - frame rate dividing frames into seconds.
    /// * `level` - device level gating keys by availability; `None`
    ///   allows gated keys.
    ///
    /// # Usage Notes
    ///
    /// A one-frame cadence emits 0, the all-keyframes mark, on every API.
    // Hint value; the encoder quantizes fractional seconds to frames.
    #[allow(clippy::cast_precision_loss)]
    fn interval_entry(
        gop: Gop,
        rate: FrameRate,
        level: Option<ApiLevel>,
    ) -> Result<FormatEntry, Decline> {
        debug_assert!(rate.num > 0 && rate.den > 0);
        if gop.b_frames > Self::MAX_B_FRAMES {
            return Err(Decline::BFramesUnsupported {
                actual: gop.b_frames,
                max: Self::MAX_B_FRAMES,
            });
        }
        if gop.refs > Self::MAX_REFS {
            return Err(Decline::RefsUnsupported {
                actual: gop.refs,
                max: Self::MAX_REFS,
            });
        }
        if gop.keyint == 1 {
            return Ok(FormatEntry::int(Self::KEY_I_FRAME_INTERVAL, 0));
        }
        let frames = u64::from(gop.keyint) * u64::from(rate.den);
        let fps = u64::from(rate.num);
        if frames.is_multiple_of(fps) {
            let seconds = frames / fps;
            return Ok(FormatEntry::int(
                Self::KEY_I_FRAME_INTERVAL,
                Self::saturate(seconds),
            ));
        }
        if !level.is_none_or(|api| api.at_least(IFRAME_FLOAT_SDK)) {
            return Err(Decline::GopInvalid { gop });
        }
        Ok(FormatEntry::float(
            Self::KEY_I_FRAME_INTERVAL,
            frames as f32 / fps as f32,
        ))
    }

    /// Profile/level pair for `codec`, absent for tier-picked levels.
    ///
    /// # Errors
    ///
    /// Returns `LevelUnsupported` below the `level` key floor or without
    /// a platform value, `ProfileUnsupported` without an exact platform
    /// value.
    ///
    /// # Arguments
    ///
    /// * `codec` - spec codec identity.
    /// * `profile` - requested encode profile.
    /// * `level` - requested encode level.
    /// * `api` - device level gating keys by availability; `None` allows
    ///   gated keys.
    ///
    /// # Returns
    ///
    /// `None` when the level is tier-picked; the encoder selects both.
    ///
    /// # Usage Notes
    ///
    /// Values follow `MediaCodecInfo.CodecProfileLevel`.
    /// <https://developer.android.com/reference/android/media/MediaCodecInfo.CodecProfileLevel>
    fn profile_level(
        codec: Codec,
        profile: Profile,
        level: Level,
        api: Option<ApiLevel>,
    ) -> Result<Option<(i32, i32)>, Decline> {
        if level.is_auto() {
            return Ok(None);
        }
        if !api.is_none_or(|device| device.at_least(LEVEL_KEY_SDK)) {
            return Err(Decline::LevelUnsupported { level });
        }
        let value = Self::profile_value(codec, profile)?;
        let pinned = Self::level_value(level)?;
        Ok(Some((value, pinned)))
    }

    /// Platform profile value for `profile` on `codec`.
    ///
    /// # Errors
    ///
    /// Returns `ProfileUnsupported` without an exact platform value.
    ///
    /// # Arguments
    ///
    /// * `codec` - spec codec identity.
    /// * `profile` - requested encode profile.
    ///
    /// # Usage Notes
    ///
    /// Only profiles native to 8-bit 4:2:0 map; anything wider would
    /// mislabel tier content. Values follow
    /// `MediaCodecInfo.CodecProfileLevel`.
    /// <https://developer.android.com/reference/android/media/MediaCodecInfo.CodecProfileLevel>
    fn profile_value(codec: Codec, profile: Profile) -> Result<i32, Decline> {
        let value = match (codec, profile) {
            (Codec::H264, Profile::H264(h264::EncodeProfile::Main)) => 2,
            (Codec::H264, Profile::H264(h264::EncodeProfile::High)) => 8,
            (Codec::H264, Profile::H264(h264::EncodeProfile::Baseline))
            | (Codec::H265, Profile::H265(h265::EncodeProfile::Main))
            | (Codec::Av1, Profile::Av1(av1::EncodeProfile::Main))
            | (Codec::Vp9, Profile::Vp9(vp9::EncodeProfile::P0)) => 1,
            _ => return Err(Decline::ProfileUnsupported { profile }),
        };
        Ok(value)
    }

    /// Platform level value for `level`.
    ///
    /// # Errors
    ///
    /// Returns `LevelUnsupported` without a platform value.
    ///
    /// # Arguments
    ///
    /// * `level` - requested encode level.
    ///
    /// # Usage Notes
    ///
    /// H.265 maps main-tier values; levels past 6.2 carry none. Values
    /// follow `MediaCodecInfo.CodecProfileLevel`.
    /// <https://developer.android.com/reference/android/media/MediaCodecInfo.CodecProfileLevel>
    fn level_value(level: Level) -> Result<i32, Decline> {
        let value = match level {
            Level::H264(h264::EncodeLevel::Pinned(pinned)) => Some(Self::avc_level(pinned)),
            Level::H265(h265::EncodeLevel::Pinned(pinned)) => Self::hevc_level(pinned),
            Level::Av1(av1::EncodeLevel::Pinned(pinned)) => Self::av1_level(pinned),
            Level::Vp9(vp9::EncodeLevel::Pinned(pinned)) => Some(Self::vp9_level(pinned)),
            _ => None,
        };
        value.ok_or(Decline::LevelUnsupported { level })
    }

    /// Platform AVC level value for `level`.
    fn avc_level(level: h264::Level) -> i32 {
        match level {
            h264::Level::L1 => 1,
            h264::Level::L1b => 2,
            h264::Level::L11 => 4,
            h264::Level::L12 => 8,
            h264::Level::L13 => 16,
            h264::Level::L2 => 32,
            h264::Level::L21 => 64,
            h264::Level::L22 => 128,
            h264::Level::L3 => 256,
            h264::Level::L31 => 512,
            h264::Level::L32 => 1024,
            h264::Level::L4 => 2048,
            h264::Level::L41 => 4096,
            h264::Level::L42 => 8192,
            h264::Level::L5 => 16384,
            h264::Level::L51 => 32768,
            h264::Level::L52 => 65536,
        }
    }

    /// Platform main-tier HEVC level value for `level`.
    fn hevc_level(level: h265::Level) -> Option<i32> {
        let value = match level {
            h265::Level::L1 => 1,
            h265::Level::L2 => 4,
            h265::Level::L21 => 16,
            h265::Level::L3 => 64,
            h265::Level::L31 => 256,
            h265::Level::L4 => 1024,
            h265::Level::L41 => 4096,
            h265::Level::L5 => 16384,
            h265::Level::L51 => 65536,
            h265::Level::L52 => 262_144,
            h265::Level::L6 => 1_048_576,
            h265::Level::L61 => 4_194_304,
            h265::Level::L62 => 16_777_216,
            h265::Level::L63 | h265::Level::L7 | h265::Level::L71 | h265::Level::L72 => {
                return None;
            }
        };
        Some(value)
    }

    /// Platform AV1 level value for `level`.
    fn av1_level(level: av1::Level) -> Option<i32> {
        let value = match level {
            av1::Level::L20 => 1,
            av1::Level::L21 => 2,
            av1::Level::L30 => 16,
            av1::Level::L31 => 32,
            av1::Level::L40 => 256,
            av1::Level::L41 => 512,
            av1::Level::L50 => 4096,
            av1::Level::L51 => 8192,
            av1::Level::L52 => 16384,
            av1::Level::L53 => 32768,
            av1::Level::L60 => 65536,
            av1::Level::L61 => 131_072,
            av1::Level::L62 => 262_144,
            av1::Level::L63 => 524_288,
            av1::Level::Maximum => return None,
        };
        Some(value)
    }

    /// Platform VP9 level value for `level`.
    fn vp9_level(level: vp9::Level) -> i32 {
        match level {
            vp9::Level::L1 => 1,
            vp9::Level::L11 => 2,
            vp9::Level::L2 => 4,
            vp9::Level::L21 => 8,
            vp9::Level::L3 => 16,
            vp9::Level::L31 => 32,
            vp9::Level::L4 => 64,
            vp9::Level::L41 => 128,
            vp9::Level::L5 => 256,
            vp9::Level::L51 => 512,
            vp9::Level::L52 => 1024,
            vp9::Level::L6 => 2048,
            vp9::Level::L61 => 4096,
            vp9::Level::L62 => 8192,
        }
    }

    /// Frame rate entry for `rate`.
    ///
    /// # Arguments
    ///
    /// * `rate` - requested frame rate.
    ///
    /// # Usage Notes
    ///
    /// Whole rates emit integers; fractional ones emit floats, which
    /// video codecs accept at every API.
    // Hint value; sub-hertz precision past f32 is meaningless to the encoder.
    #[allow(clippy::cast_precision_loss)]
    fn frame_rate_entry(rate: FrameRate) -> FormatEntry {
        debug_assert!(rate.num > 0 && rate.den > 0);
        if rate.num.is_multiple_of(rate.den) {
            FormatEntry::int(
                FormatSpec::KEY_FRAME_RATE,
                Self::saturate(rate.num / rate.den),
            )
        } else {
            FormatEntry::float(
                FormatSpec::KEY_FRAME_RATE,
                rate.num as f32 / rate.den as f32,
            )
        }
    }

    /// Color entries for `color` on `level`.
    ///
    /// # Errors
    ///
    /// Returns `HdrUnsupported` on HDR metadata, axis declines for
    /// unmappable or ungated color.
    ///
    /// # Arguments
    ///
    /// * `color` - requested color description.
    /// * `level` - device level gating keys by availability; `None`
    ///   allows gated keys.
    ///
    /// # Usage Notes
    ///
    /// Default BT.709 SDR emits nothing; every other axis emits its key
    /// where `level` carries color keys. Values follow the platform
    /// color tables.
    /// <https://android.googlesource.com/platform/frameworks/av/+/main/media/libstagefright/foundation/include/media/stagefright/foundation/ColorUtils.h>
    fn color_entries(
        color: ColorConfig,
        level: Option<ApiLevel>,
    ) -> Result<Vec<FormatEntry>, Decline> {
        if color.is_hdr() {
            return Err(Decline::HdrUnsupported);
        }
        let standard = Self::color_standard(color.primaries, color.matrix)?;
        let transfer = Self::color_transfer(color.transfer)?;
        let range = Self::color_range(color.range);
        let mut entries = Vec::new();
        if let Some(value) = standard {
            if !level.is_none_or(|api| api.at_least(COLOR_KEYS_SDK)) {
                return Err(Decline::PrimariesUnsupported {
                    primaries: color.primaries,
                });
            }
            entries.push(FormatEntry::int(Self::KEY_COLOR_STANDARD, value));
        }
        if let Some(value) = transfer {
            if !level.is_none_or(|api| api.at_least(COLOR_KEYS_SDK)) {
                return Err(Decline::TransferUnsupported {
                    transfer: color.transfer,
                });
            }
            entries.push(FormatEntry::int(Self::KEY_COLOR_TRANSFER, value));
        }
        if let Some(value) = range {
            if !level.is_none_or(|api| api.at_least(COLOR_KEYS_SDK)) {
                return Err(Decline::RangeUnsupported { range: color.range });
            }
            entries.push(FormatEntry::int(Self::KEY_COLOR_RANGE, value));
        }
        Ok(entries)
    }

    /// Platform color standard for `primaries` with `matrix`.
    ///
    /// # Errors
    ///
    /// Returns `MatrixUnsupported` when known primaries pair with an
    /// unmappable matrix, `PrimariesUnsupported` otherwise.
    ///
    /// # Arguments
    ///
    /// * `primaries` - requested color primaries.
    /// * `matrix` - requested matrix coefficients.
    ///
    /// # Returns
    ///
    /// `None` for default BT.709; the key stays unset.
    fn color_standard(
        primaries: ColorPrimaries,
        matrix: MatrixCoefficients,
    ) -> Result<Option<i32>, Decline> {
        match (primaries, matrix) {
            (ColorPrimaries::Bt709, MatrixCoefficients::Bt709) => Ok(None),
            (ColorPrimaries::Smpte170M, MatrixCoefficients::Smpte170M) => Ok(Some(4)),
            (ColorPrimaries::Bt470Bg, MatrixCoefficients::Bt470Bg) => Ok(Some(2)),
            (
                ColorPrimaries::Bt2020,
                MatrixCoefficients::Bt2020Ncl | MatrixCoefficients::Bt2020Cl,
            ) => Ok(Some(6)),
            (ColorPrimaries::Unspecified, MatrixCoefficients::Unspecified) => Ok(Some(0)),
            _ => Err(Self::color_standard_decline(primaries, matrix)),
        }
    }

    /// Decline blaming `matrix` under known `primaries`, else `primaries`.
    fn color_standard_decline(primaries: ColorPrimaries, matrix: MatrixCoefficients) -> Decline {
        match primaries {
            ColorPrimaries::Bt709
            | ColorPrimaries::Smpte170M
            | ColorPrimaries::Bt470Bg
            | ColorPrimaries::Bt2020
            | ColorPrimaries::Unspecified => Decline::MatrixUnsupported { matrix },
            _ => Decline::PrimariesUnsupported { primaries },
        }
    }

    /// Platform color transfer for `transfer`.
    ///
    /// # Errors
    ///
    /// Returns `TransferUnsupported` without a platform value.
    ///
    /// # Arguments
    ///
    /// * `transfer` - requested transfer characteristics.
    ///
    /// # Returns
    ///
    /// `None` for default BT.709; the key stays unset.
    fn color_transfer(transfer: TransferCharacteristics) -> Result<Option<i32>, Decline> {
        match transfer {
            TransferCharacteristics::Bt709 => Ok(None),
            TransferCharacteristics::Linear => Ok(Some(1)),
            TransferCharacteristics::Iec61966Part1 => Ok(Some(2)),
            TransferCharacteristics::Unspecified => Ok(Some(0)),
            TransferCharacteristics::Smpte170M
            | TransferCharacteristics::Smpte240M
            | TransferCharacteristics::Bt2020Ten
            | TransferCharacteristics::Bt2020Twelve => Ok(Some(3)),
            _ => Err(Decline::TransferUnsupported { transfer }),
        }
    }

    /// Platform color range for `range`.
    ///
    /// # Arguments
    ///
    /// * `range` - requested color range.
    ///
    /// # Returns
    ///
    /// `None` for default limited range; the key stays unset.
    fn color_range(range: ColorRange) -> Option<i32> {
        match range {
            ColorRange::Limited => None,
            ColorRange::Full => Some(1),
        }
    }

    /// Rejection unless `preset` needs no format key.
    ///
    /// # Errors
    ///
    /// Returns `PresetUnsupported`; effort has no portable key and vendor
    /// keys differ per `SoC`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn check_preset(preset: SpeedPreset) -> Result<(), Decline> {
        if preset == SpeedPreset::Realtime {
            Ok(())
        } else {
            Err(Decline::PresetUnsupported { preset })
        }
    }
}

/// One encoder format key with its value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FormatEntry {
    /// Format key.
    key: &'static str,
    /// Key value.
    value: FormatValue,
}

impl FormatEntry {
    /// Integer entry for `key`.
    pub(crate) fn int(key: &'static str, value: i32) -> Self {
        Self {
            key,
            value: FormatValue::Int(value),
        }
    }

    /// Float entry for `key`.
    pub(crate) fn float(key: &'static str, value: f32) -> Self {
        Self {
            key,
            value: FormatValue::Float(value),
        }
    }

    /// String entry for `key`.
    pub(crate) fn str(key: &'static str, value: &'static str) -> Self {
        Self {
            key,
            value: FormatValue::Str(value),
        }
    }

    /// Format key.
    // Read only from the Android materializer and tests.
    #[allow(dead_code)]
    pub(crate) fn key(&self) -> &'static str {
        self.key
    }

    /// Key value.
    // Read only from the Android materializer and tests.
    #[allow(dead_code)]
    pub(crate) fn value(&self) -> FormatValue {
        self.value
    }
}

/// Packed encoder input geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct InputLayout {
    /// Picture dimensions in luma samples.
    size: PictureSize,
    /// Plane arrangement.
    layout: Layout,
    /// Luma bytes per row.
    stride: u32,
    /// Luma rows.
    slice_height: u32,
    /// True after an input format landed.
    negotiated: bool,
}

impl InputLayout {
    /// Initial guess from `spec`, ahead of the input format.
    ///
    /// # Arguments
    ///
    /// * `spec` - encode spec carrying the coded dimensions.
    #[must_use]
    pub fn tentative(spec: &EncodeSpec) -> Option<Self> {
        PictureSize::new(spec.width(), spec.height()).map(|size| Self {
            size,
            layout: Layout::Planar,
            stride: spec.width(),
            slice_height: spec.height(),
            negotiated: false,
        })
    }

    /// Layout from raw input format readings.
    ///
    /// # Arguments
    ///
    /// * `color` - `color-format` value; unmapped values are rejected.
    /// * `stride` - row stride; missing or non-positive falls back to width.
    /// * `slice_height` - row count; missing or non-positive falls back to height.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the color has no plane layout.
    pub fn apply_format(
        &mut self,
        color: Option<i32>,
        stride: Option<i32>,
        slice_height: Option<i32>,
    ) -> Result<(), MediaCodecError> {
        let color = color.unwrap_or(-1);
        let format = FormatSpec::pixel_format_for(color)
            .ok_or(MediaCodecError::UnsupportedColorFormat { value: color })?;
        let layout = match format {
            PixelFormat::Nv12 => Layout::Nv12,
            PixelFormat::Yuv420P => Layout::Planar,
            _ => {
                return Err(MediaCodecError::UnsupportedColorFormat { value: color });
            }
        };
        self.layout = layout;
        self.stride = Self::or_fallback(stride, self.size.width());
        self.slice_height = Self::or_fallback(slice_height, self.size.height());
        self.negotiated = true;
        Ok(())
    }

    /// True after an input format landed.
    #[must_use]
    pub fn negotiated(&self) -> bool {
        self.negotiated
    }

    /// Frame packed into one contiguous input buffer.
    ///
    /// # Arguments
    ///
    /// * `frame` - planar 4:2:0 input at the configured extent.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the layout is not planar 4:2:0, the extent
    /// mismatches, or a plane is short for its rows.
    pub fn pack(&self, frame: &CpuFrame) -> Result<Bytes, MediaCodecError> {
        if frame.format() != PixelFormat::Yuv420P {
            return Err(MediaCodecError::UnsupportedInput {
                format: frame.format(),
            });
        }
        if frame.size() != self.size {
            return Err(self.mismatch(frame));
        }
        let [y, u, v] = frame.planes() else {
            return Err(self.mismatch(frame));
        };
        let width = self.size.width();
        let height = self.size.height();
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let mut packed = Vec::new();
        let _reserved = packed.try_reserve_exact(self.packed_len());
        match self.layout {
            Layout::Planar => {
                let half = self.stride / 2;
                Self::copy_rows(&mut packed, y, width, height, self.stride)?;
                Self::copy_rows(&mut packed, u, chroma_width, chroma_height, half)?;
                Self::copy_rows(&mut packed, v, chroma_width, chroma_height, half)?;
            }
            Layout::Nv12 => {
                Self::copy_rows(&mut packed, y, width, height, self.stride)?;
                Self::interleave_rows(&mut packed, u, v, chroma_width, chroma_height, self.stride)?;
            }
        }
        Ok(Bytes::from(packed))
    }

    /// Positive `value`, else `fallback`.
    fn or_fallback(value: Option<i32>, fallback: u32) -> u32 {
        value
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(fallback)
    }

    /// Extent error carrying this layout against `frame`.
    fn mismatch(&self, frame: &CpuFrame) -> MediaCodecError {
        MediaCodecError::FrameMismatch {
            width: frame.size().width(),
            height: frame.size().height(),
            expected_width: self.size.width(),
            expected_height: self.size.height(),
        }
    }

    /// Packed byte count for this layout.
    fn packed_len(&self) -> usize {
        let width = usize::try_from(self.size.width()).unwrap_or(usize::MAX);
        let height = usize::try_from(self.size.height()).unwrap_or(usize::MAX);
        let chroma_width = usize::try_from(self.size.width().div_ceil(2)).unwrap_or(usize::MAX);
        let chroma_height = usize::try_from(self.size.height().div_ceil(2)).unwrap_or(usize::MAX);
        let stride = usize::try_from(self.stride).unwrap_or(usize::MAX);
        let y = width.max(stride).saturating_mul(height);
        let chroma = match self.layout {
            Layout::Planar => chroma_width
                .max(stride / 2)
                .saturating_mul(chroma_height)
                .saturating_mul(2),
            Layout::Nv12 => chroma_width
                .saturating_mul(2)
                .max(stride)
                .saturating_mul(chroma_height),
        };
        y.saturating_add(chroma)
    }

    /// Plane rows copied into `dst`, padded to `pitch`.
    fn copy_rows(
        dst: &mut Vec<u8>,
        plane: &Plane,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), MediaCodecError> {
        let stride = Self::checked_stride(plane, width, height)?;
        let row = usize::try_from(width).unwrap_or(usize::MAX);
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let pitch = usize::try_from(pitch).unwrap_or(usize::MAX);
        let pad = pitch.saturating_sub(row);
        let src = plane.data();
        for r in 0..rows {
            let start = r.saturating_mul(stride);
            let end = start.saturating_add(row);
            dst.extend_from_slice(&src[start..end]);
            dst.extend(std::iter::repeat_n(0, pad));
        }
        Ok(())
    }

    /// Chroma rows interleaved into `dst`, padded to `pitch`.
    fn interleave_rows(
        dst: &mut Vec<u8>,
        u: &Plane,
        v: &Plane,
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), MediaCodecError> {
        let u_stride = Self::checked_stride(u, width, height)?;
        let v_stride = Self::checked_stride(v, width, height)?;
        let row = usize::try_from(width).unwrap_or(usize::MAX);
        let rows = usize::try_from(height).unwrap_or(usize::MAX);
        let pitch = usize::try_from(pitch).unwrap_or(usize::MAX);
        let pad = pitch.saturating_sub(row.saturating_mul(2));
        let u_src = u.data();
        let v_src = v.data();
        for r in 0..rows {
            let u_start = r.saturating_mul(u_stride);
            let v_start = r.saturating_mul(v_stride);
            let u_row = &u_src[u_start..u_start.saturating_add(row)];
            let v_row = &v_src[v_start..v_start.saturating_add(row)];
            for (u_byte, v_byte) in u_row.iter().zip(v_row.iter()) {
                dst.push(*u_byte);
                dst.push(*v_byte);
            }
            dst.extend(std::iter::repeat_n(0, pad));
        }
        Ok(())
    }

    /// Plane stride after the stride and length checks.
    fn checked_stride(plane: &Plane, width: u32, height: u32) -> Result<usize, MediaCodecError> {
        planes::check_plane(plane.data(), plane.stride(), width, height).map_err(|fault| {
            match fault {
                planes::PlaneFault::StrideBelowWidth { stride, width } => {
                    MediaCodecError::BufferRange {
                        offset: 0,
                        size: i32::try_from(width).unwrap_or(i32::MAX),
                        len: usize::try_from(stride).unwrap_or(usize::MAX),
                    }
                }
                planes::PlaneFault::ShortOfStride { need, len } => MediaCodecError::BufferRange {
                    offset: 0,
                    size: i32::try_from(need).unwrap_or(i32::MAX),
                    len,
                },
            }
        })?;
        Ok(usize::try_from(plane.stride()).unwrap_or(usize::MAX))
    }
}

/// `MediaCodec` encoder over byte buffers.
#[derive(Debug)]
pub struct MediaCodecEncoder {
    /// Lifecycle gate.
    state: EncodeState,
    /// Configured format.
    spec: EncodeSpec,
    /// Packed input layout.
    input: InputLayout,
    /// Frames queued; stamps the next presentation time.
    frames: u64,
    /// OS codec session.
    backend: Box<dyn CodecSeam>,
}

impl MediaCodecEncoder {
    /// `MediaCodec#BUFFER_FLAG_KEY_FRAME`.
    const BUFFER_FLAG_KEY_FRAME: u32 = 1;
    /// `MediaCodec#BUFFER_FLAG_CODEC_CONFIG`.
    const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;

    /// Encoder in `state` for `spec` with `input` layout and `backend` session.
    pub(crate) fn from_backend(
        state: EncodeState,
        spec: EncodeSpec,
        input: InputLayout,
        backend: Box<dyn CodecSeam>,
    ) -> Self {
        Self {
            state,
            spec,
            input,
            frames: 0,
            backend,
        }
    }

    /// Encoder for `spec`.
    ///
    /// # Errors
    ///
    /// Returns `Err` below the device floor, when the input color has no
    /// plane layout, when the config exceeds the tier, or when no encoder
    /// answers the mime.
    ///
    /// # Arguments
    ///
    /// * `spec` - codec output format.
    ///
    /// # Usage Notes
    ///
    /// Off-Android the stub opens without a device; use fails loud.
    pub fn open(spec: &EncodeSpec) -> Result<Self, MediaCodecError> {
        #[cfg(target_os = "android")]
        {
            Self::open_android(spec)
        }
        #[cfg(not(target_os = "android"))]
        {
            Self::open_stub(spec)
        }
    }

    /// Drained packets, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub fn reset(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.reset_packets()
    }

    /// Lifecycle state.
    #[must_use]
    pub fn state(&self) -> EncodeState {
        self.state
    }

    /// Configured format.
    #[must_use]
    pub fn spec(&self) -> &EncodeSpec {
        &self.spec
    }

    /// Packed input layout.
    #[must_use]
    pub fn input_layout(&self) -> &InputLayout {
        &self.input
    }

    /// Packet from one output range, unless config-only.
    fn packet(bytes: Bytes, flags: u32, pts_us: i64) -> Option<Packet> {
        if flags & Self::BUFFER_FLAG_CODEC_CONFIG != 0 && flags & Self::BUFFER_FLAG_KEY_FRAME == 0 {
            return None;
        }
        let mut packet =
            Packet::new(bytes).with_pts(Duration::from_micros(u64::try_from(pts_us).unwrap_or(0)));
        if flags & Self::BUFFER_FLAG_KEY_FRAME != 0 {
            packet = packet.mark_keyframe();
        }
        Some(packet)
    }

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the frame is not planar 4:2:0
    /// at the configured extent, when no input buffer frees inside the
    /// queue budget, or when the codec faults.
    pub(crate) fn push_frame(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("encode")?;
        let bytes = self.input.pack(frame)?;
        let pts = self.spec.pts_for(self.frames);
        let mut items = self.backend.submit(&bytes, pts)?;
        self.frames += 1;
        items.extend(self.backend.read()?);
        Ok(Self::packets(items))
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub(crate) fn drain_packets(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("flush")?;
        let items = self.backend.read()?;
        Ok(Self::packets(items))
    }

    /// Drained packets, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    pub(crate) fn reset_packets(&mut self) -> Result<Vec<Packet>, MediaCodecError> {
        self.state.ensure_started("reset")?;
        let items = self.backend.reset()?;
        Ok(Self::packets(items))
    }

    /// Encoder marked released.
    pub(crate) fn mark_released(&mut self) {
        self.state = self.state.released().unwrap_or(EncodeState::Released);
    }

    /// Packets over drained `items`, config-only outputs dropped.
    fn packets(items: Vec<SeamItem>) -> Vec<Packet> {
        items
            .into_iter()
            .filter_map(|item| match item {
                SeamItem::Output(output) => {
                    let flags = output.flags();
                    let pts_us = output.pts_us();
                    Self::packet(output.into_bytes(), flags, pts_us)
                }
                SeamItem::Format(_) => None,
            })
            .collect()
    }
}

impl Encoder for MediaCodecEncoder {
    type Error = MediaCodecError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, when the frame is not planar 4:2:0
    /// at the configured extent, or when the codec faults.
    ///
    /// # Arguments
    ///
    /// * `frame` - raw input frame.
    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        self.push_frame(frame)
    }

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` unless started, or when the codec faults.
    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        self.drain_packets()
    }
}

impl Drop for MediaCodecEncoder {
    /// State marked released; the session stops with its fields.
    fn drop(&mut self) {
        self.mark_released();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use rust_video_caps::av1;
    use rust_video_caps::color::{
        ColorConfig, ColorPrimaries, ColorRange, ContentLight, MatrixCoefficients,
        TransferCharacteristics,
    };
    use rust_video_caps::common::{ChromaSampling, PictureSize};
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::h264;
    use rust_video_caps::h265;
    #[cfg(not(target_os = "android"))]
    use rust_video_caps::traits::Encoder;
    use rust_video_caps::vp9;

    use super::{
        EncodeSpec, EncodeState, FormatEntry, InputLayout, MediaCodecEncoder, MediaCodecError,
    };
    use crate::format::FormatSpec;
    use crate::sdk::ApiLevel;

    fn spec_16x16() -> EncodeSpec {
        EncodeSpec::h264(16, 16, 1_000_000, 30, 1).expect("valid spec")
    }

    fn frame_16x16() -> CpuFrame {
        CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 64]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes")
    }

    fn layout_16x16_planar() -> InputLayout {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(19), Some(16), Some(16))
            .expect("valid format");
        layout
    }

    fn entries(spec: &EncodeSpec, level: u32) -> Vec<FormatEntry> {
        spec.format_entries(Some(ApiLevel::new(level)))
            .expect("mapped")
    }

    #[test]
    fn builders_carry_codec_and_rates() {
        let spec = EncodeSpec::h264(1920, 1080, 8_000_000, 60, 2).expect("valid spec");
        assert_eq!(spec.mime(), "video/avc");
        assert_eq!((spec.width(), spec.height()), (1920, 1080));
        assert_eq!(
            spec.config().rate_control,
            RateControl::Cbr {
                bitrate: 8_000_000,
                maxrate: 8_000_000,
                bufsize: 8_000_000,
            }
        );
        assert_eq!(spec.config().frame_rate, FrameRate { num: 60, den: 1 });
        assert_eq!(spec.config().gop.keyint, 120);
        assert_eq!(spec.color_format(), 19);
        let spec = EncodeSpec::h265(3840, 2160, 20_000_000, 30, 1).expect("valid spec");
        assert_eq!(spec.mime(), "video/hevc");
        assert!(matches!(
            spec.config().profile,
            Profile::H265(h265::EncodeProfile::Main)
        ));
        let spec = EncodeSpec::av1(1280, 720, 4_000_000, 30, 0).expect("valid spec");
        assert_eq!(spec.mime(), "video/av01");
        assert_eq!(spec.config().gop.keyint, 1);
        let spec = EncodeSpec::vp9(640, 480, 2_000_000, 30, 1).expect("valid spec");
        assert_eq!(spec.mime(), "video/x-vnd.on2.vp9");
        assert!(matches!(
            spec.config().profile,
            Profile::Vp9(vp9::EncodeProfile::P0)
        ));
        let spec = EncodeSpec::vp8(320, 240, 1_000_000, 30, 1).expect("valid spec");
        assert_eq!(spec.mime(), "video/x-vnd.on2.vp8");
        assert_eq!(spec.config().profile, Profile::Vp8);
        let spec = spec.with_color_format(21);
        assert_eq!(spec.color_format(), 21);
    }

    #[test]
    fn builders_reject_bad_dims_and_rates() {
        assert!(EncodeSpec::h264(0, 1080, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 0, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h265(u32::MAX, 1080, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::av1(1920, u32::MAX, 1_000_000, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 1080, 0, 30, 1).is_none());
        assert!(EncodeSpec::h264(1920, 1080, 1_000_000, 0, 1).is_none());
        assert!(EncodeSpec::vp9(16, 16, 1_000_000, u32::MAX, u32::MAX).is_none());
    }

    #[test]
    fn keys_match_platform_constants() {
        assert_eq!(EncodeSpec::KEY_BIT_RATE, "bitrate");
        assert_eq!(EncodeSpec::KEY_BITRATE_MODE, "bitrate-mode");
        assert_eq!(EncodeSpec::KEY_MAX_BIT_RATE, "max-bitrate");
        assert_eq!(EncodeSpec::KEY_I_FRAME_INTERVAL, "i-frame-interval");
        assert_eq!(EncodeSpec::KEY_PROFILE, "profile");
        assert_eq!(EncodeSpec::KEY_LEVEL, "level");
        assert_eq!(EncodeSpec::KEY_COLOR_STANDARD, "color-standard");
        assert_eq!(EncodeSpec::KEY_COLOR_TRANSFER, "color-transfer");
        assert_eq!(EncodeSpec::KEY_COLOR_RANGE, "color-range");
        assert_eq!(EncodeSpec::KEY_QUALITY, "quality");
    }

    #[test]
    fn min_sdk_gates_av1() {
        let h264 = EncodeSpec::h264(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let h265 = EncodeSpec::h265(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let av1 = EncodeSpec::av1(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let vp9 = EncodeSpec::vp9(16, 16, 1_000_000, 30, 1).expect("valid spec");
        let vp8 = EncodeSpec::vp8(16, 16, 1_000_000, 30, 1).expect("valid spec");
        assert_eq!(h264.min_sdk(), 21);
        assert_eq!(h265.min_sdk(), 21);
        assert_eq!(av1.min_sdk(), 29);
        assert_eq!(vp9.min_sdk(), 21);
        assert_eq!(vp8.min_sdk(), 21);
    }

    #[test]
    fn pts_for_scales_frames() {
        let spec = spec_16x16();
        assert_eq!(spec.pts_for(0), 0);
        assert_eq!(spec.pts_for(30), 1_000_000);
        assert_eq!(spec.pts_for(60), 2_000_000);
        assert_eq!(spec.pts_for(u64::MAX), u64::MAX);
        let config = EncodeConfig {
            frame_rate: FrameRate {
                num: 30_000,
                den: 1001,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(spec.pts_for(1001), 33_400_033);
    }

    #[test]
    fn transitions_walk_the_lifecycle() {
        let state = EncodeState::Unconfigured
            .configured()
            .expect("configure")
            .started()
            .expect("start");
        assert_eq!(state, EncodeState::Started);
        state.ensure_started("encode").expect("started accepts");
        let state = state.released().expect("release");
        assert_eq!(state, EncodeState::Released);
    }

    #[test]
    fn transitions_reject_skips_and_repeats() {
        assert_eq!(
            EncodeState::Unconfigured.started(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "start",
                state: EncodeState::Unconfigured,
            })
        );
        assert_eq!(
            EncodeState::Started.configured(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "configure",
                state: EncodeState::Started,
            })
        );
        assert_eq!(
            EncodeState::Released.released(),
            Err(MediaCodecError::InvalidEncodeState {
                op: "release",
                state: EncodeState::Released,
            })
        );
        assert_eq!(
            EncodeState::Configured.ensure_started("flush"),
            Err(MediaCodecError::InvalidEncodeState {
                op: "flush",
                state: EncodeState::Configured,
            })
        );
    }

    #[test]
    fn tentative_not_negotiated() {
        let layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        assert!(!layout.negotiated());
    }

    #[test]
    fn apply_format_negotiates_layouts() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(21), Some(16), Some(16))
            .expect("semiplanar maps");
        assert!(layout.negotiated());
        let packed = layout.pack(&frame_16x16()).expect("semiplanar packs");
        assert_eq!(packed.len(), 384);
        assert_eq!(&packed[256..260], &[2, 3, 2, 3]);
        layout
            .apply_format(Some(19), None, Some(0))
            .expect("planar maps with fallback");
        let packed = layout.pack(&frame_16x16()).expect("planar packs");
        assert_eq!(packed.len(), 384);
        assert_eq!(&packed[256..258], &[2, 2]);
        assert_eq!(&packed[320..322], &[3, 3]);
    }

    #[test]
    fn apply_format_rejects_bad_color() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        assert_eq!(
            layout.apply_format(Some(7), None, None),
            Err(MediaCodecError::UnsupportedColorFormat { value: 7 })
        );
        assert!(!layout.negotiated());
    }

    #[test]
    fn pack_honors_stride_padding() {
        let mut layout = InputLayout::tentative(&spec_16x16()).expect("valid tentative");
        layout
            .apply_format(Some(19), Some(32), Some(16))
            .expect("valid format");
        let packed = layout.pack(&frame_16x16()).expect("padded pack");
        assert_eq!(packed.len(), 32 * 16 + 2 * 16 * 8);
        assert_eq!(&packed[16..32], &[0_u8; 16]);
        assert_eq!(&packed[32..48], &[1_u8; 16]);
    }

    #[test]
    fn pack_declines_other_layouts() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Nv12,
            vec![
                Plane::new(Bytes::from(vec![0_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
            ],
        )
        .expect("two planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::UnsupportedInput {
                format: PixelFormat::Nv12,
            })
        );
    }

    #[test]
    fn pack_declines_extent_mismatch() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(32, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![0_u8; 512]), 32).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![0_u8; 128]), 16).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::FrameMismatch {
                width: 32,
                height: 16,
                expected_width: 16,
                expected_height: 16,
            })
        );
    }

    #[test]
    fn pack_declines_short_planes() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 16).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 10]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: 64,
                len: 10,
            })
        );
    }

    #[test]
    fn pack_declines_narrow_stride() {
        let layout = layout_16x16_planar();
        let frame = CpuFrame::new(
            PictureSize::new(16, 16).expect("valid size"),
            PixelFormat::Yuv420P,
            vec![
                Plane::new(Bytes::from(vec![1_u8; 256]), 4).expect("valid plane"),
                Plane::new(Bytes::from(vec![2_u8; 64]), 8).expect("valid plane"),
                Plane::new(Bytes::from(vec![3_u8; 64]), 8).expect("valid plane"),
            ],
        )
        .expect("three planes");
        assert_eq!(
            layout.pack(&frame),
            Err(MediaCodecError::BufferRange {
                offset: 0,
                size: 16,
                len: 4,
            })
        );
    }

    #[test]
    fn packet_flags_keyframes_and_config() {
        let bytes = Bytes::from_static(&[0x65, 0x88]);
        let packet = MediaCodecEncoder::packet(bytes.clone(), 1, 40_000).expect("keyframe packet");
        assert!(packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::from_millis(40)));
        let packet = MediaCodecEncoder::packet(bytes.clone(), 0, 80_000).expect("plain packet");
        assert!(!packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::from_millis(80)));
        assert_eq!(MediaCodecEncoder::packet(bytes.clone(), 2, 0), None);
        let packet =
            MediaCodecEncoder::packet(bytes.clone(), 3, -7).expect("config keyframe packet");
        assert!(packet.is_keyframe());
        assert_eq!(packet.pts(), Some(Duration::ZERO));
        let packet = MediaCodecEncoder::packet(bytes, 0, -1).expect("negative stamp packet");
        assert_eq!(packet.pts(), Some(Duration::ZERO));
    }

    #[test]
    fn auto_default_matches_legacy_recipe() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::h264(1280, 720, 4_000_000, 30, 2)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            entries(&spec, 21),
            vec![
                FormatEntry::str(FormatSpec::KEY_MIME, "video/avc"),
                FormatEntry::int(FormatSpec::KEY_WIDTH, 1280),
                FormatEntry::int(FormatSpec::KEY_HEIGHT, 720),
                FormatEntry::int(EncodeSpec::KEY_BIT_RATE, 5_000_000),
                FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 2),
                FormatEntry::int(FormatSpec::KEY_FRAME_RATE, 30),
                FormatEntry::int(FormatSpec::KEY_COLOR_FORMAT, 19),
            ]
        );
    }

    #[test]
    fn legacy_ctor_emits_explicit_cbr() {
        let spec = EncodeSpec::h264(1280, 720, 8_000_000, 60, 2).expect("valid spec");
        assert_eq!(
            entries(&spec, 21),
            vec![
                FormatEntry::str(FormatSpec::KEY_MIME, "video/avc"),
                FormatEntry::int(FormatSpec::KEY_WIDTH, 1280),
                FormatEntry::int(FormatSpec::KEY_HEIGHT, 720),
                FormatEntry::int(EncodeSpec::KEY_BITRATE_MODE, 2),
                FormatEntry::int(EncodeSpec::KEY_BIT_RATE, 8_000_000),
                FormatEntry::int(EncodeSpec::KEY_MAX_BIT_RATE, 8_000_000),
                FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 2),
                FormatEntry::int(FormatSpec::KEY_FRAME_RATE, 60),
                FormatEntry::int(FormatSpec::KEY_COLOR_FORMAT, 19),
            ]
        );
    }

    #[test]
    fn vbr_marks_mode_and_cap() {
        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 2_000_000,
                maxrate: 4_000_000,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            entries(&spec, 21),
            vec![
                FormatEntry::str(FormatSpec::KEY_MIME, "video/avc"),
                FormatEntry::int(FormatSpec::KEY_WIDTH, 16),
                FormatEntry::int(FormatSpec::KEY_HEIGHT, 16),
                FormatEntry::int(EncodeSpec::KEY_BITRATE_MODE, 1),
                FormatEntry::int(EncodeSpec::KEY_BIT_RATE, 2_000_000),
                FormatEntry::int(EncodeSpec::KEY_MAX_BIT_RATE, 4_000_000),
                FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 2),
                FormatEntry::int(FormatSpec::KEY_FRAME_RATE, 30),
                FormatEntry::int(FormatSpec::KEY_COLOR_FORMAT, 19),
            ]
        );
    }

    #[test]
    fn quality_modes_mark_cq_with_quality() {
        for mode in [RateControl::Cqp { qp: 23 }, RateControl::Crf { value: 23 }] {
            let config = EncodeConfig {
                rate_control: mode,
                ..EncodeConfig::default()
            };
            let spec = spec_16x16().with_config(config);
            assert_eq!(
                entries(&spec, 28),
                vec![
                    FormatEntry::str(FormatSpec::KEY_MIME, "video/avc"),
                    FormatEntry::int(FormatSpec::KEY_WIDTH, 16),
                    FormatEntry::int(FormatSpec::KEY_HEIGHT, 16),
                    FormatEntry::int(EncodeSpec::KEY_BITRATE_MODE, 0),
                    FormatEntry::int(EncodeSpec::KEY_QUALITY, 23),
                    FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 2),
                    FormatEntry::int(FormatSpec::KEY_FRAME_RATE, 30),
                    FormatEntry::int(FormatSpec::KEY_COLOR_FORMAT, 19),
                ]
            );
            assert_eq!(
                spec.format_entries(Some(ApiLevel::new(27))),
                Err(MediaCodecError::ConfigDeclined {
                    decline: Decline::RateControlUnsupported { mode },
                })
            );
        }
    }

    #[test]
    fn all_intra_keyint_emits_zero() {
        let spec = EncodeSpec::h264(16, 16, 1_000_000, 30, 0).expect("valid spec");
        assert!(entries(&spec, 21).contains(&FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 0)));
        let config = EncodeConfig {
            gop: Gop {
                keyint: 1,
                min_keyint: 1,
                b_frames: 0,
                refs: 1,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(entries(&spec, 21).contains(&FormatEntry::int(EncodeSpec::KEY_I_FRAME_INTERVAL, 0)));
    }

    #[test]
    fn fractional_cadence_floats_above_25() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 45,
                min_keyint: 45,
                b_frames: 0,
                refs: 1,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(
            entries(&spec, 25).contains(&FormatEntry::float(EncodeSpec::KEY_I_FRAME_INTERVAL, 1.5))
        );
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(24))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::GopInvalid { gop: config.gop },
            })
        );
    }

    #[test]
    fn bframes_and_refs_decline() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 1,
                refs: 1,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::BFramesUnsupported { actual: 1, max: 0 },
            })
        );
        let config = EncodeConfig {
            gop: Gop {
                keyint: 60,
                min_keyint: 60,
                b_frames: 0,
                refs: 2,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::RefsUnsupported { actual: 2, max: 1 },
            })
        );
    }

    #[test]
    fn presets_decline_unless_realtime() {
        for preset in [SpeedPreset::Balanced, SpeedPreset::Quality] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..EncodeConfig::default()
            };
            let spec = spec_16x16().with_config(config);
            assert_eq!(
                spec.format_entries(Some(ApiLevel::new(28))),
                Err(MediaCodecError::ConfigDeclined {
                    decline: Decline::PresetUnsupported { preset },
                })
            );
        }
    }

    #[test]
    fn pinned_level_emits_pair_above_23() {
        let config = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::High),
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L41)),
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        let got = entries(&spec, 23);
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_PROFILE, 8)));
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_LEVEL, 4096)));
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(22))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::LevelUnsupported {
                    level: config.level,
                },
            })
        );
    }

    #[test]
    fn wide_profiles_decline() {
        let config = EncodeConfig {
            profile: Profile::H264(h264::EncodeProfile::High10),
            level: Level::H264(h264::EncodeLevel::Pinned(h264::Level::L41)),
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ProfileUnsupported {
                    profile: config.profile,
                },
            })
        );
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main10),
            level: Level::H265(h265::EncodeLevel::Pinned(h265::Level::L41)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::h265(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ProfileUnsupported {
                    profile: config.profile,
                },
            })
        );
        let config = EncodeConfig {
            profile: Profile::Av1(av1::EncodeProfile::Professional),
            level: Level::Av1(av1::EncodeLevel::Pinned(av1::Level::L40)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::av1(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ProfileUnsupported {
                    profile: config.profile,
                },
            })
        );
    }

    #[test]
    fn level_tables_pin_platform_values() {
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Pinned(h265::Level::L51)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::h265(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        let got = entries(&spec, 28);
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_PROFILE, 1)));
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_LEVEL, 65536)));
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Pinned(h265::Level::L63)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::h265(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::LevelUnsupported {
                    level: config.level,
                },
            })
        );
        let config = EncodeConfig {
            profile: Profile::Av1(av1::EncodeProfile::Main),
            level: Level::Av1(av1::EncodeLevel::Pinned(av1::Level::L40)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::av1(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        let got = entries(&spec, 28);
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_PROFILE, 1)));
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_LEVEL, 256)));
        let config = EncodeConfig {
            profile: Profile::Vp9(vp9::EncodeProfile::P0),
            level: Level::Vp9(vp9::EncodeLevel::Pinned(vp9::Level::L31)),
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::vp9(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        let got = entries(&spec, 28);
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_PROFILE, 1)));
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_LEVEL, 32)));
    }

    #[test]
    fn codec_mismatch_declines() {
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main),
            level: Level::H265(h265::EncodeLevel::Auto),
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ProfileUnsupported {
                    profile: config.profile,
                },
            })
        );
    }

    #[test]
    fn depth_and_chroma_decline_past_tier() {
        let config = EncodeConfig {
            profile: Profile::H265(h265::EncodeProfile::Main10),
            level: Level::H265(h265::EncodeLevel::Auto),
            bit_depth: 10,
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::h265(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::BitDepthUnsupported { actual: 10, max: 8 },
            })
        );
        let config = EncodeConfig {
            profile: Profile::Av1(av1::EncodeProfile::Professional),
            level: Level::Av1(av1::EncodeLevel::Auto),
            chroma: ChromaSampling::Yuv422,
            ..EncodeConfig::default()
        };
        let spec = EncodeSpec::av1(16, 16, 1_000_000, 30, 1)
            .expect("valid spec")
            .with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ChromaUnsupported {
                    chroma: ChromaSampling::Yuv422,
                },
            })
        );
    }

    #[test]
    fn fractional_frame_rate_floats() {
        let config = EncodeConfig {
            frame_rate: FrameRate {
                num: 60_000,
                den: 1001,
            },
            gop: Gop {
                keyint: 60_000,
                min_keyint: 60_000,
                b_frames: 0,
                refs: 1,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        let expected = 60_000_f32 / 1001_f32;
        assert!(
            entries(&spec, 21).contains(&FormatEntry::float(FormatSpec::KEY_FRAME_RATE, expected))
        );
        let config = EncodeConfig {
            frame_rate: FrameRate { num: 60, den: 2 },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(entries(&spec, 21).contains(&FormatEntry::int(FormatSpec::KEY_FRAME_RATE, 30)));
    }

    #[test]
    fn color_full_range_marks_and_gates() {
        let config = EncodeConfig {
            color: ColorConfig {
                range: ColorRange::Full,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(entries(&spec, 24).contains(&FormatEntry::int(EncodeSpec::KEY_COLOR_RANGE, 1)));
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(23))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::RangeUnsupported {
                    range: ColorRange::Full,
                },
            })
        );
    }

    #[test]
    fn color_transfer_and_standard_map() {
        let config = EncodeConfig {
            color: ColorConfig {
                transfer: TransferCharacteristics::Linear,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        let got = entries(&spec, 24);
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_COLOR_TRANSFER, 1)));
        assert!(!got
            .iter()
            .any(|entry| entry.key() == EncodeSpec::KEY_COLOR_STANDARD));
        let config = EncodeConfig {
            color: ColorConfig {
                primaries: ColorPrimaries::Bt2020,
                matrix: MatrixCoefficients::Bt2020Ncl,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(entries(&spec, 24).contains(&FormatEntry::int(EncodeSpec::KEY_COLOR_STANDARD, 6)));
        let config = EncodeConfig {
            color: ColorConfig {
                primaries: ColorPrimaries::Unspecified,
                matrix: MatrixCoefficients::Unspecified,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(entries(&spec, 24).contains(&FormatEntry::int(EncodeSpec::KEY_COLOR_STANDARD, 0)));
    }

    #[test]
    fn color_unmappable_declines() {
        let config = EncodeConfig {
            color: ColorConfig {
                transfer: TransferCharacteristics::Gamma22,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::TransferUnsupported {
                    transfer: TransferCharacteristics::Gamma22,
                },
            })
        );
        let config = EncodeConfig {
            color: ColorConfig {
                transfer: TransferCharacteristics::SmpteSt2084,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::TransferUnsupported {
                    transfer: TransferCharacteristics::SmpteSt2084,
                },
            })
        );
        let config = EncodeConfig {
            color: ColorConfig {
                matrix: MatrixCoefficients::Smpte170M,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::MatrixUnsupported {
                    matrix: MatrixCoefficients::Smpte170M,
                },
            })
        );
        let config = EncodeConfig {
            color: ColorConfig {
                primaries: ColorPrimaries::Film,
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::PrimariesUnsupported {
                    primaries: ColorPrimaries::Film,
                },
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
                ..ColorConfig::default()
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::HdrUnsupported,
            })
        );
    }

    #[test]
    fn invalid_config_declines_structural() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 0,
                min_keyint: 0,
                b_frames: 0,
                refs: 1,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::GopInvalid { gop: config.gop },
            })
        );
        let config = EncodeConfig {
            level: Level::H265(h265::EncodeLevel::Auto),
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(
            spec.format_entries(Some(ApiLevel::new(28))),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::ProfileLevelMismatch {
                    profile: config.profile,
                    level: config.level,
                },
            })
        );
    }

    #[test]
    fn vp8_implicit_profile_omits_keys() {
        let spec = EncodeSpec::vp8(320, 240, 1_000_000, 30, 1).expect("valid spec");
        let got = entries(&spec, 21);
        assert_eq!(
            got[0],
            FormatEntry::str(FormatSpec::KEY_MIME, "video/x-vnd.on2.vp8")
        );
        assert!(!got
            .iter()
            .any(|entry| entry.key() == EncodeSpec::KEY_PROFILE));
        assert!(!got.iter().any(|entry| entry.key() == EncodeSpec::KEY_LEVEL));
    }

    #[test]
    fn with_config_replaces_ctor_config() {
        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 1_000_000,
                maxrate: 2_000_000,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert_eq!(spec.config().rate_control, config.rate_control);
        assert!(entries(&spec, 21).contains(&FormatEntry::int(EncodeSpec::KEY_BITRATE_MODE, 1)));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn stub_open_encodes_loud() {
        let spec = spec_16x16();
        let mut encoder = MediaCodecEncoder::open(&spec).expect("stub opens");
        assert_eq!(encoder.state(), EncodeState::Started);
        assert!(!encoder.input_layout().negotiated());
        assert_eq!(
            encoder.encode(&frame_16x16()),
            Err(MediaCodecError::NoEncoder { mime: "video/avc" })
        );
        assert_eq!(
            encoder.flush(),
            Err(MediaCodecError::NoEncoder { mime: "video/avc" })
        );
        assert_eq!(
            encoder.reset(),
            Err(MediaCodecError::NoEncoder { mime: "video/avc" })
        );
    }

    #[test]
    fn unknown_level_allows_gated_keys() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        let got = spec.format_entries(None).expect("unknown device allows");
        assert!(got.contains(&FormatEntry::int(EncodeSpec::KEY_QUALITY, 23)));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn stub_open_declines_invalid_config() {
        let config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..EncodeConfig::default()
        };
        let spec = spec_16x16().with_config(config);
        assert!(matches!(
            MediaCodecEncoder::open(&spec),
            Err(MediaCodecError::ConfigDeclined {
                decline: Decline::PresetUnsupported {
                    preset: SpeedPreset::Quality,
                },
            })
        ));
    }
}
