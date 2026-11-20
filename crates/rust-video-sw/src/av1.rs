#![allow(unsafe_code)]
//! AV1 encode over ffmpeg's `libsvtav1` or `libaom-av1` wrapper.
//!
//! Encode only: decode rides the `libdav1d` wrapper behind the same sys layer.
// Unsafe is the option-setting FFI below: live context, valid C strings.

use std::ffi::{CStr, CString};

use crate::config::{check_color_default, check_common, time_base, FfmpegRate};
use crate::error::SwError;
use crate::ffmpeg_enc::{set_rate, FfmpegEncoder};
use crate::ffmpeg_sys as sys;

use rust_video_caps::av1::EncodeProfile as Av1Profile;
use rust_video_caps::encode::{
    Codec, Decline, EncodeConfig, Level as ConfigLevel, Profile, RateControl, SpeedPreset,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Encoder, KeyframeRequest};
use rust_video_caps::transcode::GpuCaps;

/// Realtime AV1 encoder over `libsvtav1` or `libaom-av1`.
pub struct Av1Encoder {
    /// Shared ffmpeg encode session.
    inner: FfmpegEncoder,
}

/// Resolved `libsvtav1` options behind one open.
struct SvtParams {
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
    /// Worker threads, driving the lookahead-process count.
    threads: u32,
    /// Wrapper preset.
    preset: i64,
}

/// Resolved `libaom-av1` options behind one open.
struct AomParams {
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

impl Av1Encoder {
    /// Preferred encoders in fastest-first order.
    const CANDIDATES: [&'static CStr; 2] = [c"libsvtav1", c"libaom-av1"];
    /// Realtime usage (`AOM_USAGE_REALTIME`).
    const USAGE_REALTIME: i64 = 1;
    /// Maximum constant rate factor the wrappers accept.
    const MAX_CRF: u8 = 63;

    /// Encoder for `width`x`height` under `config` on `threads`.
    ///
    /// Maps the shared config onto the first available AV1 wrapper: rate
    /// control selects bitrate or CRF options, `gop.keyint` sets the keyframe
    /// interval, preset selects the wrapper speed, frame rate sets the time
    /// base. Only the Main profile passes and levels stay auto: neither
    /// wrapper exposes profile or level options. Color stays default: the
    /// tier signals none. B-frames, extra references, CQP, and zero CRF
    /// decline typed; zero CRF declines on both wrappers uniformly because it
    /// is indistinguishable from unset on SVT.
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
        // SVT-AV1 rejects dimensions below 64x64; small extents route to libaom-av1.
        let candidates = if width >= 64 && height >= 64 {
            Self::CANDIDATES.to_vec()
        } else {
            Self::CANDIDATES[1..].to_vec()
        };
        Self::open_candidates(candidates, width, height, config, threads)
    }

    /// Encoder for `width`x`height` under `config` on `threads`, pinned to `libsvtav1`.
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
    pub fn open_libsvt(
        width: u32,
        height: u32,
        config: EncodeConfig,
        threads: u32,
    ) -> Result<Self, SwError> {
        Self::open_candidates(vec![Self::CANDIDATES[0]], width, height, config, threads)
    }

    /// Encoder for `width`x`height` under `config` on `threads`, pinned to `libaom-av1`.
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
    pub fn open_libaom(
        width: u32,
        height: u32,
        config: EncodeConfig,
        threads: u32,
    ) -> Result<Self, SwError> {
        Self::open_candidates(vec![Self::CANDIDATES[1]], width, height, config, threads)
    }

