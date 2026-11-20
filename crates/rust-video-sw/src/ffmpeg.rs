// FFI boundary: every unsafe block states its contract.
#![allow(unsafe_code)]
//! Software decode over ffmpeg's native decoders.
//!
//! One safe owner around the raw context; each FFI call justifies itself.

use std::ffi::c_int;
use std::ptr;
use std::sync::Arc;

use crate::error::SwError;
use crate::ffmpeg_sys as sys;
use crate::overlap::RawSource;

use bytes::{Bytes, BytesMut};
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

pub use crate::ffmpeg_sys::AVCodecContext;
pub use rust_video_core::Codec;

/// Sink for pictures drained from the decoder.
trait PictureSink {
    /// Pictures staged so far.
    fn staged(&self) -> usize;

    /// One decoded picture staged.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the picture is unusable.
    fn push_frame(&mut self, frame: &sys::AVFrame) -> Result<(), SwError>;
}

impl PictureSink for Vec<CpuFrame> {
    fn staged(&self) -> usize {
        self.len()
    }

    fn push_frame(&mut self, frame: &sys::AVFrame) -> Result<(), SwError> {
        self.push(convert(frame)?);
        Ok(())
    }
}

impl PictureSink for Vec<RawPicture> {
    fn staged(&self) -> usize {
        self.len()
    }

    fn push_frame(&mut self, frame: &sys::AVFrame) -> Result<(), SwError> {
        self.push(RawPicture::copy_from(frame)?);
        Ok(())
    }
}

/// libavcodec id behind `codec`; `None` for undecodable families.
///
/// # Arguments
///
/// * `codec` - codec family; MPEG-2, VC-1, and AV2 have no ffmpeg decoder here.
pub(crate) fn codec_id(codec: Codec) -> Option<c_int> {
    match codec {
        Codec::H264 => Some(sys::CODEC_ID_H264),
        Codec::H265 => Some(sys::CODEC_ID_HEVC),
        Codec::Vp8 => Some(sys::CODEC_ID_VP8),
        Codec::Vp9 => Some(sys::CODEC_ID_VP9),
        Codec::Av1 => Some(sys::CODEC_ID_AV1),
        Codec::Mpeg2 | Codec::Vc1 | Codec::Av2 => None,
    }
}

/// Decoder name for `find_decoder_by_name`, when id lookup is wrong.
///
/// Callers check [`codec_id`] first; undecodable families never arrive here.
///
/// # Arguments
///
/// * `codec` - codec family.
pub(crate) fn decoder_name(codec: Codec) -> Option<&'static std::ffi::CStr> {
    match codec {
        // By name, not by id: id lookup returns whichever AV1 decoder
        // registered first, and the true native `av1` is hardware-only.
        Codec::Av1 => Some(c"libdav1d"),
        _ => None,
    }
}

/// True when packets already hold whole frames, so no parser runs.
///
/// `None` for undecodable families.
///
/// # Arguments
///
/// * `codec` - codec family; MPEG-2, VC-1, and AV2 have no ffmpeg decoder here.
pub(crate) fn framed(codec: Codec) -> Option<bool> {
    match codec {
        Codec::H264 | Codec::H265 => Some(false),
        Codec::Vp8 | Codec::Vp9 | Codec::Av1 => Some(true),
        Codec::Mpeg2 | Codec::Vc1 | Codec::Av2 => None,
    }
}

/// Threaded decoder over one libavcodec decoder.
///
/// `Send`, never `Sync`: hand the decoder to another thread when idle,
/// but run every call on one thread. Each `decode` drains every
/// picture the library has ready; any hold-back on reorder streams
/// happens inside libavcodec.
pub struct FfmpegDecoder {
    /// Live libavcodec decode context.
    ctx: *mut sys::AVCodecContext,
    /// Annex B splitter; null for framed codecs.
    parser: *mut sys::AVCodecParserContext,
    /// Reusable decoded-frame slot.
    frame: *mut sys::AVFrame,
    /// Reusable coded-packet slot.
    packet: *mut sys::AVPacket,
    /// True when packets hold whole frames.
    framed: bool,
    /// Parser-starved bytes awaiting the next packet.
    pending: Vec<u8>,
}

