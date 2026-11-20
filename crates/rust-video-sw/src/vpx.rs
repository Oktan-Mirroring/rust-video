// FFI boundary: libvpx option calls through the sys layer live here.
#![allow(unsafe_code)]
//! VP8/VP9 encode over ffmpeg's libvpx wrappers.
//!
//! Encode only: decode rides the same sys layer, and the wrappers drive the
//! same libvpx underneath, so the coded output keeps its performance.

use std::ffi::CString;

use crate::config::{check_color_default, check_common, time_base, FfmpegRate};
use crate::error::SwError;
use crate::ffmpeg_enc::{set_rate, FfmpegEncoder};
use crate::ffmpeg_sys as sys;

use rust_video_caps::encode::{
    Codec, Decline, EncodeConfig, Level as ConfigLevel, RateControl, SpeedPreset,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Encoder, KeyframeRequest};
use rust_video_caps::transcode::GpuCaps;
use rust_video_caps::vp9::{EncodeLevel, Level as Vp9Level};

/// Realtime VP8 encoder over ffmpeg's `libvpx` wrapper.
pub struct Vp8Encoder {
    /// Shared ffmpeg encode session.
    inner: FfmpegEncoder,
}

/// Resolved `libvpx` options behind one open.
struct Vp8Params {
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Resolved rate options.
    rate: FfmpegRate,
    /// Maximum keyframe interval in frames.
    keyint: u32,
    /// Coded `time_base` value.
    time_base: CString,
    /// Worker threads.
    threads: u32,
    /// Speed preset as `cpu-used`.
    cpu_used: i64,
}

impl Vp8Encoder {
    /// Maximum constant rate factor the wrapper accepts.
    const MAX_CRF: u8 = 63;

    /// Encoder for `width`x`height` under `config` on `threads`.
    ///
    /// Maps the shared config onto the `libvpx` wrapper: rate control selects
    /// bitrate or CRF options, `gop.keyint` sets the keyframe interval,
    /// preset selects `cpu-used`, frame rate sets the time base. VP8 carries
    /// an implicit 8-bit 4:2:0 profile and level, so only those pass; color
    /// stays default: the tier signals none. B-frames, extra references, and
    /// CQP decline typed.
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
        check_common(Codec::Vp8, &config)?;
        check_color_default(&config.color)?;
        let rate = Self::map_rate(config.rate_control)?;
        let params = Vp8Params {
            width,
            height,
            rate,
            keyint: config.gop.keyint,
            time_base: time_base(config.frame_rate).ok_or(SwError::Init)?,
            threads,
            cpu_used: Self::map_preset(config.quality.preset),
        };
        FfmpegEncoder::open(
            c"libvpx",
            width,
            height,
            rate.bitrate_bps(),
            move |ctx, _bitrate| Self::configure(ctx, &params),
        )
        .map(|inner| Self { inner })
    }

    /// Resolved rate options behind `mode`; CQP has no wrapper knob.
    ///
    /// # Arguments
    ///
    /// * `mode` - requested rate control.
    ///
    /// # Errors
    ///
    /// Returns `Err` on CQP or CRF above the wrapper range.
    fn map_rate(mode: RateControl) -> Result<FfmpegRate, SwError> {
        match mode {
            RateControl::Auto => Ok(FfmpegRate::auto()),
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => Ok(FfmpegRate::cbr(bitrate, maxrate, bufsize)),
            RateControl::Vbr { bitrate, maxrate } => Ok(FfmpegRate::vbr(bitrate, maxrate)),
            RateControl::Cqp { .. } => Err(Decline::RateControlUnsupported { mode }.into()),
            RateControl::Crf { value } => {
                if value > Self::MAX_CRF {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(FfmpegRate::Crf { value })
            }
        }
    }

    /// `cpu-used` behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset(preset: SpeedPreset) -> i64 {
        match preset {
            SpeedPreset::Realtime => 8,
            SpeedPreset::Balanced => 4,
            SpeedPreset::Quality => 2,
        }
    }

    /// Context options matching the realtime preset; false on first refusal.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `params` - resolved options.
    fn configure(ctx: *mut sys::AVCodecContext, params: &Vp8Params) -> bool {
        let Ok(size) = CString::new(format!("{}x{}", params.width, params.height)) else {
            return false;
        };
        let crf = match params.rate {
            FfmpegRate::Crf { value } => match CString::new(format!("{value}")) {
                Ok(text) => Some(text),
                Err(_) => return false,
            },
            _ => None,
        };
        let slices = i64::from(params.threads.clamp(1, 4));
        // SAFETY: context is live; option names are valid C strings.
        unsafe {
            sys::opt_str(ctx, c"video_size", size.as_c_str(), 0)
                && sys::opt_str(ctx, c"pixel_format", c"yuv420p", 0)
                && sys::opt_str(ctx, c"time_base", params.time_base.as_c_str(), 0)
                && set_rate(ctx, &params.rate, crf.as_deref())
                && sys::opt_int(ctx, c"g", i64::from(params.keyint), 0)
                && sys::opt_int(ctx, c"bf", 0, 0)
                && sys::opt_int(ctx, c"slices", slices, 0)
                && sys::opt_int(ctx, c"threads", i64::from(params.threads), 0)
                && sys::opt_int(ctx, c"cpu-used", params.cpu_used, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"deadline", 1, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"lag-in-frames", 0, sys::OPT_SEARCH_CHILDREN)
        }
    }
}

