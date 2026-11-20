#![allow(unsafe_code)]
//! HEVC encode over ffmpeg's libx265 wrapper (GPL).
//!
//! Enabling links the GPL path (x265 GPL-2+ flips the ffmpeg build GPL); see <https://ffmpeg.org/legal.html>.
// Unsafe is the option-setting FFI below: live context, valid C strings.

use std::ffi::CString;

use crate::config::{check_color_default, check_common, time_base, FfmpegRate};
use crate::error::SwError;
use crate::ffmpeg_enc::{set_rate, FfmpegEncoder};
use crate::ffmpeg_sys as sys;

use rust_video_caps::encode::{
    Codec, Decline, EncodeConfig, Level as ConfigLevel, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::h265::EncodeProfile as H265Profile;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Encoder, KeyframeRequest};
use rust_video_caps::transcode::GpuCaps;

/// HEVC encoder over `libx265`; throughput unmeasured.
pub struct H265Encoder {
    /// Shared ffmpeg encode session.
    inner: FfmpegEncoder,
}

/// Resolved libx265 options behind one open.
struct X265Params {
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Resolved rate options.
    rate: FfmpegRate,
    /// Maximum keyframe interval in frames.
    keyint: u32,
    /// Minimum keyframe interval in frames.
    min_keyint: u32,
    /// Wrapper preset name.
    preset: &'static str,
    /// Wrapper profile name.
    profile: &'static str,
    /// Coded `time_base` value.
    time_base: CString,
    /// Worker threads.
    threads: u32,
}

impl H265Encoder {
    /// Maximum constant-QP the wrapper accepts.
    const MAX_QP: u8 = 51;
    /// Maximum constant rate factor the wrapper accepts.
    const MAX_CRF: u8 = 51;

    /// Encoder for `width`x`height` under `config` on `threads`.
    ///
    /// Maps the shared config onto the libx265 wrapper: rate control selects
    /// bitrate, QP, or CRF options, `gop` sets the keyframe interval plus
    /// x265 keyint bounds, preset selects the wrapper preset, profile pins
    /// the wrapper restriction, frame rate sets the time base. Pinned levels
    /// decline: the wrapper exposes no level option, so the tier auto-selects.
    /// Color stays default: the tier signals none. B-frames, extra
    /// references, and 10-bit profiles decline typed.
    ///
    /// # Arguments
    ///
    /// * `width` - coded width in luma samples; nonzero.
    /// * `height` - coded height in luma samples; nonzero.
    /// * `config` - shared encode configuration.
    /// * `threads` - worker threads; zero resolves inside ffmpeg.
    ///
    /// # Errors
    ///
    /// Returns `Err` on structural or tier [`Decline`]s, when the system
    /// library drifts from the bound ABI, or when it rejects the options.
    pub fn open(
        width: u32,
        height: u32,
        config: EncodeConfig,
        threads: u32,
    ) -> Result<Self, SwError> {
        check_common(Codec::H265, &config)?;
        check_color_default(&config.color)?;
        Self::check_level(config.level)?;
        let rate = Self::map_rate(config.rate_control)?;
        let params = X265Params {
            width,
            height,
            rate,
            keyint: config.gop.keyint,
            min_keyint: config.gop.min_keyint,
            preset: Self::map_preset(config.quality.preset),
            profile: Self::map_profile(config.profile)?,
            time_base: time_base(config.frame_rate).ok_or(SwError::Init)?,
            threads,
        };
        FfmpegEncoder::open(
            c"libx265",
            width,
            height,
            rate.bitrate_bps(),
            move |ctx, _bitrate| Self::configure(ctx, &params),
        )
        .map(|inner| Self { inner })
    }