// SAFETY: sole owner of its context, parser, frame, and packet; every
// FFI call takes `&mut self` and `Drop` frees each allocation once.
unsafe impl Send for FfmpegDecoder {}

impl FfmpegDecoder {
    /// Explicit worker count for `threads`; zero resolves to the machine width.
    fn resolve_threads(threads: u32) -> u32 {
        if threads != 0 {
            return threads;
        }
        std::thread::available_parallelism()
            .ok()
            .and_then(|width| u32::try_from(width.get()).ok())
            .unwrap_or(0)
    }

    /// Decoder for `codec` with `threads` worker threads; zero uses every CPU.
    ///
    /// MPEG-2, VC-1, and AV2 decline: no ffmpeg decoder ships for them here.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the codec family is undecodable, the system library
    /// drifts from the bound ABI, or it rejects the configuration.
    pub fn open(codec: Codec, threads: u32) -> Result<Self, SwError> {
        let id = codec_id(codec).ok_or(SwError::Init)?;
        let framed = framed(codec).ok_or(SwError::Init)?;
        // SAFETY: version queries take no arguments and touch no state.
        if unsafe { sys::avcodec_version() } >> 16 != sys::ABI_MAJOR
            || unsafe { sys::avutil_version() } >> 16 != sys::UTIL_MAJOR
        {
            return Err(SwError::Init);
        }
        let codec_ptr = match decoder_name(codec) {
            // SAFETY: name is a valid C string; null is checked.
            Some(name) => unsafe { sys::avcodec_find_decoder_by_name(name.as_ptr()) },
            // SAFETY: id names a registered decoder; null is checked.
            None => unsafe { sys::avcodec_find_decoder(id) },
        };
        if codec_ptr.is_null() {
            return Err(SwError::Init);
        }
        // SAFETY: codec is live; null is checked.
        let mut ctx = unsafe { sys::avcodec_alloc_context3(codec_ptr) };
        if ctx.is_null() {
            return Err(SwError::Init);
        }
        let threads = Self::resolve_threads(threads);
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
            let code = if err != 0 { err } else { typed };
            return Err(SwError::Native {
                code: i64::from(code),
            });
        }
        // SAFETY: context and codec are live; null options select defaults.
        let err = unsafe { sys::avcodec_open2(ctx, codec_ptr, ptr::null_mut()) };
        if err != 0 {
            // SAFETY: context came from a successful alloc and is freed once.
            unsafe { sys::avcodec_free_context(&raw mut ctx) };
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
        // Framed codecs take whole frames per packet; byte streams need the splitter.
        // SAFETY: id names a registered parser; null is checked.
        let parser = if framed {
            ptr::null_mut()
        } else {
            unsafe { sys::av_parser_init(id) }
        };
        // SAFETY: blank frame or null; null is checked.
        let frame = unsafe { sys::av_frame_alloc() };
        // SAFETY: blank packet or null; null is checked.
        let packet = unsafe { sys::av_packet_alloc() };
        if (!framed && parser.is_null()) || frame.is_null() || packet.is_null() {
            // SAFETY: non-null slots are live; null slots are skipped.
            unsafe { Self::release(ctx, parser, frame, packet) };
            return Err(SwError::Init);
        }
        Ok(Self {
            ctx,
            parser,
            frame,
            packet,
            framed,
            pending: Vec::new(),
        })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// Threading: a single thread needs nothing; threads sharing one
    /// decoder serialize context calls with crate submissions, sequencing
    /// calls made through the pointer with `decode` and `flush`, never
    /// overlapping them.
    ///
    /// Lifetime: the pointer borrows the decoder and stays valid only
    /// while the decoder lives; using it after the decoder drops, or
    /// freeing or reconfiguring the context through it, is undefined
    /// behavior.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.ctx.cast_const()
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
    /// # Performance
    ///
    /// One copy per packet.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the packet is rejected or a drained picture is unusable.
    fn send(&mut self, bytes: &[u8], out: &mut impl PictureSink) -> Result<(), SwError> {
        let size = std::ffi::c_int::try_from(bytes.len()).map_err(|_| SwError::Rejected)?;
        // SAFETY: packet is live; unref resets it for reuse.
        unsafe { sys::av_packet_unref(self.packet) };
        // SAFETY: packet is live and blank; size is the input length.
        let err = unsafe { sys::av_new_packet(self.packet, size) };
        if err != 0 {
            return Err(SwError::Native {
                code: i64::from(err),
            });
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
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
            let drained = out.staged();
            self.receive(out)?;
            if out.staged() == drained {
                return Err(SwError::Rejected);
            }
        }
    }

    /// Pictures ready now, converted and pushed into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ready picture breaks mid-decode.
    fn receive(&mut self, out: &mut impl PictureSink) -> Result<(), SwError> {
        loop {
            // SAFETY: frame is a valid output slot; success lends its planes.
            let err = unsafe { sys::avcodec_receive_frame(self.ctx, self.frame) };
            if err == sys::AGAIN || err == sys::EOF {
                return Ok(());
            }
            if err != 0 {
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
            // SAFETY: success initialized the frame; unref below balances it.
            let pushed = out.push_frame(unsafe { &*self.frame });
            // SAFETY: frame is live and uniquely borrowed here.
            unsafe { sys::av_frame_unref(self.frame) };
            pushed?;
        }
    }

    /// Parser output from `bytes`, each completed frame sent and drained.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the parser or the decoder rejects its input.
    fn parse(&mut self, bytes: &[u8], out: &mut impl PictureSink) -> Result<(), SwError> {
        let joined;
        let mut rest = if self.pending.is_empty() {
            bytes
        } else {
            joined = [self.pending.as_slice(), bytes].concat();
            self.pending.clear();
            joined.as_slice()
        };
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
                return Err(SwError::Native {
                    code: i64::from(consumed),
                });
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
                    self.pending.extend_from_slice(rest);
                    return Ok(());
                }
                continue;
            }
            rest = rest.get(consumed..).ok_or(SwError::Rejected)?;
        }
        Ok(())
    }

    /// Input frames from `packet` queued.
    ///
    /// Pictures freed only to make room surface in `freed`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the packet is rejected or a freed picture is unusable.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    /// * `freed` - pictures freed to make room.
    ///
    /// # Returns
    ///
    /// False when `packet` held no framed input and nothing queued.
    fn submit_into(
        &mut self,
        packet: &Packet,
        freed: &mut impl PictureSink,
    ) -> Result<bool, SwError> {
        if self.framed {
            if packet.data().is_empty() {
                return Ok(false);
            }
            self.send(packet.data(), freed)?;
        } else {
            self.parse(packet.data(), freed)?;
        }
        Ok(true)
    }

    /// Parser tail plus held pictures staged; resets the stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tail breaks mid-decode.
    ///
    /// # Arguments
    ///
    /// * `out` - pictures staged, oldest first.
    fn flush_into(&mut self, out: &mut impl PictureSink) -> Result<(), SwError> {
        if !self.framed {
            let booked = std::mem::take(&mut self.pending);
            if !booked.is_empty() {
                self.parse(&booked, out)?;
                self.pending.clear();
            }
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
                    return Err(SwError::Native {
                        code: i64::from(err),
                    });
                }
                if pout_size == 0 {
                    break;
                }
                let len = usize::try_from(pout_size).map_err(|_| SwError::Rejected)?;
                // SAFETY: parser lends the tail frame, copied into the send below.
                let frame = unsafe { std::slice::from_raw_parts(pout, len) };
                self.send(frame, out)?;
            }
        }
        // SAFETY: null packet signals end of stream.
        let err = unsafe { sys::avcodec_send_packet(self.ctx, ptr::null()) };
        if err == sys::AGAIN {
            self.receive(out)?;
            // SAFETY: queue drained above, so the signal fits now.
            let err = unsafe { sys::avcodec_send_packet(self.ctx, ptr::null()) };
            if err != 0 {
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
        } else if err != 0 {
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
        self.receive(out)?;
        // SAFETY: context is live; reset readies it for the next stream.
        unsafe { sys::avcodec_flush_buffers(self.ctx) };
        Ok(())
    }
}