impl Encoder for Vp8Encoder {
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

impl KeyframeRequest for Vp8Encoder {
    type Error = SwError;

    fn request_keyframe(&mut self) -> Result<(), Self::Error> {
        self.inner.request_keyframe();
        Ok(())
    }
}

/// Realtime VP9 encoder.
pub struct Vp9Encoder {
    /// Shared ffmpeg encode session.
    inner: FfmpegEncoder,
}

/// Resolved `libvpx-vp9` options behind one open.
struct Vp9Params {
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Resolved rate options.
    rate: FfmpegRate,
    /// Maximum keyframe interval in frames.
    keyint: u32,
    /// Wrapper level name, absent for auto.
    level: Option<&'static str>,
    /// Coded `time_base` value.
    time_base: CString,
    /// Worker threads.
    threads: u32,
    /// Speed preset as `cpu-used`.
    cpu_used: i64,
}

impl Vp9Encoder {
    /// Maximum constant rate factor the wrapper accepts.
    const MAX_CRF: u8 = 63;

    /// Encoder for `width`x`height` under `config` on `threads`.
    ///
    /// Maps the shared config onto the `libvpx-vp9` wrapper: rate control
    /// selects bitrate or CRF options, `gop.keyint` sets the keyframe
    /// interval, preset selects `cpu-used`, level pins the wrapper level,
    /// frame rate sets the time base. Only the 8-bit 4:2:0 profile passes;
    /// color stays default: the tier signals none. B-frames, extra
    /// references, and CQP decline typed.
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
        check_common(Codec::Vp9, &config)?;
        check_color_default(&config.color)?;
        let rate = Self::map_rate(config.rate_control)?;
        let params = Vp9Params {
            width,
            height,
            rate,
            keyint: config.gop.keyint,
            level: Self::map_level(config.level)?,
            time_base: time_base(config.frame_rate).ok_or(SwError::Init)?,
            threads,
            cpu_used: Self::map_preset(config.quality.preset),
        };
        FfmpegEncoder::open(
            c"libvpx-vp9",
            width,
            height,
            rate.bitrate_bps(),
            move |ctx, _bitrate| Self::configure(ctx, &params),
        )
        .map(|inner| Self { inner })
    }

    /// Resolved rate options behind `mode`; CQP has no wrapper knob.
    ///
    /// # Arguments
    ///
    /// * `mode` - requested rate control.
    ///
    /// # Errors
    ///
    /// Returns `Err` on CQP or CRF above the wrapper range.
    fn map_rate(mode: RateControl) -> Result<FfmpegRate, SwError> {
        match mode {
            RateControl::Auto => Ok(FfmpegRate::auto()),
            RateControl::Cbr {
                bitrate,
                maxrate,
                bufsize,
            } => Ok(FfmpegRate::cbr(bitrate, maxrate, bufsize)),
            RateControl::Vbr { bitrate, maxrate } => Ok(FfmpegRate::vbr(bitrate, maxrate)),
            RateControl::Cqp { .. } => Err(Decline::RateControlUnsupported { mode }.into()),
            RateControl::Crf { value } => {
                if value > Self::MAX_CRF {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(FfmpegRate::Crf { value })
            }
        }
    }

    /// `cpu-used` behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset(preset: SpeedPreset) -> i64 {
        match preset {
            SpeedPreset::Realtime => 8,
            SpeedPreset::Balanced => 4,
            SpeedPreset::Quality => 2,
        }
    }