    /// Resolved rate options behind `mode`.
    ///
    /// # Arguments
    ///
    /// * `mode` - requested rate control.
    ///
    /// # Errors
    ///
    /// Returns `Err` on QP or CRF above the wrapper range.
    fn map_rate(mode: RateControl) -> Result<FfmpegRate, SwError> {
        match mode {
            RateControl::Auto => Ok(FfmpegRate::auto()),
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => Ok(FfmpegRate::cbr(bitrate, maxrate, bufsize)),
            RateControl::Vbr { bitrate, maxrate } => Ok(FfmpegRate::vbr(bitrate, maxrate)),
            RateControl::Cqp { qp } => {
                if qp > Self::MAX_QP {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(FfmpegRate::Qp { qp })
            }
            RateControl::Crf { value } => {
                if value > Self::MAX_CRF {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(FfmpegRate::Crf { value })
            }
        }
    }

    /// Wrapper preset name behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset(preset: SpeedPreset) -> &'static str {
        match preset {
            SpeedPreset::Realtime => "ultrafast",
            SpeedPreset::Balanced => "superfast",
            SpeedPreset::Quality => "veryfast",
        }
    }

    /// Wrapper profile name behind `profile`; 10-bit profiles decline.
    ///
    /// # Arguments
    ///
    /// * `profile` - requested encode profile; H.265 Main only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the profile names another codec or needs 10-bit.
    fn map_profile(profile: Profile) -> Result<&'static str, SwError> {
        match profile {
            Profile::H265(H265Profile::Main) => Ok("main"),
            _ => Err(Decline::ProfileUnsupported { profile }.into()),
        }
    }

    /// Rejection unless `level` leaves selection to the backend.
    ///
    /// # Arguments
    ///
    /// * `level` - requested encode level; auto only.
    ///
    /// # Errors
    ///
    /// Returns `Err` on pinned levels or levels naming another codec.
    fn check_level(level: ConfigLevel) -> Result<(), SwError> {
        match level {
            ConfigLevel::H265(rust_video_caps::h265::EncodeLevel::Auto) => Ok(()),
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// Context options matching the `zerolatency` tune; false on first refusal.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `params` - resolved options.
    fn configure(ctx: *mut sys::AVCodecContext, params: &X265Params) -> bool {
        let Ok(size) = CString::new(format!("{}x{}", params.width, params.height)) else {
            return false;
        };
        let Ok(x265_params) = CString::new(format!(
            "keyint={}:min-keyint={}:repeat-headers=1:log-level=error",
            params.keyint, params.min_keyint
        )) else {
            return false;
        };
        let Ok(preset) = CString::new(params.preset) else {
            return false;
        };
        let Ok(profile) = CString::new(params.profile) else {
            return false;
        };
        let crf = match params.rate {
            FfmpegRate::Crf { value } => match CString::new(format!("{value}")) {
                Ok(text) => Some(text),
                Err(_) => return false,
            },
            _ => None,
        };
        // SAFETY: context is live; option names are valid C strings.
        unsafe {
            sys::opt_str(ctx, c"video_size", size.as_c_str(), 0)
                && sys::opt_str(ctx, c"pixel_format", c"yuv420p", 0)
                && sys::opt_str(ctx, c"time_base", params.time_base.as_c_str(), 0)
                && set_rate(ctx, &params.rate, crf.as_deref())
                && sys::opt_int(ctx, c"g", i64::from(params.keyint), 0)
                && sys::opt_int(ctx, c"threads", i64::from(params.threads), 0)
                && sys::opt_str(ctx, c"preset", preset.as_c_str(), sys::OPT_SEARCH_CHILDREN)
                && sys::opt_str(ctx, c"tune", c"zerolatency", sys::OPT_SEARCH_CHILDREN)
                && sys::opt_str(
                    ctx,
                    c"x265-params",
                    x265_params.as_c_str(),
                    sys::OPT_SEARCH_CHILDREN,
                )
                && sys::opt_str(
                    ctx,
                    c"profile",
                    profile.as_c_str(),
                    sys::OPT_SEARCH_CHILDREN,
                )
        }
    }
}

impl Encoder for H265Encoder {
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        self.inner.encode(frame)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        self.inner.flush()
    }
}

impl KeyframeRequest for H265Encoder {
    type Error = SwError;