    /// Encoder over `candidates` in preference order.
    ///
    /// # Arguments
    ///
    /// * `candidates` - wrapper names in preference order.
    /// * `width` - coded width in luma samples.
    /// * `height` - coded height in luma samples.
    /// * `config` - shared encode configuration.
    /// * `threads` - worker threads.
    ///
    /// # Errors
    ///
    /// Returns `Err` on structural or tier [`Decline`]s, when the system
    /// library drifts from the bound ABI, or when it rejects the options.
    fn open_candidates(
        candidates: Vec<&'static CStr>,
        width: u32,
        height: u32,
        config: EncodeConfig,
        threads: u32,
    ) -> Result<Self, SwError> {
        check_common(Codec::Av1, &config)?;
        check_color_default(&config.color)?;
        Self::check_profile(config.profile)?;
        Self::check_level(config.level)?;
        let rate = Self::map_rate(config.rate_control)?;
        let coded_time_base = time_base(config.frame_rate).ok_or(SwError::Init)?;
        let svt = SvtParams {
            width,
            height,
            rate,
            keyint: config.gop.keyint,
            time_base: coded_time_base.clone(),
            threads,
            preset: Self::map_preset_svt(config.quality.preset),
        };
        let aom = AomParams {
            width,
            height,
            rate,
            keyint: config.gop.keyint,
            time_base: coded_time_base,
            threads,
            cpu_used: Self::map_preset_aom(config.quality.preset),
        };
        FfmpegEncoder::open_candidates(
            candidates,
            width,
            height,
            rate.bitrate_bps(),
            move |name, ctx, _bitrate| {
                if name == c"libsvtav1" {
                    Self::configure_svt(ctx, &svt)
                } else {
                    Self::configure_aom(ctx, &aom)
                }
            },
        )
        .map(|inner| Self { inner })
    }