    /// Wrapper level name behind `level`; auto leaves backend selection.
    ///
    /// Non-P0 profiles decline earlier through the depth and chroma gates:
    /// every one needs non-8-bit or non-4:2:0 input.
    ///
    /// # Arguments
    ///
    /// * `level` - requested encode level; VP9 only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the level names another codec.
    fn map_level(level: ConfigLevel) -> Result<Option<&'static str>, SwError> {
        match level {
            ConfigLevel::Vp9(EncodeLevel::Auto) => Ok(None),
            ConfigLevel::Vp9(EncodeLevel::Pinned(pinned)) => Ok(Some(match pinned {
                Vp9Level::L1 => "1",
                Vp9Level::L11 => "1.1",
                Vp9Level::L2 => "2",
                Vp9Level::L21 => "2.1",
                Vp9Level::L3 => "3",
                Vp9Level::L31 => "3.1",
                Vp9Level::L4 => "4",
                Vp9Level::L41 => "4.1",
                Vp9Level::L5 => "5",
                Vp9Level::L51 => "5.1",
                Vp9Level::L52 => "5.2",
                Vp9Level::L6 => "6",
                Vp9Level::L61 => "6.1",
                Vp9Level::L62 => "6.2",
            })),
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// Context options matching the realtime preset; false on first refusal.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `params` - resolved options.
    fn configure(ctx: *mut sys::AVCodecContext, params: &Vp9Params) -> bool {
        let Ok(size) = CString::new(format!("{}x{}", params.width, params.height)) else {
            return false;
        };
        let level = match params.level {
            None => None,
            Some(text) => match CString::new(text) {
                Ok(value) => Some(value),
                Err(_) => return false,
            },
        };
        let crf = match params.rate {
            FfmpegRate::Crf { value } => match CString::new(format!("{value}")) {
                Ok(text) => Some(text),
                Err(_) => return false,
            },
            _ => None,
        };
        let tile_cols = if params.width >= 512 && params.threads > 1 {
            2
        } else {
            0
        };
        // SAFETY: context is live; option names are valid C strings.
        unsafe {
            sys::opt_str(ctx, c"video_size", size.as_c_str(), 0)
                && sys::opt_str(ctx, c"pixel_format", c"yuv420p", 0)
                && sys::opt_str(ctx, c"time_base", params.time_base.as_c_str(), 0)
                && set_rate(ctx, &params.rate, crf.as_deref())
                && sys::opt_int(ctx, c"g", i64::from(params.keyint), 0)
                && sys::opt_int(ctx, c"bf", 0, 0)
                && sys::opt_int(ctx, c"threads", i64::from(params.threads), 0)
                && sys::opt_int(ctx, c"cpu-used", params.cpu_used, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"deadline", 1, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"lag-in-frames", 0, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"row-mt", 1, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"tile-columns", tile_cols, sys::OPT_SEARCH_CHILDREN)
                && match &level {
                    None => true,
                    Some(text) => {
                        sys::opt_str(ctx, c"level", text.as_c_str(), sys::OPT_SEARCH_CHILDREN)
                    }
                }
        }
    }
}

impl Encoder for Vp9Encoder {
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

impl KeyframeRequest for Vp9Encoder {
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
    use rust_video_caps::traits::{Encoder, KeyframeRequest};
    use rust_video_caps::vp9::{EncodeLevel, EncodeProfile};

    use super::{Vp8Encoder, Vp9Encoder};
    use crate::error::SwError;
    use crate::fixtures::{frame_16, frame_64, frame_noise_64, frame_with_y};

    /// VP8 config with a CBR target of `bps` at 60 fps and a 60-frame GOP.
    fn vp8_cbr(bps: u32) -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: bps,
                maxrate: bps,
                bufsize: bps,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            profile: Profile::Vp8,
            level: Level::Vp8,
            ..Default::default()
        }
    }