impl Decoder for FfmpegDecoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        if self.submit_into(packet, &mut out)? {
            self.receive(&mut out)?;
        }
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        self.flush_into(&mut out)?;
        Ok(out)
    }
}

impl RawSource for FfmpegDecoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.submit_into(packet, freed)?;
        Ok(())
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.receive(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        let mut out = Vec::new();
        self.flush_into(&mut out)?;
        Ok(out)
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        raw.gather()
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        // SAFETY: every slot came from its allocator and is dropped once.
        unsafe { Self::release(self.ctx, self.parser, self.frame, self.packet) };
    }
}

/// Threaded H.264 decoder.
pub struct H264Decoder {
    inner: FfmpegDecoder,
}

impl H264Decoder {
    /// Decoder with `threads` worker threads; zero uses every CPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        FfmpegDecoder::open(Codec::H264, threads).map(|inner| Self { inner })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// See [`FfmpegDecoder::context`] for the threading and lifetime rules.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.inner.context()
    }
}

impl Decoder for H264Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.decode(packet)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.flush()
    }
}

impl RawSource for H264Decoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.inner.submit_raw(packet, freed)
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.inner.poll_raw(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        self.inner.flush_raw()
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        FfmpegDecoder::convert(raw)
    }
}

/// Threaded HEVC decoder.
pub struct H265Decoder {
    inner: FfmpegDecoder,
}

