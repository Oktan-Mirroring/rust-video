#![allow(unsafe_code)]
//! VP9 encode over ffmpeg's libvpx-vp9 wrapper.
//!
//! Encode only: decode rides the same sys layer, and the wrapper drives the
//! same libvpx underneath, so the coded output keeps its performance.

use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Encoder;

use crate::error::SwError;
use crate::ffmpeg_enc::FfmpegEncoder;
use crate::ffmpeg_sys as sys;

/// Realtime VP9 encoder.
pub struct Vp9Encoder {
    inner: FfmpegEncoder,
}

impl Vp9Encoder {
    /// Speed preset matching the fastest libvpx realtime tune.
    const CPU_USED: i64 = 8;
    /// Realtime deadline, `VPX_DL_REALTIME`.
    const DEADLINE: i64 = 1;
    /// Keyframe interval in frames.
    const GOP: i64 = 60;

    /// CBR encoder for `width`x`height` at `bitrate_kbps` on `threads`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(width: u32, height: u32, bitrate_kbps: u32, threads: u32) -> Result<Self, SwError> {
        FfmpegEncoder::open(
            c"libvpx-vp9",
            width,
            height,
            bitrate_kbps,
            |ctx, bitrate| Self::configure(ctx, width, height, bitrate, threads),
        )
        .map(|inner| Self { inner })
    }

    /// Context options matching the realtime preset; false on first refusal.
    fn configure(
        ctx: *mut sys::AVCodecContext,
        width: u32,
        height: u32,
        bitrate: i64,
        threads: u32,
    ) -> bool {
        let Ok(size) = std::ffi::CString::new(format!("{width}x{height}")) else {
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
                && sys::av_opt_set_int(ctx.cast::<std::ffi::c_void>(), c"g".as_ptr(), Self::GOP, 0)
                    == 0
                && sys::av_opt_set_int(ctx.cast::<std::ffi::c_void>(), c"bf".as_ptr(), 0, 0) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"threads".as_ptr(),
                    i64::from(threads),
                    0,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"cpu-used".as_ptr(),
                    Self::CPU_USED,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"deadline".as_ptr(),
                    Self::DEADLINE,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"lag-in-frames".as_ptr(),
                    0,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
        }
    }
}

impl Encoder for Vp9Encoder {
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

    use super::Vp9Encoder;
    use crate::fixtures::{frame_16, frame_64, frame_with_y};

    #[test]
    fn encode_rejects_short_plane() {
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 63)).is_err());
    }

    #[test]
    fn encode_rejects_oversized_plane() {
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_with_y(64 * 65)).is_err());
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video_caps::traits::Decoder as _;

        use crate::ffmpeg::Vp9Decoder;

        let mut encoder = Vp9Encoder::open(64, 64, 500, 2).expect("encoder");
        let mut decoder = Vp9Decoder::open(2).expect("decoder");
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
        let mut encoder = Vp9Encoder::open(64, 64, 500, 1).expect("encoder");
        assert!(encoder.encode(&frame_16()).is_err());
    }

    #[test]
    fn encode_rejects_zero_geometry() {
        assert!(Vp9Encoder::open(0, 64, 500, 1).is_err());
        assert!(Vp9Encoder::open(64, 0, 500, 1).is_err());
    }

    #[test]
    fn encode_rejects_zero_bitrate() {
        assert!(Vp9Encoder::open(64, 64, 0, 1).is_err());
    }
}