    /// Resolved rate options behind `mode`; CQP and zero CRF decline.
    ///
    /// # Arguments
    ///
    /// * `mode` - requested rate control.
    ///
    /// # Errors
    ///
    /// Returns `Err` on CQP, zero CRF, or CRF above the wrapper range.
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
                if value == 0 || value > Self::MAX_CRF {
                    return Err(Decline::RateControlUnsupported { mode }.into());
                }
                Ok(FfmpegRate::Crf { value })
            }
        }
    }

    /// SVT preset behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset_svt(preset: SpeedPreset) -> i64 {
        match preset {
            SpeedPreset::Realtime => 13,
            SpeedPreset::Balanced => 10,
            SpeedPreset::Quality => 8,
        }
    }

    /// AOM `cpu-used` behind `preset`.
    ///
    /// # Arguments
    ///
    /// * `preset` - requested speed preset.
    fn map_preset_aom(preset: SpeedPreset) -> i64 {
        match preset {
            SpeedPreset::Realtime => 8,
            SpeedPreset::Balanced => 4,
            SpeedPreset::Quality => 2,
        }
    }

    /// Rejection unless `profile` is the Main profile.
    ///
    /// # Arguments
    ///
    /// * `profile` - requested encode profile; AV1 Main only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the profile names another codec or needs depth.
    fn check_profile(profile: Profile) -> Result<(), SwError> {
        match profile {
            Profile::Av1(Av1Profile::Main) => Ok(()),
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
            ConfigLevel::Av1(rust_video_caps::av1::EncodeLevel::Auto) => Ok(()),
            _ => Err(Decline::LevelUnsupported { level }.into()),
        }
    }

    /// Rate options behind `rate` on live `ctx`; false on first refusal.
    ///
    /// CBR carries the target alone, preserving the legacy SVT recipe;
    /// quality modes zero `b` explicitly. `crf` carries the preformatted
    /// factor for `Crf`.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `rate` - resolved rate options.
    /// * `crf` - preformatted factor; required for `Crf`.
    fn set_rate_svt(ctx: *mut sys::AVCodecContext, rate: &FfmpegRate, crf: Option<&CStr>) -> bool {
        // SAFETY: context is live; option names are valid C strings.
        unsafe {
            match *rate {
                FfmpegRate::Cbr { bitrate, .. } => sys::opt_int(ctx, c"b", i64::from(bitrate), 0),
                FfmpegRate::Vbr {
                    bitrate,
                    maxrate,
                    bufsize,
                } => {
                    sys::opt_int(ctx, c"b", i64::from(bitrate), 0)
                        && sys::opt_int(ctx, c"maxrate", i64::from(maxrate), 0)
                        && sys::opt_int(ctx, c"bufsize", i64::from(bufsize), 0)
                }
                FfmpegRate::Qp { .. } => false,
                FfmpegRate::Crf { .. } => {
                    let Some(text) = crf else {
                        return false;
                    };
                    sys::opt_int(ctx, c"b", 0, 0)
                        && sys::opt_str(ctx, c"crf", text, sys::OPT_SEARCH_CHILDREN)
                }
            }
        }
    }

    /// Context options for `libsvtav1` low-delay realtime; false on refusal.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `params` - resolved options.
    fn configure_svt(ctx: *mut sys::AVCodecContext, params: &SvtParams) -> bool {
        let Ok(size) = CString::new(format!("{}x{}", params.width, params.height)) else {
            return false;
        };
        let lp = params.threads.clamp(1, 6);
        let Ok(svt_params) = CString::new(format!("lookahead=0:lp={lp}")) else {
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
                && Self::set_rate_svt(ctx, &params.rate, crf.as_deref())
                && sys::opt_int(ctx, c"g", i64::from(params.keyint), 0)
                && sys::opt_int(ctx, c"preset", params.preset, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_str(
                    ctx,
                    c"svtav1-params",
                    svt_params.as_c_str(),
                    sys::OPT_SEARCH_CHILDREN,
                )
        }
    }

    /// Context options for `libaom-av1` `usage=realtime`; false on refusal.
    ///
    /// # Arguments
    ///
    /// * `ctx` - live encoder context.
    /// * `params` - resolved options.
    fn configure_aom(ctx: *mut sys::AVCodecContext, params: &AomParams) -> bool {
        let Ok(size) = CString::new(format!("{}x{}", params.width, params.height)) else {
            return false;
        };
        let tile_cols = if params.width >= 512 && params.threads > 1 {
            2
        } else {
            0
        };
        let tile_rows = u8::from(params.height >= 512 && params.threads > 2);
        let tiles = if tile_cols == 0 && tile_rows == 0 {
            None
        } else {
            let Ok(tiles) = CString::new(format!("{tile_cols}x{tile_rows}")) else {
                return false;
            };
            Some(tiles)
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
                && sys::opt_int(ctx, c"cpu-used", params.cpu_used, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(
                    ctx,
                    c"usage",
                    Self::USAGE_REALTIME,
                    sys::OPT_SEARCH_CHILDREN,
                )
                && sys::opt_int(ctx, c"lag-in-frames", 0, sys::OPT_SEARCH_CHILDREN)
                && sys::opt_int(ctx, c"row-mt", 1, sys::OPT_SEARCH_CHILDREN)
                && (match &tiles {
                    None => true,
                    Some(tiles) => {
                        sys::opt_str(ctx, c"tiles", tiles.as_c_str(), sys::OPT_SEARCH_CHILDREN)
                    }
                })
        }
    }
}

impl Encoder for Av1Encoder {
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

impl KeyframeRequest for Av1Encoder {
    type Error = SwError;

    fn request_keyframe(&mut self) -> Result<(), Self::Error> {
        self.inner.request_keyframe();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rust_video_caps::av1::{EncodeLevel, EncodeProfile};
    use rust_video_caps::color::{ColorPrimaries, ContentLight};
    use rust_video_caps::common::ChromaSampling;
    use rust_video_caps::encode::{
        Decline, EncodeConfig, FrameRate, Gop, Level, Profile, Quality, RateControl, SpeedPreset,
    };
    use rust_video_caps::frame::PixelFormat;
    use rust_video_caps::traits::{Encoder, KeyframeRequest};

    use super::Av1Encoder;
    use crate::error::SwError;
    use crate::fixtures::{frame_16, frame_64, frame_noise_64, frame_with_y};

    /// AV1 config with a CBR target of `bps` at 60 fps and a 60-frame GOP.
    fn cbr(bps: u32) -> EncodeConfig {
        EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: bps,
                maxrate: bps,
                bufsize: bps,
            },
            frame_rate: FrameRate { num: 60, den: 1 },
            profile: Profile::Av1(EncodeProfile::Main),
            level: Level::Av1(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    /// Default config retargeted at AV1 Main auto.
    fn av1() -> EncodeConfig {
        EncodeConfig {
            profile: Profile::Av1(EncodeProfile::Main),
            level: Level::Av1(EncodeLevel::Auto),
            ..Default::default()
        }
    }

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = Av1Encoder::open(64, 64, av1(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = Av1Encoder::open(64, 64, av1(), 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Av1Encoder::open(64, 64, av1(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Av1Decoder;

        let mut encoder = Av1Encoder::open(64, 64, av1(), 2).expect("encoder");
        let mut decoder = Av1Decoder::open(2).expect("decoder");
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
        let mut low = Av1Encoder::open(64, 64, cbr(500_000), 1).expect("encoder");
        let mut high = Av1Encoder::open(64, 64, cbr(2_000_000), 1).expect("encoder");
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
        let mut fast = Av1Encoder::open(64, 64, av1(), 1).expect("encoder");
        let slow_config = EncodeConfig {
            quality: Quality {
                preset: SpeedPreset::Quality,
            },
            ..av1()
        };
        let mut slow = Av1Encoder::open(64, 64, slow_config, 1).expect("encoder");
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
    fn pinned_svt_encodes_keyframe() {
        let mut encoder = Av1Encoder::open_libsvt(64, 64, av1(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn pinned_aom_encodes_keyframe() {
        let mut encoder = Av1Encoder::open_libaom(64, 64, av1(), 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    // Lab-gated: needs system libavcodec.
    #[test]
    fn pinned_svt_keyframe_request_forces_single_keyframe() {
        let mut encoder = Av1Encoder::open_libsvt(64, 64, cbr(500_000), 1).expect("encoder");
        let keyed = forced_keyed(&mut encoder);
        assert_eq!(keyed.len(), 4, "one packet per frame: {keyed:?}");
        assert!(keyed[0], "first packet keys: {keyed:?}");
        assert!(!keyed[1], "second packet coasts: {keyed:?}");
        assert!(keyed[2], "forced packet keys: {keyed:?}");
        assert!(!keyed[3], "request is single-shot: {keyed:?}");
    }

    // Lab-gated: needs system libavcodec.
    #[test]
    fn pinned_aom_keyframe_request_forces_single_keyframe() {
        let mut encoder = Av1Encoder::open_libaom(64, 64, cbr(500_000), 1).expect("encoder");
        let keyed = forced_keyed(&mut encoder);
        assert_eq!(keyed.len(), 4, "one packet per frame: {keyed:?}");
        assert!(keyed[0], "first packet keys: {keyed:?}");
        assert!(!keyed[1], "second packet coasts: {keyed:?}");
        assert!(keyed[2], "forced packet keys: {keyed:?}");
        assert!(!keyed[3], "request is single-shot: {keyed:?}");
    }

    /// Keyframe flags over four flat frames with a request before the third.
    fn forced_keyed(encoder: &mut Av1Encoder) -> Vec<bool> {
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
    fn pinned_svt_roundtrip() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Av1Decoder;

        let mut encoder = Av1Encoder::open_libsvt(64, 64, av1(), 2).expect("encoder");
        let mut decoder = Av1Decoder::open(2).expect("decoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        let mut frames = Vec::new();
        for packet in &packets {
            frames.extend(decoder.decode(packet).expect("decode"));
        }
        frames.extend(decoder.flush().expect("flush"));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].format(), PixelFormat::Yuv420P);
    }

    #[test]
    fn pinned_aom_roundtrip() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Av1Decoder;

        let mut encoder = Av1Encoder::open_libaom(64, 64, av1(), 2).expect("encoder");
        let mut decoder = Av1Decoder::open(2).expect("decoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        let mut frames = Vec::new();
        for packet in &packets {
            frames.extend(decoder.decode(packet).expect("decode"));
        }
        frames.extend(decoder.flush().expect("flush"));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].format(), PixelFormat::Yuv420P);
    }

    #[test]
    fn pinned_aom_crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 30 },
            ..av1()
        };
        let mut encoder = Av1Encoder::open_libaom(64, 64, config, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn pinned_svt_crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 35 },
            ..av1()
        };
        let mut encoder = Av1Encoder::open_libsvt(64, 64, config, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn crf_encodes_keyframe() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 30 },
            ..av1()
        };
        let mut encoder = Av1Encoder::open(64, 64, config, 1).expect("encoder");
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
            ..av1()
        };
        let mut encoder = Av1Encoder::open(64, 64, config, 1).expect("encoder");
        let packets = encoder.encode(&frame_64()).expect("encode");
        assert_ne!(packets.len(), 0);
    }

    #[test]
    fn foreign_codec_profile_declines() {
        assert!(matches!(
            Av1Encoder::open(64, 64, EncodeConfig::default(), 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn professional_profile_declines() {
        let config = EncodeConfig {
            profile: Profile::Av1(EncodeProfile::Professional),
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::ProfileUnsupported { .. },
            })
        ));
    }

    #[test]
    fn pinned_level_declines() {
        let config = EncodeConfig {
            level: Level::Av1(EncodeLevel::Pinned(rust_video_caps::av1::Level::L40)),
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::LevelUnsupported { .. },
            })
        ));
    }

    #[test]
    fn ten_bit_depth_declines() {
        let config = EncodeConfig {
            profile: Profile::Av1(EncodeProfile::Professional),
            bit_depth: 10,
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BitDepthUnsupported { actual: 10, max: 8 },
            })
        ));
    }

    #[test]
    fn non_420_chroma_declines() {
        let config = EncodeConfig {
            chroma: ChromaSampling::Yuv444,
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
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
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::BFramesUnsupported { actual: 2, max: 0 },
            })
        ));
    }

    #[test]
    fn cqp_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Cqp { qp: 23 },
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn zero_crf_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 0 },
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn wild_crf_declines() {
        let config = EncodeConfig {
            rate_control: RateControl::Crf { value: 64 },
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlUnsupported { .. },
            })
        ));
    }

    #[test]
    fn unsignaled_color_declines() {
        let mut color = av1().color;
        color.primaries = ColorPrimaries::Smpte170M;
        let config = EncodeConfig { color, ..av1() };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::PrimariesUnsupported { .. },
            })
        ));
    }

    #[test]
    fn hdr_color_declines() {
        let mut color = av1().color;
        color.light = Some(ContentLight {
            max_cll: 1000,
            max_fall: 400,
        });
        let config = EncodeConfig { color, ..av1() };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::HdrUnsupported,
            })
        ));
    }

    #[test]
    fn encode_rejects_size_mismatch() {
        let mut encoder = Av1Encoder::open(64, 64, av1(), 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(Av1Encoder::open(0, 64, av1(), 1).is_err());
        assert!(Av1Encoder::open(64, 0, av1(), 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        let config = EncodeConfig {
            rate_control: RateControl::Cbr {
                bitrate: 0,
                maxrate: 5_000_000,
                bufsize: 5_000_000,
            },
            ..av1()
        };
        assert!(matches!(
            Av1Encoder::open(64, 64, config, 1),
            Err(SwError::Declined {
                decline: Decline::RateControlInvalid { .. },
            })
        ));
    }
}