impl H265Decoder {
    /// Decoder with `threads` worker threads; zero uses every CPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        FfmpegDecoder::open(Codec::H265, threads).map(|inner| Self { inner })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// See [`FfmpegDecoder::context`] for the threading and lifetime rules.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.inner.context()
    }
}

impl Decoder for H265Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.decode(packet)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.flush()
    }
}

impl RawSource for H265Decoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.inner.submit_raw(packet, freed)
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.inner.poll_raw(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        self.inner.flush_raw()
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        FfmpegDecoder::convert(raw)
    }
}

/// Threaded VP8 decoder.
pub struct Vp8Decoder {
    inner: FfmpegDecoder,
}

impl Vp8Decoder {
    /// Decoder with `threads` decoder threads; zero uses every CPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        FfmpegDecoder::open(Codec::Vp8, threads).map(|inner| Self { inner })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// See [`FfmpegDecoder::context`] for the threading and lifetime rules.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.inner.context()
    }
}

impl Decoder for Vp8Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.decode(packet)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.flush()
    }
}

impl RawSource for Vp8Decoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.inner.submit_raw(packet, freed)
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.inner.poll_raw(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        self.inner.flush_raw()
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        FfmpegDecoder::convert(raw)
    }
}

/// Threaded VP9 decoder.
pub struct Vp9Decoder {
    inner: FfmpegDecoder,
}

impl Vp9Decoder {
    /// Decoder with `threads` decoder threads; zero uses every CPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        FfmpegDecoder::open(Codec::Vp9, threads).map(|inner| Self { inner })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// See [`FfmpegDecoder::context`] for the threading and lifetime rules.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.inner.context()
    }
}

impl Decoder for Vp9Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.decode(packet)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.flush()
    }
}

impl RawSource for Vp9Decoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.inner.submit_raw(packet, freed)
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.inner.poll_raw(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        self.inner.flush_raw()
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        FfmpegDecoder::convert(raw)
    }
}

/// Threaded AV1 decoder.
pub struct Av1Decoder {
    inner: FfmpegDecoder,
}

impl Av1Decoder {
    /// Decoder with `threads` worker threads; zero uses every CPU.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        FfmpegDecoder::open(Codec::Av1, threads).map(|inner| Self { inner })
    }

    /// Native libavcodec context behind the decoder.
    ///
    /// See [`FfmpegDecoder::context`] for the threading and lifetime rules.
    #[must_use]
    pub fn context(&self) -> *const AVCodecContext {
        self.inner.context()
    }
}

impl Decoder for Av1Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(None, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.decode(packet)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.inner.flush()
    }
}

impl RawSource for Av1Decoder {
    type Raw = RawPicture;
    type Frame = CpuFrame;
    type Error = SwError;

    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error> {
        self.inner.submit_raw(packet, freed)
    }

    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error> {
        self.inner.poll_raw(out)
    }

    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error> {
        self.inner.flush_raw()
    }

    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
        FfmpegDecoder::convert(raw)
    }
}

