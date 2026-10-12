#![allow(unsafe_code)]
//! AV1 encode over ffmpeg's libaom-av1 wrapper.
//!
//! Encode only: decode rides the libdav1d wrapper behind the same sys layer.

use std::ptr;

use bytes::Bytes;

use rust_video::frame::{CpuFrame, PixelFormat};
use rust_video::packet::Packet;
use rust_video::traits::Encoder;

use rust_video_caps::common::PictureSize;

use crate::error::SwError;
use crate::ffmpeg_sys as sys;

/// Realtime AV1 encoder.
pub struct Av1Encoder {
    ctx: *mut sys::AVCodecContext,
    frame: *mut sys::AVFrame,
    packet: *mut sys::AVPacket,
    width: u32,
    height: u32,
    next_pts: i64,
}

impl Av1Encoder {
    /// Fastest libaom speed tune.
    const CPU_USED: i64 = 8;
    /// Realtime usage, `realtime`.
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
        // SAFETY: version queries take no arguments and touch no state.
        if unsafe { sys::avcodec_version() } >> 16 != sys::ABI_MAJOR
            || unsafe { sys::avutil_version() } >> 16 != sys::UTIL_MAJOR
        {
            return Err(SwError::Init);
        }
        if PictureSize::new(width, height).is_none() {
            return Err(SwError::Init);
        }
        let bitrate = i64::from(bitrate_kbps) * 1000;
        if bitrate <= 0 {
            return Err(SwError::Init);
        }
        // SAFETY: name is a valid C string; null is checked.
        let codec = unsafe { sys::avcodec_find_encoder_by_name(c"libaom-av1".as_ptr()) };
        if codec.is_null() {
            return Err(SwError::Init);
        }
        // SAFETY: codec is live; null is checked.
        let ctx = unsafe { sys::avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            return Err(SwError::Init);
        }
        let configured = Self::configure(ctx, width, height, bitrate, threads);
        // SAFETY: context and codec are live; null options select defaults.
        let opened = configured && unsafe { sys::avcodec_open2(ctx, codec, ptr::null_mut()) } == 0;
        // SAFETY: blank frame or null; null is checked.
        let frame = if opened {
            unsafe { sys::av_frame_alloc() }
        } else {
            ptr::null_mut()
        };
        // SAFETY: blank packet or null; null is checked.
        let packet = if frame.is_null() {
            ptr::null_mut()
        } else {
            unsafe { sys::av_packet_alloc() }
        };
        if !opened || frame.is_null() || packet.is_null() {
            // SAFETY: non-null slots are live; null slots are skipped.
            unsafe { Self::release(ctx, frame, packet) };
            return Err(SwError::Init);
        }
        Ok(Self {
            ctx,
            frame,
            packet,
            width,
            height,
            next_pts: 0,
        })
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
                    Self::CPU_USED,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
                && sys::av_opt_set_int(
                    ctx.cast::<std::ffi::c_void>(),
                    c"usage".as_ptr(),
                    Self::USAGE_REALTIME,
                    sys::OPT_SEARCH_CHILDREN,
                ) == 0
        }
    }

    /// Partially built state released; null slots are skipped.
    ///
    /// # Safety
    ///
    /// Each non-null pointer must come from its allocator and be freed once.
    unsafe fn release(
        ctx: *mut sys::AVCodecContext,
        frame: *mut sys::AVFrame,
        packet: *mut sys::AVPacket,
    ) {
        if !packet.is_null() {
            let mut packet = packet;
            // SAFETY: packet is live by the contract above.
            unsafe { sys::av_packet_free(&raw mut packet) };
        }
        if !frame.is_null() {
            let mut frame = frame;
            // SAFETY: frame is live by the contract above.
            unsafe { sys::av_frame_free(&raw mut frame) };
        }
        if !ctx.is_null() {
            let mut ctx = ctx;
            // SAFETY: context is live by the contract above.
            unsafe { sys::avcodec_free_context(&raw mut ctx) };
        }
    }

    /// Input frame queued from `frame`, packets drained into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame is rejected or a ready packet is unusable.
    fn send(&mut self, frame: &CpuFrame, out: &mut Vec<Packet>) -> Result<(), SwError> {
        // SAFETY: frame is live; unref resets it for reuse.
        unsafe { sys::av_frame_unref(self.frame) };
        // SAFETY: geometry matches the open call; get_buffer fills the planes.
        unsafe {
            (*self.frame).width = self.width.cast_signed();
            (*self.frame).height = self.height.cast_signed();
            (*self.frame).format = sys::PIX_FMT_YUV420P;
        }
        // SAFETY: dimensions and format are set; zero aligns by CPU.
        if unsafe { sys::av_frame_get_buffer(self.frame, 0) } != 0 {
            return Err(SwError::Rejected);
        }
        // SAFETY: buffer is live and writable; rows are copied within strides.
        unsafe {
            for (slot, plane) in frame.planes().iter().enumerate() {
                let stride =
                    usize::try_from((*self.frame).linesize[slot]).map_err(|_| SwError::Rejected)?;
                let cols = plane.stride() as usize;
                let rows = plane.data().len() / cols;
                if stride < cols {
                    return Err(SwError::Rejected);
                }
                for row in 0..rows {
                    let dst = (*self.frame).data[slot].add(row * stride);
                    let src = &plane.data()[row * cols..row * cols + cols];
                    std::ptr::copy_nonoverlapping(src.as_ptr(), dst, cols);
                }
            }
            (*self.frame).pts = self.next_pts;
        }
        self.next_pts += 1;
        loop {
            // SAFETY: frame holds one complete picture; EAGAIN keeps it queued.
            let err = unsafe { sys::avcodec_send_frame(self.ctx, self.frame) };
            if err == 0 {
                return Ok(());
            }
            if err != sys::AGAIN {
                return Err(SwError::Rejected);
            }
            let drained = out.len();
            self.receive(out)?;
            if out.len() == drained {
                return Err(SwError::Rejected);
            }
        }
    }

    /// Packets ready now, copied out of the reusable slot.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ready packet breaks mid-encode.
    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), SwError> {
        loop {
            // SAFETY: packet is a valid output slot; success lends its bytes.
            let err = unsafe { sys::avcodec_receive_packet(self.ctx, self.packet) };
            if err == sys::AGAIN || err == sys::EOF {
                return Ok(());
            }
            if err != 0 {
                return Err(SwError::Rejected);
            }
            // SAFETY: success initialized the packet; unref below balances it.
            let len =
                usize::try_from(unsafe { (*self.packet).size }).map_err(|_| SwError::Rejected)?;
            // SAFETY: payload spans live packet memory, copied now.
            let bytes = unsafe { std::slice::from_raw_parts((*self.packet).data, len) };
            let mut packet = Packet::new(Bytes::copy_from_slice(bytes));
            if unsafe { (*self.packet).flags } & sys::FLAG_KEY != 0 {
                packet = packet.mark_keyframe();
            }
            out.push(packet);
            // SAFETY: packet is live and uniquely borrowed here.
            unsafe { sys::av_packet_unref(self.packet) };
        }
    }
}

