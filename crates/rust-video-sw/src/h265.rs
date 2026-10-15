#![allow(unsafe_code)]
//! HEVC encode over ffmpeg's libx265 wrapper (GPL).
//!
//! Enabling links the GPL path (x265 GPL-2+ flips the ffmpeg build GPL); see <https://ffmpeg.org/legal.html>.

use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

use crate::error::SwError;
use crate::ffmpeg_enc::FfmpegEncoder;
use crate::ffmpeg_sys as sys;

/// Speed preset for the x265 wrapper.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum H265Preset {
    /// Fastest realtime tune.
    Ultrafast,
    /// Slower tune, still realtime on desktop.
    Superfast,
    /// Slowest shipped tune, still realtime on desktop.
    Veryfast,
}

impl H265Preset {
    /// Wrapper `preset` string.
    fn as_str(self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
        }
    }
}

/// Realtime HEVC encoder.
pub struct H265Encoder {
    inner: FfmpegEncoder,
}

impl H265Encoder {
    /// CBR encoder for `width`x`height` at `bitrate_kbps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(width: u32, height: u32, bitrate_kbps: u32, threads: u32) -> Result<Self, SwError> {
        Self::open_with_preset(
            width,
            height,
            bitrate_kbps,
            threads,
            H265Preset::Ultrafast,
            60,
        )
    }

    /// CBR encoder with `preset` and a `keyint_frames` keyframe interval.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open_with_preset(
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        threads: u32,
        preset: H265Preset,
        keyint_frames: u32,
    ) -> Result<Self, SwError> {
        if keyint_frames == 0 {
            return Err(SwError::Init);
        }
        FfmpegEncoder::open(c"libx265", width, height, bitrate_kbps, |ctx, bitrate| {
            Self::configure(ctx, width, height, bitrate, threads, preset, keyint_frames)
        })
        .map(|inner| Self { inner })
    }

    /// Context options matching the realtime preset; false on first refusal.
    fn configure(
        ctx: *mut sys::AVCodecContext,
        width: u32,
        height: u32,
        bitrate: i64,
        threads: u32,
        preset: H265Preset,
        keyint_frames: u32,
    ) -> bool {
        let Ok(size) = std::ffi::CString::new(format!("{width}x{height}")) else {
            return false;
        };
        let Ok(x265_params) =
            std::ffi::CString::new(format!("keyint={keyint_frames}:min-keyint={keyint_frames}"))
        else {
            return false;
        };
        let Ok(preset) = std::ffi::CString::new(preset.as_str()) else {
            return false;
        };
        // SAFETY: context is live; option names are valid C strings.
        unsafe {
            sys::av_opt_set(
                ctx.cast::<std::ffi::c_void>(),
                c"video_size".as_ptr(),
                size.as_ptr(),
                0,
            ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"pixel_format".as_ptr(),
                    c"yuv420p".as_ptr(),
                    0,
                ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"time_base".as_ptr(),
                    c"1/60".as_ptr(),
                    0,
                ) == 0
                && sys::av_opt_set_int(ctx.cast::<std::ffi::c_void>(), c"b".as_ptr(), bitrate, 0)
                    == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"maxrate".as_ptr(),
                    bitrate,
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"minrate".as_ptr(),
                    bitrate,
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"bufsize".as_ptr(),
                    bitrate * 2,
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"g".as_ptr(),
                    i64::from(keyint_frames),
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"threads".as_ptr(),
                    i64::from(threads),
                    0,
                ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"preset".as_ptr(),
                    preset.as_ptr(),
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"tune".as_ptr(),
                    c"zerolatency".as_ptr(),
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"x265-params".as_ptr(),
                    x265_params.as_ptr(),
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
        }
    }
}

impl Encoder for H265Encoder {
    type Error = SwError;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        self.inner.encode(frame)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use rust_video_caps::frame::PixelFormat;
    use rust_video_caps::traits::Encoder;

    use super::{H265Encoder, H265Preset};
    use crate::fixtures::{frame_16, frame_64, frame_with_y};

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = H265Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = H265Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = H265Encoder::open(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn preset_open_encodes_keyframe() {
        for preset in [
            H265Preset::Ultrafast,
            H265Preset::Superfast,
            H265Preset::Veryfast,
        ] {
            let mut encoder =
                H265Encoder::open_with_preset(64, 64, 500, 1, preset, 60).expect("encoder");
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

        let mut encoder = H265Encoder::open(64, 64, 500, 2).expect("encoder");
        let mut decoder = H265Decoder::open(2).expect("decoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
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
    fn encode_rejects_size_mismatch() {
        let mut encoder = H265Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(H265Encoder::open(0, 64, 500, 1).is_err());
        assert!(H265Encoder::open(64, 0, 500, 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        assert!(H265Encoder::open(64, 64, 0, 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_keyint() {
        assert!(H265Encoder::open_with_preset(64, 64, 500, 1, H265Preset::Ultrafast, 0).is_err());
    }
}