/// Validated geometry plus row strides behind one decoded picture.
struct PictureLayout {
    /// Visible extent.
    size: PictureSize,
    /// Luma bytes per row.
    y_stride: usize,
    /// Chroma U bytes per row.
    u_stride: usize,
    /// Chroma V bytes per row.
    v_stride: usize,
}

impl PictureLayout {
    /// Layout read from a decoded frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame is not 8-bit 4:2:0 or its geometry is unusable.
    fn read(frame: &sys::AVFrame) -> Result<Self, SwError> {
        if frame.format != sys::PIX_FMT_YUV420P && frame.format != sys::PIX_FMT_YUVJ420P {
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
        let chroma_width = width.div_ceil(2);
        if y_stride < width as usize
            || u_stride < chroma_width as usize
            || v_stride < chroma_width as usize
        {
            return Err(SwError::BadFrame);
        }
        Ok(Self {
            size,
            y_stride,
            u_stride,
            v_stride,
        })
    }

    /// True when every plane stride equals its visible row width.
    fn is_tight(&self) -> bool {
        let width = self.size.width() as usize;
        let chroma = self.size.width().div_ceil(2) as usize;
        self.y_stride == width && self.u_stride == chroma && self.v_stride == chroma
    }
}

/// Refcounted libavutil frame handle backing zero-copy plane views.
struct OwnedAvFrame {
    /// Cloned frame; freed once on drop.
    raw: *mut sys::AVFrame,
}

// SAFETY: `raw` is an immutable cloned `AVFrame` whose `AVBuffer` refcounts
// are atomic inside libavutil; no mutation occurs after `clone_from`.
unsafe impl Send for OwnedAvFrame {}
// SAFETY: Plane readers only form shared `&[u8]` slices over immutable bytes.
unsafe impl Sync for OwnedAvFrame {}

impl OwnedAvFrame {
    /// Refcounted clone of `frame` sharing its underlying `AVBuffer` planes.
    fn clone_from(frame: &sys::AVFrame) -> Result<Self, SwError> {
        // SAFETY: `frame` is a live decoded frame; null return is checked.
        let raw = unsafe { sys::av_frame_clone(ptr::from_ref(frame)) };
        if raw.is_null() {
            return Err(SwError::BadFrame);
        }
        Ok(Self { raw })
    }

    /// Borrowed view of the cloned frame.
    fn frame(&self) -> &sys::AVFrame {
        // SAFETY: `self.raw` is non-null and immutable for `self`'s lifetime.
        unsafe { &*self.raw }
    }
}

impl Drop for OwnedAvFrame {
    fn drop(&mut self) {
        let mut raw = self.raw;
        // SAFETY: `self.raw` came from `av_frame_clone` and is freed once here.
        unsafe { sys::av_frame_free(&raw mut raw) };
    }
}

/// Immutable slice of one plane inside a refcounted `OwnedAvFrame`.
struct AvPlaneBytes {
    /// Shared frame owner keeping the plane buffer alive.
    _owner: Arc<OwnedAvFrame>,
    /// Plane start pointer.
    ptr: *const u8,
    /// Byte length of the tight plane.
    len: usize,
}

// SAFETY: `_owner` keeps the immutable `AVBuffer` alive; `ptr` is read-only.
unsafe impl Send for AvPlaneBytes {}
// SAFETY: Shared access only yields `&[u8]` over immutable pixel bytes.
unsafe impl Sync for AvPlaneBytes {}

impl AsRef<[u8]> for AvPlaneBytes {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `ptr..ptr + len` is a validated tight plane kept live by `_owner`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

/// Refcounted decoded picture awaiting conversion or zero-copy handoff.
pub struct RawPicture {
    /// Cloned libavutil frame.
    frame: OwnedAvFrame,
}

impl RawPicture {
    /// Refcounted clone of a decoded frame without copying plane bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame is not 8-bit 4:2:0 or cloning fails.
    fn copy_from(frame: &sys::AVFrame) -> Result<Self, SwError> {
        let _ = PictureLayout::read(frame)?;
        Ok(Self {
            frame: OwnedAvFrame::clone_from(frame)?,
        })
    }

