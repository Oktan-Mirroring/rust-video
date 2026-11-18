// FFI boundary: every unsafe block states its contract.
#![allow(unsafe_code)]
//! Shared ffmpeg encode session behind the software encoders.
//!
//! One live context plus a reusable frame and packet; typed wrappers
//! supply codec candidates and option presets.

use std::ffi::CStr;
use std::ptr;

use crate::error::SwError;
use crate::ffmpeg_sys as sys;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;

/// Live ffmpeg encode session: context plus reusable frame and packet.
///
/// `Send`, never `Sync`: hand the session to another thread when idle,
/// but run every call on one thread.
pub(crate) struct FfmpegEncoder {
    /// Active encoder context.
    ctx: *mut sys::AVCodecContext,
    /// Reusable input frame with pooled plane backing.
    frame: *mut sys::AVFrame,
    /// Reusable output packet slot.
    packet: *mut sys::AVPacket,
    /// Factory recreating the context when `encode` follows `flush`.
    reopen: Box<dyn FnMut() -> Result<*mut sys::AVCodecContext, SwError> + Send>,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Next presentation timestamp in frame units.
    next_pts: i64,
    /// True once `frame` owns an allocated plane buffer.
    allocated: bool,
    /// True after `flush` sent end-of-stream to `ctx`.
    draining: bool,
}

// SAFETY: sole owner of its context, frame, and packet; every FFI call
// takes `&mut self` and `Drop` frees each allocation once.
unsafe impl Send for FfmpegEncoder {}

