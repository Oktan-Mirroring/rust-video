// FFI boundary: every unsafe block states its contract.
#![allow(unsafe_code)]
//! Shared ffmpeg encode session behind the software encoders.
//!
//! One live context plus a reusable frame and packet; the typed wrappers
//! differ only by codec name and option preset.

use std::ffi::CStr;
use std::ptr;

use bytes::Bytes;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;

use crate::error::SwError;
use crate::ffmpeg_sys as sys;

/// Live ffmpeg encode session: context plus reusable frame and packet.
///
/// `Send`, never `Sync`: hand the session to another thread when idle,
/// but run every call on one thread.
pub(crate) struct FfmpegEncoder {
    ctx: *mut sys::AVCodecContext,
    frame: *mut sys::AVFrame,
    packet: *mut sys::AVPacket,
    width: u32,
    height: u32,
    next_pts: i64,
}

// SAFETY: sole owner of its context, frame, and packet; every FFI call
// takes `&mut self` and `Drop` frees each allocation once.
unsafe impl Send for FfmpegEncoder {}

impl FfmpegEncoder {
    /// CBR encoder for `name` at `width`x`height` and `bitrate_kbps`.
    ///
    /// `configure` applies codec options to the blank context; false rejects it.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the system library drifts from the bound ABI or
    /// rejects the configuration.
    pub(crate) fn open(
        name: &CStr,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        configure: impl FnOnce(*mut sys::AVCodecContext, i64) -> bool,
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
        // SAFETY: name is a valid C string; null is checked.
        let codec = unsafe { sys::avcodec_find_encoder_by_name(name.as_ptr()) };
        if codec.is_null() {
            return Err(SwError::Init);
        }
        // SAFETY: codec is live; null is checked.
        let ctx = unsafe { sys::avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            return Err(SwError::Init);
        }
        if !configure(ctx, bitrate) {
            // SAFETY: context came from a successful alloc and is freed once.
            unsafe { Self::release(ctx, ptr::null_mut(), ptr::null_mut()) };
            return Err(SwError::Init);
        }
        // SAFETY: context and codec are live; null options select defaults.
        let err = unsafe { sys::avcodec_open2(ctx, codec, ptr::null_mut()) };
        if err != 0 {
            // SAFETY: context came from a successful alloc and is freed once.
            unsafe { Self::release(ctx, ptr::null_mut(), ptr::null_mut()) };
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
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
            width,
            height,
            next_pts: 0,
        })
    }

    /// Coded packets for one validated YUV420P `frame`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the frame is rejected or a ready packet is unusable.
    pub(crate) fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, SwError> {
        SwError::check_yuv420p(frame, self.width, self.height)?;
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
        let mut out = Vec::new();
        // SAFETY: null frame signals end of stream.
        let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
        if err == sys::AGAIN {
            self.receive(&mut out)?;
            // SAFETY: queue drained above, so the signal fits now.
            let err = unsafe { sys::avcodec_send_frame(self.ctx, ptr::null()) };
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

    /// Input frame queued from `frame`, packets drained into `out`.
    ///
    /// # Performance
    ///
    /// One copy per plane.
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
        let err = unsafe { sys::av_frame_get_buffer(self.frame, 0) };
        if err != 0 {
            return Err(SwError::Native {
                code: i64::from(err),
            });
        }
        // SAFETY: buffer is live and writable; rows are copied within strides.
        unsafe {
            for (slot, plane) in frame.planes().iter().enumerate() {
                let stride =
                    usize::try_from((*self.frame).linesize[slot]).map_err(|_| SwError::Rejected)?;
                let src_stride = plane.stride() as usize;
                let rows = if slot == 0 {
                    self.height as usize
                } else {
                    self.height as usize / 2
                };
                if stride < src_stride {
                    return Err(SwError::Rejected);
                }
                let dst = std::slice::from_raw_parts_mut((*self.frame).data[slot], rows * stride);
                copy_plane(dst, stride, plane.data(), src_stride, rows, src_stride);
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
    ///
    /// # Performance
    ///
    /// One copy per packet.
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
///
/// # Performance
///
/// One copy per plane.
///
/// # Arguments
///
/// * `dst` - destination bytes; holds `rows` times `dst_stride`.
/// * `dst_stride` - destination bytes per row.
/// * `src` - source bytes; holds `rows` times `src_stride`.
/// * `src_stride` - source bytes per row.
/// * `rows` - row count.
/// * `cols` - bytes copied per row; at most both strides.
fn copy_plane(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    rows: usize,
    cols: usize,
) {
    if dst_stride == src_stride {
        let len = rows * src_stride;
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