    /// CPU frame built from the retained picture, zero-copy when tight.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the planes cannot form a frame.
    pub(crate) fn gather(self) -> Result<CpuFrame, SwError> {
        let layout = PictureLayout::read(self.frame.frame())?;
        if layout.is_tight() {
            return zero_copy_frame(Arc::new(self.frame), &layout);
        }
        gather_strided(self.frame.frame(), &layout)
    }
}

/// Zero-copy `CpuFrame` over a tight refcounted `OwnedAvFrame`.
fn zero_copy_frame(owner: Arc<OwnedAvFrame>, layout: &PictureLayout) -> Result<CpuFrame, SwError> {
    let data = owner.frame().data;
    let width = layout.size.width();
    let height = layout.size.height() as usize;
    let chroma_width = width.div_ceil(2);
    let chroma_height = layout.size.height().div_ceil(2) as usize;
    let y_bytes = Bytes::from_owner(AvPlaneBytes {
        _owner: Arc::clone(&owner),
        ptr: data[0].cast_const(),
        len: width as usize * height,
    });
    let u_bytes = Bytes::from_owner(AvPlaneBytes {
        _owner: Arc::clone(&owner),
        ptr: data[1].cast_const(),
        len: chroma_width as usize * chroma_height,
    });
    let v_bytes = Bytes::from_owner(AvPlaneBytes {
        _owner: owner,
        ptr: data[2].cast_const(),
        len: chroma_width as usize * chroma_height,
    });
    let [Some(y), Some(u), Some(v)] = [
        Plane::new(y_bytes, width),
        Plane::new(u_bytes, chroma_width),
        Plane::new(v_bytes, chroma_width),
    ] else {
        return Err(SwError::BadFrame);
    };
    CpuFrame::new(layout.size, PixelFormat::Yuv420P, vec![y, u, v]).ok_or(SwError::BadFrame)
}

/// Single-allocation `CpuFrame` gathered row-by-row from a strided `AVFrame`.
fn gather_strided(frame: &sys::AVFrame, layout: &PictureLayout) -> Result<CpuFrame, SwError> {
    let width = layout.size.width() as usize;
    let height = layout.size.height() as usize;
    let chroma_width = layout.size.width().div_ceil(2) as usize;
    let chroma_height = layout.size.height().div_ceil(2) as usize;
    let y_len = width * height;
    let uv_len = chroma_width * chroma_height;
    let mut backing = BytesMut::with_capacity(y_len + uv_len * 2);
    // SAFETY: each row copies only `cols` visible bytes within `frame`'s plane buffer.
    unsafe {
        copy_rows(
            &mut backing,
            frame.data[0].cast_const(),
            layout.y_stride,
            height,
            width,
        );
        copy_rows(
            &mut backing,
            frame.data[1].cast_const(),
            layout.u_stride,
            chroma_height,
            chroma_width,
        );
        copy_rows(
            &mut backing,
            frame.data[2].cast_const(),
            layout.v_stride,
            chroma_height,
            chroma_width,
        );
    }
    let y = backing.split_to(y_len).freeze();
    let u = backing.split_to(uv_len).freeze();
    let v = backing.freeze();
    let stride_y = layout.size.width();
    let stride_uv = stride_y.div_ceil(2);
    let [Some(y), Some(u), Some(v)] = [
        Plane::new(y, stride_y),
        Plane::new(u, stride_uv),
        Plane::new(v, stride_uv),
    ] else {
        return Err(SwError::BadFrame);
    };
    CpuFrame::new(layout.size, PixelFormat::Yuv420P, vec![y, u, v]).ok_or(SwError::BadFrame)
}

/// Append `rows` of `cols` visible bytes from `ptr` stepped by `stride`.
///
/// # Safety
///
/// `ptr` must address `rows` valid rows of `cols` bytes separated by `stride`.
unsafe fn copy_rows(out: &mut BytesMut, ptr: *const u8, stride: usize, rows: usize, cols: usize) {
    if rows == 0 || cols == 0 {
        return;
    }
    if stride == cols {
        // SAFETY: contiguous span of `rows * cols` bytes per caller contract.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr, rows * cols) });
    } else {
        for row in 0..rows {
            // SAFETY: row `row` has `cols` valid bytes at `row * stride`.
            let line = unsafe { std::slice::from_raw_parts(ptr.add(row * stride), cols) };
            out.extend_from_slice(line);
        }
    }
}

/// CPU frame from a decoded `AVFrame`, zero-copy when plane strides are tight.
///
/// # Errors
///
/// Returns `Err` when the frame is not 8-bit 4:2:0 or its geometry is unusable.
fn convert(frame: &sys::AVFrame) -> Result<CpuFrame, SwError> {
    let layout = PictureLayout::read(frame)?;
    if layout.is_tight() {
        let owned = OwnedAvFrame::clone_from(frame)?;
        return zero_copy_frame(Arc::new(owned), &layout);
    }
    gather_strided(frame, &layout)
}

#[cfg(test)]
mod tests {
    use bytes::{Bytes, BytesMut};

    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;