    /// VP9 config with a CBR target of `bps` at 60 fps and a 60-frame GOP.
    fn vp9_cbr(bps: u32) -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: bps,
                maxrate: bps,
                bufsize: bps,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            profile: Profile::Vp9(EncodeProfile::P0),
            level: Level::Vp9(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    /// Default config retargeted at VP8.
    fn vp8() -> EncodeConfig {
        EncodeConfig {
            profile: Profile::Vp8,
            level: Level::Vp8,
            ..Default::default()
        }
    }

    /// Default config retargeted at VP9 P0 auto.
    fn vp9() -> EncodeConfig {
        EncodeConfig {
            profile: Profile::Vp9(EncodeProfile::P0),
            level: Level::Vp9(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = Vp9Encoder::open(64, 64, vp9(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = Vp9Encoder::open(64, 64, vp9(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Vp9Encoder::open(64, 64, vp9(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    // Lab-gated: needs system libavcodec.
    #[test]
    fn vp8_keyframe_request_forces_single_keyframe() {
        let mut encoder = Vp8Encoder::open(64, 64, vp8_cbr(500_000), 1).expect("encoder");
        let keyed = forced_keyed(&mut encoder);
        assert_eq!(keyed.len(), 4, "one packet per frame: {keyed:?}");
        assert!(keyed[0], "first packet keys: {keyed:?}");
        assert!(!keyed[1], "second packet coasts: {keyed:?}");
        assert!(keyed[2], "forced packet keys: {keyed:?}");
        assert!(!keyed[3], "request is single-shot: {keyed:?}");
    }

    // Lab-gated: needs system libavcodec.
    #[test]
    fn vp9_keyframe_request_forces_single_keyframe() {
        let mut encoder = Vp9Encoder::open(64, 64, vp9_cbr(500_000), 1).expect("encoder");
        let keyed = forced_keyed(&mut encoder);
        assert_eq!(keyed.len(), 4, "one packet per frame: {keyed:?}");
        assert!(keyed[0], "first packet keys: {keyed:?}");
        assert!(!keyed[1], "second packet coasts: {keyed:?}");
        assert!(keyed[2], "forced packet keys: {keyed:?}");
        assert!(!keyed[3], "request is single-shot: {keyed:?}");
    }

    /// Keyframe flags over four flat frames with a request before the third.
    fn forced_keyed<E>(encoder: &mut E) -> Vec<bool>
    where
        E: Encoder<Error = SwError> + KeyframeRequest<Error = SwError>,
    {
        let mut packets = Vec::new();
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        encoder.request_keyframe().expect("request");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        packets.iter().map(|packet| packet.is_keyframe()).collect()
    }

    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Vp9Decoder;

        let mut encoder = Vp9Encoder::open(64, 64, vp9(), 2).expect("encoder");
        let mut decoder = Vp9Decoder::open(2).expect("decoder");
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
        let mut low = Vp9Encoder::open(64, 64, vp9_cbr(500_000), 1).expect("encoder");
        let mut high = Vp9Encoder::open(64, 64, vp9_cbr(2_000_000), 1).expect("encoder");
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
        let mut fast = Vp9Encoder::open(64, 64, vp9(), 1).expect("encoder");
        let slow_config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..vp9()
        };
        let mut slow = Vp9Encoder::open(64, 64, slow_config, 1).expect("encoder");
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
    fn pinned_level_opens() {
        let config = EncodeConfig {
            level: Level::Vp9(EncodeLevel::Pinned(rust_video_caps::vp9::Level::L41)),
            ..vp9()
        };
        let mut encoder = Vp9Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(packets.len(), 0);
    }

    #[test]
    fn crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 30 },
            ..vp9()
        };
        let mut encoder = Vp9Encoder::open(64, 64, config, 1).expect("encoder");
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
            ..vp9()
        };
        let mut encoder = Vp9Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(packets.len(), 0);
    }

    #[test]
    fn tier_chroma_gate_declines() {
        let config = EncodeConfig {
            profile: Profile::Vp9(EncodeProfile::P1),
            chroma: ChromaSampling::Yuv444,
            ..vp9()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ChromaUnsupported { .. },
            })
        ));
    }

    #[test]
    fn ten_bit_depth_declines() {
        let config = EncodeConfig {
            profile: Profile::Vp9(EncodeProfile::P2),
            level: Level::Vp9(EncodeLevel::Auto),
            bit_depth: 10,
            ..Default::default()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BitDepthUnsupported { actual: 10, max: 8 },
            })
        ));
    }

    #[test]
    fn cqp_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..vp9()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn wild_crf_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 64 },
            ..vp9()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn b_frames_decline() {
        let config = EncodeConfig {
            gop: Gop {
                b_frames: 1,
                ..Default::default()
            },
            ..vp9()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BFramesUnsupported { actual: 1, max: 0 },
            })
        ));
    }

    #[test]
    fn unsignaled_color_declines() {
        let mut color = vp9().color;
        color.primaries = ColorPrimaries::Smpte170M;
        let config = EncodeConfig { color, ..vp9() };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::PrimariesUnsupported { .. },
            })
        ));
    }

    #[test]
    fn hdr_color_declines() {
        let mut color = vp9().color;
        color.light = Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        });
        let config = EncodeConfig { color, ..vp9() };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::HdrUnsupported,
            })
        ));
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = Vp9Encoder::open(64, 64, vp9(), 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(Vp9Encoder::open(0, 64, vp9(), 1).is_err());
        assert!(Vp9Encoder::open(64, 0, vp9(), 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..vp9()
        };
        assert!(matches!(
            Vp9Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlInvalid { .. },
            })
        ));
    }

    #[test]
    fn vp8_encode_rejects_short_plane() {
        let mut encoder = Vp8Encoder::open(64, 64, vp8(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn vp8_encode_rejects_oversized_plane() {
        let mut encoder = Vp8Encoder::open(64, 64, vp8(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn vp8_encode_marks_first_packet_keyframe() {
        let mut encoder = Vp8Encoder::open(64, 64, vp8(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn vp8_roundtrip_through_ffmpeg_decoder() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Vp8Decoder;

        let mut encoder = Vp8Encoder::open(64, 64, vp8(), 2).expect("encoder");
        let mut decoder = Vp8Decoder::open(2).expect("decoder");
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
    fn vp8_bitrate_changes_output() {
        let mut low = Vp8Encoder::open(64, 64, vp8_cbr(500_000), 1).expect("encoder");
        let mut high = Vp8Encoder::open(64, 64, vp8_cbr(2_000_000), 1).expect("encoder");
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
    fn vp8_preset_changes_output() {
        let mut fast = Vp8Encoder::open(64, 64, vp8(), 1).expect("encoder");
        let slow_config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..vp8()
        };
        let mut slow = Vp8Encoder::open(64, 64, slow_config, 1).expect("encoder");
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
    fn vp8_crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 30 },
            ..vp8()
        };
        let mut encoder = Vp8Encoder::open(64, 64, config, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn vp8_cqp_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..vp8()
        };
        assert!(matches!(
            Vp8Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn vp8_foreign_codec_profile_declines() {
        assert!(matches!(
            Vp8Encoder::open(64, 64, EncodeConfig::default(), 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn vp8_ten_bit_depth_declines() {
        let config = EncodeConfig {
            bit_depth: 10,
            ..vp8()
        };
        assert!(matches!(
            Vp8Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BitDepthMismatch { .. },
            })
        ));
    }

    #[test]
    fn vp8_non_420_chroma_declines() {
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..vp8()
        };
        assert!(matches!(
            Vp8Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ChromaUnsupported { .. },
            })
        ));
    }

    #[test]
    fn vp8_encode_rejects_size_mismatch() {
        let mut encoder = Vp8Encoder::open(64, 64, vp8(), 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn vp8_encode_rejects_zero_geometry() {
        assert!(Vp8Encoder::open(0, 64, vp8(), 1).is_err());
        assert!(Vp8Encoder::open(64, 0, vp8(), 1).is_err());
    }

    #[test]
    fn vp8_encode_rejects_zero_bitrate() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..vp8()
        };
        assert!(matches!(
            Vp8Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlInvalid { .. },
            })
        ));
    }
}