    fn request_keyframe(&mut self) -> Result<(), Self::Error> {
        self.inner.request_keyframe();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rust_video_caps::color::{ColorPrimaries, ContentLight};
    use rust_video_caps::common::ChromaSampling;
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::frame::PixelFormat;
    use rust_video_caps::h265::{EncodeLevel, EncodeProfile};
    use rust_video_caps::traits::{Encoder, KeyframeRequest};

    use super::H265Encoder;
    use crate::error::SwError;
    use crate::fixtures::{frame_16, frame_64, frame_noise_64, frame_with_y};

    /// H.265 config with a CBR target of `bps` at 60 fps and a 60-frame GOP.
    fn cbr(bps: u32) -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: bps,
                maxrate: bps,
                bufsize: bps,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            profile: Profile::H265(EncodeProfile::Main),
            level: Level::H265(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    /// Default config retargeted at H.265 Main auto.
    fn hevc() -> EncodeConfig {
        EncodeConfig {
            profile: Profile::H265(EncodeProfile::Main),
            level: Level::H265(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = H265Encoder::open(64, 64, hevc(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = H265Encoder::open(64, 64, hevc(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = H265Encoder::open(64, 64, hevc(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    // Lab-gated: needs system libavcodec.
    #[test]
    fn keyframe_request_forces_single_idr() {
        let mut encoder = H265Encoder::open(64, 64, cbr(500_000), 1).expect("encoder");
        let mut packets = Vec::new();
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        encoder.request_keyframe().expect("request");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        let keyed: Vec<bool> = packets.iter().map(|packet| packet.is_keyframe()).collect();
        assert_eq!(keyed.len(), 4, "one packet per frame: {keyed:?}");
        assert!(keyed[0], "first packet keys: {keyed:?}");
        assert!(!keyed[1], "second packet coasts: {keyed:?}");
        assert!(keyed[2], "forced packet keys: {keyed:?}");
        assert!(!keyed[3], "request is single-shot: {keyed:?}");
    }

    #[test]
    fn preset_configs_encode_keyframe() {
        for preset in [
            SpeedPreset::Realtime,
            SpeedPreset::Balanced,
            SpeedPreset::Quality,
        ] {
            let config = EncodeConfig {
                quality: Quality { preset },
                ..hevc()
            };
            let mut encoder = H265Encoder::open(64, 64, config, 1).expect("encoder");
            let mut packets = encoder.encode(&frame_64()).expect("encode");
            packets.extend(encoder.flush().expect("flush"));
            assert!(!packets.is_empty(), "{preset:?}");
            assert!(packets[0].is_keyframe(), "{preset:?}");
        }
    }

    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::H265Decoder;

        let mut encoder = H265Encoder::open(64, 64, hevc(), 2).expect("encoder");
        let mut decoder = H265Decoder::open(2).expect("decoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
        let mut frames = Vec::new();
        for packet in &packets {
            frames.extend(decoder.decode(packet).expect("decode"));
        }
        frames.extend(decoder.flush().expect("flush"));
        assert_eq!(frames.len(), 2);
        for frame in &frames {
            assert_eq!(frame.format(), PixelFormat::Yuv420P);
            assert_eq!(frame.size().width(), 64);
            assert_eq!(frame.size().height(), 64);
        }
    }

    #[test]
    fn bitrate_changes_output() {
        let mut low = H265Encoder::open(64, 64, cbr(500_000), 1).expect("encoder");
        let mut high = H265Encoder::open(64, 64, cbr(2_000_000), 1).expect("encoder");
        let frame = frame_noise_64(7);
        let mut low_packets = low.encode(&frame).expect("encode");
        low_packets.extend(low.flush().expect("flush"));
        let mut high_packets = high.encode(&frame).expect("encode");
        high_packets.extend(high.flush().expect("flush"));
        let low_bytes: Vec<u8> = low_packets.iter().flat_map(|p| p.data().to_vec()).collect();
        let high_bytes: Vec<u8> = high_packets
            .iter()
            .flat_map(|p| p.data().to_vec())
            .collect();
        assert_ne!(low_bytes, high_bytes);
    }

    #[test]
    fn preset_changes_output() {
        let mut fast = H265Encoder::open(64, 64, hevc(), 1).expect("encoder");
        let slow_config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..hevc()
        };
        let mut slow = H265Encoder::open(64, 64, slow_config, 1).expect("encoder");
        let frame = frame_noise_64(11);
        let mut fast_packets = fast.encode(&frame).expect("encode");
        fast_packets.extend(fast.flush().expect("flush"));
        let mut slow_packets = slow.encode(&frame).expect("encode");
        slow_packets.extend(slow.flush().expect("flush"));
        let fast_bytes: Vec<u8> = fast_packets
            .iter()
            .flat_map(|p| p.data().to_vec())
            .collect();
        let slow_bytes: Vec<u8> = slow_packets
            .iter()
            .flat_map(|p| p.data().to_vec())
            .collect();
        assert_ne!(fast_bytes, slow_bytes);
    }

    #[test]
    fn cqp_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..hevc()
        };
        let mut encoder = H265Encoder::open(64, 64, config, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 23 },
            ..hevc()
        };
        let mut encoder = H265Encoder::open(64, 64, config, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn vbr_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Vbr {
                bitrate: 500_000,
                maxrate: 1_000_000,
            },
            ..hevc()
        };
        let mut encoder = H265Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(packets.len(), 0);
    }

    #[test]
    fn auto_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Auto,
            ..hevc()
        };
        let mut encoder = H265Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(packets.len(), 0);
    }

    #[test]
    fn foreign_codec_profile_declines() {
        assert!(matches!(
            H265Encoder::open(64, 64, EncodeConfig::default(), 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn main10_profile_declines() {
        let config = EncodeConfig {
            profile: Profile::H265(EncodeProfile::Main10),
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn pinned_level_declines() {
        let config = EncodeConfig {
            level: Level::H265(EncodeLevel::Pinned(rust_video_caps::h265::Level::L41)),
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::LevelUnsupported { .. },
            })
        ));
    }

    #[test]
    fn ten_bit_depth_declines() {
        let config = EncodeConfig {
            profile: Profile::H265(EncodeProfile::Main10),
            bit_depth: 10,
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BitDepthUnsupported { actual: 10, max: 8 },
            })
        ));
    }

    #[test]
    fn non_420_chroma_declines() {
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ChromaUnsupported { .. },
            })
        ));
    }

    #[test]
    fn b_frames_decline() {
        let config = EncodeConfig {
            gop: Gop {
                b_frames: 2,
                ..Default::default()
            },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BFramesUnsupported { actual: 2, max: 0 },
            })
        ));
    }

    #[test]
    fn extra_refs_decline() {
        let config = EncodeConfig {
            gop: Gop {
                refs: 3,
                ..Default::default()
            },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RefsUnsupported { actual: 3, max: 1 },
            })
        ));
    }

    #[test]
    fn wild_qp_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 60 },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn wild_crf_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 60 },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn unsignaled_color_declines() {
        let mut color = hevc().color;
        color.primaries = ColorPrimaries::Smpte170M;
        let config = EncodeConfig { color, ..hevc() };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::PrimariesUnsupported { .. },
            })
        ));
    }

    #[test]
    fn hdr_color_declines() {
        let mut color = hevc().color;
        color.light = Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        });
        let config = EncodeConfig { color, ..hevc() };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::HdrUnsupported,
            })
        ));
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = H265Encoder::open(64, 64, hevc(), 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(H265Encoder::open(0, 64, hevc(), 1).is_err());
        assert!(H265Encoder::open(64, 0, hevc(), 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlInvalid { .. },
            })
        ));
    }

    #[test]
    fn encode_rejects_zero_keyint() {
        let config = EncodeConfig {
            gop: Gop {
                keyint: 0,
                min_keyint: 60,
                b_frames: 0,
                refs: 1,
            },
            ..hevc()
        };
        assert!(matches!(
            H265Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::GopInvalid { .. },
            })
        ));
    }
}