    use super::{
        copy_rows, Av1Decoder, Codec, FfmpegDecoder, H264Decoder, H265Decoder, Vp8Decoder,
        Vp9Decoder,
    };
    use crate::error::SwError;

    fn assert_send<T: Send>() {}

    #[test]
    fn undecodable_families_decline() {
        for codec in [Codec::Mpeg2, Codec::Vc1, Codec::Av2] {
            assert!(
                matches!(FfmpegDecoder::open(codec, 1), Err(SwError::Init)),
                "{codec:?} declines"
            );
        }
    }

    #[test]
    fn decoders_move_between_threads() {
        assert_send::<Av1Decoder>();
        assert_send::<FfmpegDecoder>();
        assert_send::<H264Decoder>();
        assert_send::<H265Decoder>();
        assert_send::<Vp8Decoder>();
        assert_send::<Vp9Decoder>();
    }

    #[test]
    fn caps_agree_with_cpu_frames() {
        use rust_video_caps::transcode::GpuCaps;
        use rust_video_harness::H264Synth;

        let mut decoder = H264Decoder::open(4).expect("decoder");
        assert_eq!(decoder.caps(), GpuCaps::new(None, false, false));
        let mut frames = Vec::new();
        for raw in H264Synth::annex_b_packets() {
            let packet = Packet::new(Bytes::copy_from_slice(&raw));
            frames.extend(decoder.decode(&packet).expect("decode"));
        }
        frames.extend(decoder.flush().expect("flush"));
        assert_eq!(frames.len(), 2);
        assert_eq!(decoder.caps(), GpuCaps::new(None, false, false));
        for frame in &frames {
            assert_ne!(frame.planes().len(), 0);
        }
    }

    #[test]
    fn copy_rows_matches_row_by_row() {
        let tight: Vec<u8> = (0..64).collect();
        let mut strided = [0xFFu8; 2 * 40];
        strided[..32].copy_from_slice(&tight[..32]);
        strided[40..72].copy_from_slice(&tight[32..]);
        let mut out_tight = BytesMut::with_capacity(64);
        // SAFETY: `tight` holds 2 rows of 32 bytes.
        unsafe { copy_rows(&mut out_tight, tight.as_ptr(), 32, 2, 32) };
        assert_eq!(&out_tight[..], &tight[..]);
        let mut out_strided = BytesMut::with_capacity(64);
        // SAFETY: `strided` holds 2 rows of 32 visible bytes at stride 40.
        unsafe { copy_rows(&mut out_strided, strided.as_ptr(), 40, 2, 32) };
        assert_eq!(&out_strided[..], &tight[..]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn independent_decoders_run_concurrently() {
        const STREAMS: usize = 4;
        let mut decoders = Vec::with_capacity(STREAMS);
        for _ in 0..STREAMS {
            decoders.push(FfmpegDecoder::open(Codec::Vp9, 1).expect("decoder"));
        }
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(STREAMS);
            for mut decoder in decoders {
                handles.push(scope.spawn(move || {
                    let packet = Packet::new(Bytes::new());
                    let frames = decoder.decode(&packet).expect("decode");
                    let tail = decoder.flush().expect("flush");
                    assert!(frames.is_empty() && tail.is_empty());
                }));
            }
            for handle in handles {
                handle.join().expect("thread");
            }
        });
    }

