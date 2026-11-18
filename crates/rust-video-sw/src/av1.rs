#![allow(unsafe_code)]
//! AV1 encode over ffmpeg's `libsvtav1` or `libaom-av1` wrapper.
//!
//! Encode only: decode rides the `libdav1d` wrapper behind the same sys layer.
// Unsafe is the option-setting FFI below: live context, valid C strings.

use std::ffi::CStr;

use crate::error::SwError;
use crate::ffmpeg_enc::FfmpegEncoder;
use crate::ffmpeg_sys as sys;

use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

/// Realtime AV1 encoder over `libsvtav1` or `libaom-av1`.
pub struct Av1Encoder {
    /// Shared ffmpeg encode session.
    inner: FfmpegEncoder,
}

impl Av1Encoder {
    /// Preferred encoders in fastest-first order.
    const CANDIDATES: [&'static CStr; 2] = [c"libsvtav1", c"libaom-av1"];
    /// SVT-AV1 fastest realtime preset.
    const SVT_PRESET: i64 = 13;
    /// Fastest libaom realtime speed tune exposed by libavcodec.
    const AOM_CPU_USED: i64 = 8;
    /// Realtime usage (`AOM_USAGE_REALTIME`).
    const USAGE_REALTIME: i64 = 1;
    /// Keyframe interval in frames.
    const GOP: i64 = 60;

    /// CBR encoder for `width`x`height` at `bitrate_kbps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(width: u32, height: u32, bitrate_kbps: u32, threads: u32) -> Result<Self, SwError> {
        // SVT-AV1 rejects dimensions below 64x64; small extents route to libaom-av1.
        let candidates = if width >= 64 && height >= 64 {
            Self::CANDIDATES.to_vec()
        } else {
            Self::CANDIDATES[1..].to_vec()
        };
        Self::open_candidates(candidates, width, height, bitrate_kbps, threads)
    }

    /// CBR encoder pinned to `libsvtav1`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open_libsvt(
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        threads: u32,
    ) -> Result<Self, SwError> {
        Self::open_candidates(
            vec![Self::CANDIDATES[0]],
            width,
            height,
            bitrate_kbps,
            threads,
        )
    }

    /// CBR encoder pinned to `libaom-av1`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open_libaom(
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        threads: u32,
    ) -> Result<Self, SwError> {
        Self::open_candidates(
            vec![Self::CANDIDATES[1]],
            width,
            height,
            bitrate_kbps,
            threads,
        )
    }

    /// CBR encoder over `candidates` in preference order.
    fn open_candidates(
        candidates: Vec<&'static CStr>,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        threads: u32,
    ) -> Result<Self, SwError> {
        FfmpegEncoder::open_candidates(
            candidates,
            width,
            height,
            bitrate_kbps,
            move |name, ctx, bitrate| {
                if name == c"libsvtav1" {
                    Self::configure_svt(ctx, width, height, bitrate, threads)
                } else {
                    Self::configure_aom(ctx, width, height, bitrate, threads)
                }
            },
        )
        .map(|inner| Self { inner })
    }

    /// Context options for `libsvtav1` low-delay realtime; false on refusal.
    fn configure_svt(
        ctx: *mut sys::AVCodecContext,
        width: u32,
        height: u32,
        bitrate: i64,
        threads: u32,
    ) -> bool {
        let Ok(size) = std::ffi::CString::new(format!("{width}x{height}")) else {
            return false;
        };
        let lp = threads.clamp(1, 6);
        let Ok(svt_params) = std::ffi::CString::new(format!("lookahead=0:lp={lp}")) else {
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
                && sys::av_opt_set_int(ctx.cast::<std::ffi::c_void>(), c"g".as_ptr(), Self::GOP, 0)
                    == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"preset".as_ptr(),
                    Self::SVT_PRESET,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set(
                    ctx.cast::<std::ffi::c_void>(),
                    c"svtav1-params".as_ptr(),
                    svt_params.as_ptr(),
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
        }
    }

    /// Context options for `libaom-av1` `usage=realtime`; false on refusal.
    fn configure_aom(
        ctx: *mut sys::AVCodecContext,
        width: u32,
        height: u32,
        bitrate: i64,
        threads: u32,
    ) -> bool {
        let Ok(size) = std::ffi::CString::new(format!("{width}x{height}")) else {
            return false;
        };
        let tile_cols = if width >= 512 && threads > 1 { 2 } else { 0 };
        let tile_rows = u8::from(height >= 512 && threads > 2);
        let tiles = if tile_cols == 0 && tile_rows == 0 {
            None
        } else {
            let Ok(tiles) = std::ffi::CString::new(format!("{tile_cols}x{tile_rows}")) else {
                return false;
            };
            Some(tiles)
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
                && sys::av_opt_set_int(ctx.cast::<std::ffi::c_void>(), c"g".as_ptr(), Self::GOP, 0)
                    == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"threads".as_ptr(),
                    i64::from(threads),
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"cpu-used".as_ptr(),
                    Self::AOM_CPU_USED,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"usage".as_ptr(),
                    Self::USAGE_REALTIME,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"lag-in-frames".as_ptr(),
                    0,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"row-mt".as_ptr(),
                    1,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && (match &tiles {
                    None => true,
                    Some(tiles) => {
                        sys::av_opt_set(
                            ctx.cast::<std::ffi::c_void>(),
                            c"tiles".as_ptr(),
                            tiles.as_ptr(),
                            sys::OPT_SEARCH_CHILDREN,
                        ) == 0
                    }
                })
        }
    }
}

impl Encoder for Av1Encoder {
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

    use super::Av1Encoder;
    use crate::fixtures::{frame_16, frame_64, frame_with_y};

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
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

        let mut encoder = Av1Encoder::open(64, 64, 500, 2).expect("encoder");
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
    fn pinned_svt_encodes_keyframe() {
        let mut encoder = Av1Encoder::open_libsvt(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn pinned_aom_encodes_keyframe() {
        let mut encoder = Av1Encoder::open_libaom(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.flush().expect("flush"));
        assert_ne!(packets, []);
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn pinned_svt_roundtrip() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Av1Decoder;

        let mut encoder = Av1Encoder::open_libsvt(64, 64, 500, 2).expect("encoder");
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

        let mut encoder = Av1Encoder::open_libaom(64, 64, 500, 2).expect("encoder");
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
    fn encode_rejects_size_mismatch() {
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(Av1Encoder::open(0, 64, 500, 1).is_err());
        assert!(Av1Encoder::open(64, 0, 500, 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        assert!(Av1Encoder::open(64, 64, 0, 1).is_err());
    }
}
