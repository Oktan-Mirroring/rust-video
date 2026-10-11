#![allow(unsafe_code)]
//! H.264 decode over ffmpeg's native decoder.
//!
//! One safe owner around the raw context; each FFI call justifies itself.

use std::ptr;

use bytes::Bytes;

use rust_video::frame::{CpuFrame, PixelFormat, Plane};
use rust_video::packet::Packet;
use rust_video::traits::Decoder;

use rust_video_caps::common::PictureSize;

use crate::error::SwError;
use crate::ffmpeg_sys as sys;

/// Threaded H.264 decoder.
pub struct H264Decoder {
    ctx: *mut sys::AVCodecContext,
    parser: *mut sys::AVCodecParserContext,
    frame: *mut sys::AVFrame,
    packet: *mut sys::AVPacket,
}

impl H264Decoder {
    /// Decoder with `threads` worker threads; zero selects the library default.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        // SAFETY: version query takes no arguments and touches no state.
        if unsafe { sys::avcodec_version() } >> 16 != sys::ABI_MAJOR {
            return Err(SwError::Init);
        }
        // SAFETY: id names a registered decoder; null is checked.
        let codec = unsafe { sys::avcodec_find_decoder(sys::CODEC_ID_H264) };
        if codec.is_null() {
            return Err(SwError::Init);
        }
        // SAFETY: codec is live; null is checked.
        let mut ctx = unsafe { sys::avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            return Err(SwError::Init);
        }
        // SAFETY: context is live; option names are valid C strings.
        let err = unsafe {
            sys::av_opt_set_int(
                ctx.cast::<std::ffi::c_void>(),
                c"threads".as_ptr(),
                i64::from(threads),
                0,
            )
        };
        // SAFETY: context is live; option names are valid C strings.
        let typed = unsafe {
            sys::av_opt_set(
                ctx.cast::<std::ffi::c_void>(),
                c"thread_type".as_ptr(),
                c"frame+slice".as_ptr(),
                0,
            )
        };
        if err != 0 || typed != 0 {
            // SAFETY: context came from a successful alloc and is freed once.
            unsafe { sys::avcodec_free_context(&raw mut ctx) };
            return Err(SwError::Init);
        }
        // SAFETY: context and codec are live; null options select defaults.
        let err = unsafe { sys::avcodec_open2(ctx, codec, ptr::null_mut()) };
        if err != 0 {
            // SAFETY: context came from a successful alloc and is freed once.
            unsafe { sys::avcodec_free_context(&raw mut ctx) };
            return Err(SwError::Init);
        }
        // SAFETY: id names a registered parser; null is checked.
        let parser = unsafe { sys::av_parser_init(sys::CODEC_ID_H264) };
        // SAFETY: blank frame or null; null is checked.
        let frame = unsafe { sys::av_frame_alloc() };
        // SAFETY: blank packet or null; null is checked.
        let packet = unsafe { sys::av_packet_alloc() };
        if parser.is_null() || frame.is_null() || packet.is_null() {
            // SAFETY: non-null slots are live; null slots are skipped.
            unsafe { Self::release(ctx, parser, frame, packet) };
            return Err(SwError::Init);
        }
        Ok(Self {
            ctx,
            parser,
            frame,
            packet,
        })
    }

    /// Partially built state released; null slots are skipped.
    ///
    /// # Safety
    ///
    /// Each non-null pointer must come from its allocator and be freed once.
    unsafe fn release(
        ctx: *mut sys::AVCodecContext,
        parser: *mut sys::AVCodecParserContext,
        frame: *mut sys::AVFrame,
        packet: *mut sys::AVPacket,
    ) {
        if !parser.is_null() {
            // SAFETY: parser is live by the contract above.
            unsafe { sys::av_parser_close(parser) };
        }
        if !frame.is_null() {
            let mut frame = frame;
            // SAFETY: frame is live by the contract above.
            unsafe { sys::av_frame_free(&raw mut frame) };
        }
        if !packet.is_null() {
            let mut packet = packet;
            // SAFETY: packet is live by the contract above.
            unsafe { sys::av_packet_free(&raw mut packet) };
        }
        if !ctx.is_null() {
            let mut ctx = ctx;
            // SAFETY: context is live by the contract above.
            unsafe { sys::avcodec_free_context(&raw mut ctx) };
        }
    }

    /// One complete frame queued from `bytes`, drained pictures into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the packet is rejected or a drained picture is unusable.
    fn send(&mut self, bytes: &[u8], out: &mut Vec<CpuFrame>) -> Result<(), SwError> {
        let size = std::ffi::c_int::try_from(bytes.len()).map_err(|_| SwError::Rejected)?;
        // SAFETY: packet is live; unref resets it for reuse.
        unsafe { sys::av_packet_unref(self.packet) };
        // SAFETY: packet is live and blank; size is the input length.
        let err = unsafe { sys::av_new_packet(self.packet, size) };
        if err != 0 {
            return Err(SwError::Rejected);
        }
        // SAFETY: payload spans the fresh packet buffer, written now.
        let dst = unsafe { std::slice::from_raw_parts_mut((*self.packet).data, bytes.len()) };
        dst.copy_from_slice(bytes);
        loop {
            // SAFETY: packet holds one complete frame; EAGAIN keeps it queued.
            let err = unsafe { sys::avcodec_send_packet(self.ctx, self.packet) };
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

    /// Pictures ready now, converted and pushed into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ready picture breaks mid-decode.
    fn receive(&mut self, out: &mut Vec<CpuFrame>) -> Result<(), SwError> {
        loop {
            // SAFETY: frame is a valid output slot; success lends its planes.
            let err = unsafe { sys::avcodec_receive_frame(self.ctx, self.frame) };
            if err == sys::AGAIN || err == sys::EOF {
                return Ok(());
            }
            if err != 0 {
                return Err(SwError::Rejected);
            }
            // SAFETY: success initialized the frame; unref below balances it.
            let frame = convert(unsafe { &*self.frame });
            // SAFETY: frame is live and uniquely borrowed here.
            unsafe { sys::av_frame_unref(self.frame) };
            out.push(frame?);
        }
    }

    /// Parser output from `bytes`, each completed frame sent and drained.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the parser or the decoder rejects its input.
    fn parse(&mut self, bytes: &[u8], out: &mut Vec<CpuFrame>) -> Result<(), SwError> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let size = std::ffi::c_int::try_from(rest.len()).map_err(|_| SwError::Rejected)?;
            let mut pout = ptr::null_mut();
            let mut pout_size = 0;
            // SAFETY: input spans live packet bytes; output pair receives the frame.
            let consumed = unsafe {
                sys::av_parser_parse2(
                    self.parser,
                    self.ctx,
                    &raw mut pout,
                    &raw mut pout_size,
                    rest.as_ptr(),
                    size,
                    sys::NOPTS,
                    sys::NOPTS,
                    -1,
                )
            };
            if consumed < 0 {
                return Err(SwError::Rejected);
            }
            if pout_size > 0 {
                let len = usize::try_from(pout_size).map_err(|_| SwError::Rejected)?;
                // SAFETY: parser lends a complete frame, copied into the send below.
                let frame = unsafe { std::slice::from_raw_parts(pout, len) };
                self.send(frame, out)?;
            }
            let consumed = usize::try_from(consumed).map_err(|_| SwError::Rejected)?;
            if consumed == 0 {
                if pout_size == 0 {
                    return Ok(());
                }
                continue;
            }
            rest = rest.get(consumed..).ok_or(SwError::Rejected)?;
        }
        Ok(())
    }
}