impl Encoder for Av1Encoder {
    type Error = SwError;

    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error> {
        if frame.format() != PixelFormat::Yuv420P
            || frame.size().width() != self.width
            || frame.size().height() != self.height
        {
            return Err(SwError::Rejected);
        }
        let mut out = Vec::new();
        self.send(frame, &mut out)?;
        self.receive(&mut out)?;
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error> {
        let mut out = Vec::new();
        // SAFETY: null frame signals end of stream.
        let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
        if err == sys::AGAIN {
            self.receive(&mut out)?;
            // SAFETY: queue drained above, so the signal fits now.
            let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
            if err != 0 {
                return Err(SwError::Rejected);
            }
        } else if err != 0 {
            return Err(SwError::Rejected);
        }
        self.receive(&mut out)?;
        Ok(out)
    }
}

impl Drop for Av1Encoder {
    fn drop(&mut self) {
        // SAFETY: every slot came from its allocator and is dropped once.
        unsafe { Self::release(self.ctx, self.frame, self.packet) };
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video::traits::Encoder;

    use rust_video_caps::common::PictureSize;

    use super::Av1Encoder;

    fn frame_64() -> CpuFrame {
        let y = Bytes::from(vec![0x80; 64 * 64]);
        let c = Bytes::from(vec![0x80; 32 * 32]);
        let planes = [
            Plane::new(y, 64).expect("plane"),
            Plane::new(c.clone(), 32).expect("plane"),
            Plane::new(c, 32).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame")
    }

    #[test]
    fn encode_marks_first_packet_keyframe() {
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
        let mut packets = encoder.encode(&frame_64()).expect("encode");
        packets.extend(encoder.encode(&frame_64()).expect("encode"));
        packets.extend(encoder.flush().expect("flush"));
        assert!(!packets.is_empty());
        assert!(packets[0].is_keyframe());
    }

    #[test]
    fn roundtrip_through_ffmpeg_decoder() {
        use rust_video::traits::Decoder as _;

        use crate::ffmpeg::Av1Decoder;

        let mut encoder = Av1Encoder::open(64, 64, 500, 2).expect("encoder");
        let mut decoder = Av1Decoder::open(2).expect("decoder");
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
        let mut encoder = Av1Encoder::open(64, 64, 500, 1).expect("encoder");
        let tiny = Bytes::from(vec![0x80; 16 * 16]);
        let chroma = Bytes::from(vec![0x80; 8 * 8]);
        let planes = [
            Plane::new(tiny, 16).expect("plane"),
            Plane::new(chroma.clone(), 8).expect("plane"),
            Plane::new(chroma, 8).expect("plane"),
        ];
        let size = PictureSize::new(16, 16).expect("size");
        let frame = CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame");
        assert!(encoder.encode(&frame).is_err());
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