impl FfmpegEncoder {
    /// CBR encoder for `name` at `width`x`height` and `bitrate_kbps`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub(crate) fn open(
        name: &'static CStr,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        configure: impl Fn(*mut sys::AVCodecContext, i64) -> bool + Send + 'static,
    ) -> Result<Self, SwError> {
        Self::open_candidates(
            vec![name],
            width,
            height,
            bitrate_kbps,
            move |candidate, ctx, bitrate| {
                debug_assert_eq!(candidate, name);
                configure(ctx, bitrate)
            },
        )
    }

    /// CBR encoder selecting the first available candidate in `names`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no candidate opens or the system library ABI drifts.
    pub(crate) fn open_candidates(
        names: Vec<&'static CStr>,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        configure: impl Fn(&'static CStr, *mut sys::AVCodecContext, i64) -> bool + Send + 'static,
    ) -> Result<Self, SwError> {
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
        let build_ctx = move || -> Result<*mut sys::AVCodecContext, SwError> {
            let mut last_err = SwError::Init;
            for &name in &names {
                // SAFETY: name is a valid static C string; null is checked.
                let codec = unsafe { sys::avcodec_find_encoder_by_name(name.as_ptr()) };
                if codec.is_null() {
                    continue;
                }
                // SAFETY: codec is live; null is checked.
                let mut ctx = unsafe { sys::avcodec_alloc_context3(codec) };
                if ctx.is_null() {
                    return Err(SwError::Init);
                }
                if !configure(name, ctx, bitrate) {
                    // SAFETY: context came from a successful alloc and is freed once.
                    unsafe { sys::avcodec_free_context(&raw mut ctx) };
                    last_err = SwError::Init;
                    continue;
                }
                // SAFETY: context and codec are live; null options select defaults.
                let err = unsafe { sys::avcodec_open2(ctx, codec, ptr::null_mut()) };
                if err == 0 {
                    return Ok(ctx);
                }
                // SAFETY: context came from a successful alloc and is freed once.
                unsafe { sys::avcodec_free_context(&raw mut ctx) };
                last_err = SwError::Native {
                    code: i64::from(err),
                };
            }
            Err(last_err)
        };
        let ctx = build_ctx()?;
        // SAFETY: blank frame or null; null is checked.
        let frame = unsafe { sys::av_frame_alloc() };
        // SAFETY: blank packet or null; null is checked.
        let packet = if frame.is_null() {
            ptr::null_mut()
        } else {
            unsafe { sys::av_packet_alloc() }
        };
        if frame.is_null() || packet.is_null() {
            // SAFETY: non-null slots are live; null slots are skipped.
            unsafe { Self::release(ctx, frame, packet) };
            return Err(SwError::Init);
        }
        Ok(Self {
            ctx,
            frame,
            packet,
            reopen: Box::new(build_ctx),
            width,
            height,
            next_pts: 0,
            allocated: false,
            draining: false,
        })
    }

    /// Coded packets for one validated YUV420P `frame`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame is rejected or a ready packet is unusable.
    pub(crate) fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, SwError> {
        SwError::check_yuv420p(frame, self.width, self.height)?;
        if self.draining {
            let fresh = (self.reopen)()?;
            let mut old = self.ctx;
            // SAFETY: `old` is live and uniquely owned here.
            unsafe { sys::avcodec_free_context(&raw mut old) };
            self.ctx = fresh;
            self.next_pts = 0;
            self.draining = false;
        }
        let mut out = Vec::new();
        self.send(frame, &mut out)?;
        self.receive(&mut out)?;
        Ok(out)
    }

    /// Trailing packets after end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the flush signal or a trailing packet fails.
    pub(crate) fn flush(&mut self) -> Result<Vec<Packet>, SwError> {
        if self.draining {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        // SAFETY: null frame signals end of stream.
        let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
        if err == sys::AGAIN {
            self.receive(&mut out)?;
            // SAFETY: queue drained above, so the signal fits now.
            let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
            if err != 0 && err != sys::EOF {
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
        } else if err != 0 && err != sys::EOF {
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
        self.draining = true;
        self.receive(&mut out)?;
        Ok(out)
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

    /// Ensure `self.frame` holds writable plane buffers.
    fn ensure_writable_frame(&mut self) -> Result<(), SwError> {
        if !self.allocated {
            // SAFETY: geometry matches the open call; get_buffer fills the planes.
            unsafe {
                (*self.frame).width = self.width.cast_signed();
                (*self.frame).height = self.height.cast_signed();
                (*self.frame).format = sys::PIX_FMT_YUV420P;
            }
            // SAFETY: dimensions and format are set; zero aligns by CPU.
            let err = unsafe { sys::av_frame_get_buffer(self.frame, 0) };
            if err != 0 {
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
            self.allocated = true;
        }
        // SAFETY: `self.frame` holds allocated refcounted buffers; COWs only if retained.
        let err = unsafe { sys::av_frame_make_writable(self.frame) };
        if err != 0 {
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
        Ok(())
    }

    /// Input frame queued from `frame`, packets drained into `out`.
    fn send(&mut self, frame: &CpuFrame, out: &mut Vec<Packet>) -> Result<(), SwError> {
        self.ensure_writable_frame()?;
        // SAFETY: buffer is live and writable; rows are copied within strides.
        unsafe {
            for (slot, plane) in frame.planes().iter().enumerate() {
                let stride =
                    usize::try_from((*self.frame).linesize[slot]).map_err(|_| SwError::Rejected)?;
                let src_stride = plane.stride() as usize;
                let cols = if slot == 0 {
                    self.width as usize
                } else {
                    self.width.div_ceil(2) as usize
                };
                let rows = if slot == 0 {
                    self.height as usize
                } else {
                    self.height as usize / 2
                };
                if stride < cols || src_stride < cols {
                    return Err(SwError::Rejected);
                }
                let dst = std::slice::from_raw_parts_mut((*self.frame).data[slot], rows * stride);
                copy_plane(dst, stride, plane.data(), src_stride, rows, cols);
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
                return Err(SwError::Native {
                    code: i64::from(err),
                });
            }
            let drained = out.len();
            self.receive(out)?;
            if out.len() == drained {
                return Err(SwError::Rejected);
            }
        }
    }

    /// Packets ready now, copied out of the reusable slot.
    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), SwError> {
        loop {
            // SAFETY: packet is a valid output slot; success lends its bytes.
            let err = unsafe { sys::avcodec_receive_packet(self.ctx, self.packet) };
            if err == sys::AGAIN || err == sys::EOF {
                return Ok(());
            }
            if err != 0 {
                return Err(SwError::Native {
                    code: i64::from(err),
                });
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

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        // SAFETY: every slot came from its allocator and is dropped once.
        unsafe { Self::release(self.ctx, self.frame, self.packet) };
    }
}

/// Destination rows filled from source rows.
fn copy_plane(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    rows: usize,
    cols: usize,
) {
    if dst_stride == src_stride && src_stride == cols {
        let len = rows * cols;
        dst[..len].copy_from_slice(&src[..len]);
    } else {
        for (row, chunk) in src.chunks_exact(src_stride).take(rows).enumerate() {
            dst[row * dst_stride..row * dst_stride + cols].copy_from_slice(&chunk[..cols]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_plane, FfmpegEncoder};

    fn assert_send<T: Send>() {}

    #[test]
    fn encoder_moves_between_threads() {
        assert_send::<FfmpegEncoder>();
    }

    #[test]
    fn copy_plane_matches_row_by_row() {
        let tight: Vec<u8> = (0..64).collect();
        let mut strided = vec![0xFFu8; 2 * 40];
        strided[..32].copy_from_slice(&tight[..32]);
        strided[40..72].copy_from_slice(&tight[32..]);
        let mut fast = vec![0u8; 64];
        copy_plane(&mut fast, 32, &tight, 32, 2, 32);
        assert_eq!(fast, tight);
        let mut slow = vec![0u8; 64];
        copy_plane(&mut slow, 32, &strided, 40, 2, 32);
        assert_eq!(slow, tight);
    }
}