impl Decoder for H264Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        self.parse(packet.data(), &mut out)?;
        self.receive(&mut out)?;
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        loop {
            let mut pout = ptr::null_mut();
            let mut pout_size = 0;
            // SAFETY: null input flushes the parser; output pair receives the tail.
            let err = unsafe {
                sys::av_parser_parse2(
                    self.parser,
                    self.ctx,
                    &raw mut pout,
                    &raw mut pout_size,
                    ptr::null(),
                    0,
                    sys::NOPTS,
                    sys::NOPTS,
                    -1,
                )
            };
            if err < 0 {
                return Err(SwError::Rejected);
            }
            if pout_size == 0 {
                break;
            }
            let len = usize::try_from(pout_size).map_err(|_| SwError::Rejected)?;
            // SAFETY: parser lends the tail frame, copied into the send below.
            let frame = unsafe { std::slice::from_raw_parts(pout, len) };
            self.send(frame, &mut out)?;
        }
        // SAFETY: null packet signals end of stream.
        let err = unsafe { sys::avcodec_send_packet(self.ctx, ptr::null()) };
        if err == sys::AGAIN {
            self.receive(&mut out)?;
            // SAFETY: queue drained above, so the signal fits now.
            let err = unsafe { sys::avcodec_send_packet(self.ctx, ptr::null()) };
            if err != 0 {
                return Err(SwError::Rejected);
            }
        } else if err != 0 {
            return Err(SwError::Rejected);
        }
        self.receive(&mut out)?;
        // SAFETY: context is live; reset readies it for the next stream.
        unsafe { sys::avcodec_flush_buffers(self.ctx) };
        Ok(out)
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        // SAFETY: every slot came from its allocator and is dropped once.
        unsafe { Self::release(self.ctx, self.parser, self.frame, self.packet) };
    }
}