    #[test]
    fn open_accepts_thread_counts() {
        assert!(H264Decoder::open(0).is_ok());
        assert!(H264Decoder::open(4).is_ok());
    }

    #[test]
    fn auto_resolves_to_machine_width() {
        assert_eq!(FfmpegDecoder::resolve_threads(4), 4);
        let expected = std::thread::available_parallelism()
            .ok()
            .and_then(|width| u32::try_from(width.get()).ok())
            .unwrap_or(0);
        assert_eq!(FfmpegDecoder::resolve_threads(0), expected);
        if expected != 0 {
            assert!(H265Decoder::open(0).is_ok());
        }
    }

    #[test]
    fn open_accepts_every_codec() {
        for codec in [Codec::H264, Codec::H265, Codec::Vp8, Codec::Vp9, Codec::Av1] {
            assert!(FfmpegDecoder::open(codec, 1).is_ok(), "{codec:?}");
        }
    }

    #[test]
    fn context_names_the_live_native_context() {
        let decoder = H264Decoder::open(1).expect("decoder");
        assert!(!decoder.context().is_null());
        assert_eq!(decoder.context(), decoder.context());
    }

    #[test]
    fn flush_without_input_is_empty() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        assert_eq!(decoder.flush().expect("flush"), []);
    }

    #[test]
    fn garbage_input_fails_without_panic() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::from(vec![0xff; 64]));
        assert_eq!(decoder.decode(&packet).unwrap_or_default(), []);
        let _ = decoder.flush();
    }

    #[test]
    fn empty_input_fails_without_panic() {
        let mut decoder = H264Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::new());
        assert_eq!(decoder.decode(&packet).expect("decode"), []);
    }

    #[test]
    fn framed_garbage_fails_without_panic() {
        let mut decoder = FfmpegDecoder::open(Codec::Vp9, 1).expect("decoder");
        let packet = Packet::new(Bytes::from(vec![0xff; 64]));
        assert_eq!(decoder.decode(&packet).unwrap_or_default(), []);
        let _ = decoder.flush();
    }

    #[test]
    fn framed_empty_input_is_empty() {
        let mut decoder = FfmpegDecoder::open(Codec::Vp9, 1).expect("decoder");
        let packet = Packet::new(Bytes::new());
        assert_eq!(decoder.decode(&packet).expect("decode"), []);
    }

    #[cfg(feature = "h264-openh264")]
    #[test]
    fn own_encoder_output_decodes_back() {
        use rust_video_caps::encode::EncodeConfig;
        use rust_video_caps::frame::PixelFormat;
        use rust_video_caps::traits::Encoder as _;

        use crate::fixtures::frame_64;
        use crate::h264::H264Encoder;

        let frame = frame_64();
        let mut encoder = H264Encoder::open(64, 64, EncodeConfig::default(), 1).expect("encoder");
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

    #[test]
    fn looped_packets_drain_fully() {
        use rust_video_harness::H264Synth;

        let raws = H264Synth::annex_b_packets();
        let mut decoder = H264Decoder::open(4).expect("decoder");
        let mut frames = 0;
        for _ in 0..300 {
            for raw in &raws {
                let packet = Packet::new(Bytes::copy_from_slice(raw));
                frames += decoder.decode(&packet).expect("decode").len();
            }
        }
        frames += decoder.flush().expect("flush").len();
        assert_eq!(frames, 600);
    }

    #[test]
    fn split_packets_drain_fully() {
        use rust_video_harness::H264Synth;

        let mut halves = Vec::new();
        for raw in H264Synth::annex_b_packets() {
            let mid = raw.len() / 2;
            halves.push(raw[..mid].to_vec());
            halves.push(raw[mid..].to_vec());
        }
        let mut decoder = H264Decoder::open(4).expect("decoder");
        let mut frames = 0;
        for _ in 0..300 {
            for raw in &halves {
                let packet = Packet::new(Bytes::copy_from_slice(raw));
                frames += decoder.decode(&packet).expect("decode").len();
            }
        }
        frames += decoder.flush().expect("flush").len();
        assert_eq!(frames, 600);
    }
}
