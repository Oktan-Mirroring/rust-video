//! Shared [`EncodeConfig`] validation behind the software encoders.
//!
//! Structural checks run before any FFI call, so declines never link.

#[cfg(feature = "ffmpeg")]
use std::ffi::CString;

use crate::error::SwError;

#[cfg(feature = "ffmpeg")]
use rust_video_caps::color::{
    ColorConfig, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristics,
};
use rust_video_caps::common::ChromaSampling;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::encode::FrameRate;
use rust_video_caps::encode::{Codec, Decline, EncodeConfig};

/// Legacy CBR target behind [`RateControl::Auto`], shared by the openh264,
/// x264, x265, vpx, aom, and svt opens.
///
/// [`RateControl::Auto`]: rust_video_caps::encode::RateControl::Auto
pub(crate) const DEFAULT_AUTO_BITRATE: u32 = 5_000_000;

/// Rejection unless `config` fits the 8-bit 4:2:0 single-ref tier for `codec`.
///
/// Runs structural validation, then the tier gates every software encoder
/// shares: profile names `codec`, 8-bit depth, 4:2:0 chroma, zero B-frames,
/// at most one reference. GOP cadence, rate, preset, level, frame rate, and
/// color map per backend after this passes.
///
/// # Arguments
///
/// * `codec` - encoder codec family; profile must name it.
/// * `config` - candidate encode configuration.
///
/// # Errors
///
/// Returns `Err` on the first structural [`Decline`] or tier gate failure.
pub(crate) fn check_common(codec: Codec, config: &EncodeConfig) -> Result<(), SwError> {
    config.validate()?;
    if config.profile.codec() != codec {
        return Err(Decline::ProfileUnsupported {
            profile: config.profile,
        }
        .into());
    }
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
    if config.gop.b_frames != 0 {
        return Err(Decline::BFramesUnsupported {
            actual: config.gop.b_frames,
            max: 0,
        }
        .into());
    }
    if config.gop.refs > 1 {
        return Err(Decline::RefsUnsupported {
            actual: config.gop.refs,
            max: 1,
        }
        .into());
    }
    Ok(())
}

#[cfg(feature = "ffmpeg")]
/// Rejection unless `color` is BT.709 limited-range SDR without HDR metadata.
///
/// The ffmpeg wrappers signal no color, so only the default description the
/// decoders assume passes; openh264 maps its wider VUI set separately.
///
/// # Arguments
///
/// * `color` - candidate color description.
///
/// # Errors
///
/// Returns `Err` on the first non-default primary, transfer, matrix, range,
/// or HDR metadata attachment.
pub(crate) fn check_color_default(color: &ColorConfig) -> Result<(), SwError> {
    if color.primaries != ColorPrimaries::Bt709 {
        return Err(Decline::PrimariesUnsupported {
            primaries: color.primaries,
        }
        .into());
    }
    if color.transfer != TransferCharacteristics::Bt709 {
        return Err(Decline::TransferUnsupported {
            transfer: color.transfer,
        }
        .into());
    }
    if color.matrix != MatrixCoefficients::Bt709 {
        return Err(Decline::MatrixUnsupported {
            matrix: color.matrix,
        }
        .into());
    }
    if color.range != ColorRange::Limited {
        return Err(Decline::RangeUnsupported { range: color.range }.into());
    }
    if color.is_hdr() {
        return Err(Decline::HdrUnsupported.into());
    }
    Ok(())
}

#[cfg(feature = "ffmpeg")]
/// Coded `time_base` value for `rate`, `None` on interior NUL.
///
/// # Arguments
///
/// * `rate` - frame rate; numerator and denominator validated nonzero.
pub(crate) fn time_base(rate: FrameRate) -> Option<CString> {
    CString::new(format!("{}/{}", rate.den, rate.num)).ok()
}

#[cfg(feature = "ffmpeg")]
/// Resolved ffmpeg rate options behind one open.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum FfmpegRate {
    /// Constant bitrate at target with cap, floor, and buffer.
    Cbr {
        /// Target bitrate in bps.
        bitrate: u32,
        /// Maximum bitrate in bps.
        maxrate: u32,
        /// Minimum bitrate in bps.
        minrate: u32,
        /// Buffer size in bits.
        bufsize: u32,
    },
    /// Variable bitrate at target with a peak cap.
    Vbr {
        /// Target bitrate in bps.
        bitrate: u32,
        /// Maximum bitrate in bps.
        maxrate: u32,
        /// Buffer size in bits.
        bufsize: u32,
    },
    /// Constant quantization parameter.
    Qp {
        /// Quantization parameter.
        qp: u8,
    },
    /// Constant rate factor.
    Crf {
        /// Rate factor value.
        value: u8,
    },
}

#[cfg(feature = "ffmpeg")]
impl FfmpegRate {
    /// Legacy CBR recipe at the shared default bitrate.
    pub(crate) fn auto() -> Self {
        Self::Cbr {
            bitrate: DEFAULT_AUTO_BITRATE,
            maxrate: DEFAULT_AUTO_BITRATE,
            minrate: DEFAULT_AUTO_BITRATE,
            bufsize: DEFAULT_AUTO_BITRATE * 2,
        }
    }

    /// Strict CBR at `bitrate` with `maxrate` cap and `bufsize` buffer.
    ///
    /// Floors at target: the minimum equals the target.
    ///
    /// # Arguments
    ///
    /// * `bitrate` - target bitrate in bps.
    /// * `maxrate` - maximum bitrate in bps.
    /// * `bufsize` - buffer size in bits.
    pub(crate) fn cbr(bitrate: u32, maxrate: u32, bufsize: u32) -> Self {
        Self::Cbr {
            bitrate,
            maxrate,
            minrate: bitrate,
            bufsize,
        }
    }

    /// VBR at `bitrate` capped at `maxrate` with a cap-sized buffer.
    ///
    /// No floor: the encoder dips below target on easy content.
    ///
    /// # Arguments
    ///
    /// * `bitrate` - target bitrate in bps.
    /// * `maxrate` - maximum bitrate in bps.
    pub(crate) fn vbr(bitrate: u32, maxrate: u32) -> Self {
        Self::Vbr {
            bitrate,
            maxrate,
            bufsize: maxrate,
        }
    }

    /// Target bitrate in bps; zero for quality modes.
    pub(crate) fn bitrate_bps(&self) -> u32 {
        match *self {
            Self::Cbr { bitrate, .. } | Self::Vbr { bitrate, .. } => bitrate,
            Self::Qp { .. } | Self::Crf { .. } => 0,
        }
    }
}