/// CPU frame copied from a decoded frame.
///
/// # Errors
///
/// Returns `Err` when the frame is not 8-bit 4:2:0 or its geometry is unusable.
fn convert(frame: &sys::AVFrame) -> Result<CpuFrame, SwError> {
    if frame.format != sys::PIX_FMT_YUV420P {
        return Err(SwError::BadFrame);
    }
    let width = u32::try_from(frame.width).map_err(|_| SwError::BadFrame)?;
    let height = u32::try_from(frame.height).map_err(|_| SwError::BadFrame)?;
    let Some(size) = PictureSize::new(width, height) else {
        return Err(SwError::BadFrame);
    };
    if frame.data[0].is_null() || frame.data[1].is_null() || frame.data[2].is_null() {
        return Err(SwError::BadFrame);
    }
    let y_stride = usize::try_from(frame.linesize[0]).map_err(|_| SwError::BadFrame)?;
    let u_stride = usize::try_from(frame.linesize[1]).map_err(|_| SwError::BadFrame)?;
    let v_stride = usize::try_from(frame.linesize[2]).map_err(|_| SwError::BadFrame)?;
    if y_stride < width as usize || u_stride < width as usize / 2 || v_stride < width as usize / 2 {
        return Err(SwError::BadFrame);
    }
    let rows = |plane: usize, stride: usize, count: usize, cols: usize| {
        let mut out = Vec::with_capacity(count * cols);
        for row in 0..count {
            // SAFETY: rows are stride bytes apart inside the live frame.
            let src = unsafe { frame.data[plane].add(row * stride) };
            // SAFETY: each row spans live decoder memory, copied now.
            let bytes = unsafe { std::slice::from_raw_parts(src.cast_const(), cols) };
            out.extend_from_slice(bytes);
        }
        out
    };
    let y = rows(0, y_stride, height as usize, width as usize);
    let u = rows(1, u_stride, height as usize / 2, width as usize / 2);
    let v = rows(2, v_stride, height as usize / 2, width as usize / 2);
    let planes = [
        Plane::new(Bytes::from(y), width),
        Plane::new(Bytes::from(u), width / 2),
        Plane::new(Bytes::from(v), width / 2),
    ];
    let [Some(y), Some(u), Some(v)] = planes else {
        return Err(SwError::BadFrame);
    };
    CpuFrame::new(size, PixelFormat::Yuv420P, vec![y, u, v]).ok_or(SwError::BadFrame)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video::packet::Packet;
    use rust_video::traits::Decoder;

    use super::H264Decoder;

    #[test]
    fn open_accepts_thread_counts() {
        assert!(H264Decoder::open(0).is_ok());
        assert!(H264Decoder::open(4).is_ok());
    }

    #[test]
    fn flush_without_input_is_empty() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        assert!(decoder.flush().expect("flush").is_empty());
    }

    #[test]
    fn garbage_input_fails_without_panic() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::from(vec![0xff; 64]));
        let _ = decoder.decode(&packet);
        let _ = decoder.flush();
    }

    #[test]
    fn empty_input_fails_without_panic() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::new());
        assert!(decoder.decode(&packet).expect("decode").is_empty());
    }

    #[cfg(feature = "h264-openh264")]
    #[test]
    fn own_encoder_output_decodes_back() {
        use rust_video::frame::{CpuFrame, PixelFormat, Plane};
        use rust_video::traits::Encoder as _;

        use rust_video_caps::common::PictureSize;

        use crate::h264::H264Encoder;

        let y = Bytes::from(vec![0x80; 64 * 64]);
        let c = Bytes::from(vec![0x80; 32 * 32]);
        let planes = [
            Plane::new(y, 64).expect("plane"),
            Plane::new(c.clone(), 32).expect("plane"),
            Plane::new(c, 32).expect("plane"),
        ];
        let size = PictureSize::new(64, 64).expect("size");
        let frame = CpuFrame::new(size, PixelFormat::Yuv420P, planes.into()).expect("frame");
        let mut encoder = H264Encoder::open(64, 64, 500_000, 1).expect("encoder");
        let first = encoder.encode(&frame).expect("encode");
        let second = encoder.encode(&frame).expect("encode");
        let mut decoder = H264Decoder::open(1).expect("decoder");
        let mut frames = Vec::new();
        for packet in first.iter().chain(&second) {
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
}
