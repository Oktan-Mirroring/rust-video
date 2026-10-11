#![allow(unsafe_code)]
//! AV1 decode over dav1d.
//!
//! One safe owner around the raw context; each FFI call justifies itself.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;

use bytes::Bytes;
use dav1d_sys::{
    Dav1dContext, Dav1dData, Dav1dPicture, Dav1dSettings, DAV1D_ERR_AGAIN, DAV1D_PIXEL_LAYOUT_I420,
};

use rust_video::frame::{CpuFrame, PixelFormat, Plane};
use rust_video::packet::Packet;
use rust_video::traits::Decoder;

use rust_video_caps::common::PictureSize;

use crate::error::SwError;

/// Threaded AV1 decoder.
pub struct Av1Decoder {
    ctx: *mut Dav1dContext,
}

impl Av1Decoder {
    /// Decoder with `threads` worker threads; zero selects dav1d's default.
    ///
    /// # Errors
    ///
    /// Returns `Err` when dav1d rejects the settings.
    pub fn open(threads: u32) -> Result<Self, SwError> {
        let mut settings = MaybeUninit::<Dav1dSettings>::zeroed();
        // SAFETY: defaults fill the whole struct.
        unsafe { dav1d_sys::dav1d_default_settings(settings.as_mut_ptr()) };
        // SAFETY: settings hold defaults from the call above.
        let mut settings = unsafe { settings.assume_init() };
        settings.n_threads = threads.cast_signed();
        settings.max_frame_delay = 0;
        let mut ctx: *mut Dav1dContext = ptr::null_mut();
        // SAFETY: settings are valid; output pointer receives the context.
        let err = unsafe { dav1d_sys::dav1d_open(&raw mut ctx, &raw const settings) };
        if err != 0 || ctx.is_null() {
            return Err(SwError::Init);
        }
        Ok(Self { ctx })
    }

    /// Pictures ready now, converted and pushed into `out`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ready picture breaks mid-decode.
    fn drain(&mut self, out: &mut Vec<CpuFrame>) -> Result<(), SwError> {
        loop {
            let mut picture = MaybeUninit::<Dav1dPicture>::zeroed();
            // SAFETY: picture is a valid output slot; success lends its planes.
            let err = unsafe { dav1d_sys::dav1d_get_picture(self.ctx, picture.as_mut_ptr()) };
            if err == DAV1D_ERR_AGAIN {
                return Ok(());
            }
            if err != 0 {
                return Err(SwError::Rejected);
            }
            // SAFETY: success initialized the picture; unref below balances it.
            let picture = unsafe { picture.assume_init() };
            let frame = convert(&picture)?;
            // SAFETY: picture is live and uniquely borrowed here.
            unsafe { dav1d_sys::dav1d_picture_unref(ptr::from_ref(&picture).cast_mut()) };
            out.push(frame);
        }
    }
}

impl Decoder for Av1Decoder {
    type Frame = CpuFrame;
    type Error = SwError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        let boxed = Box::new(packet.data().to_vec());
        let (ptr, len) = (boxed.as_ptr(), boxed.len());
        let cookie = Box::into_raw(boxed);
        let mut data = MaybeUninit::<Dav1dData>::zeroed();
        // SAFETY: buffer and cookie stay alive until wrap decides ownership.
        let err = unsafe {
            dav1d_sys::dav1d_data_wrap(
                data.as_mut_ptr(),
                ptr,
                len,
                Some(free_boxed),
                cookie.cast::<c_void>(),
            )
        };
        if err != 0 {
            // SAFETY: failed wrap keeps the box with the caller.
            drop(unsafe { Box::from_raw(cookie) });
            return Err(SwError::Rejected);
        }
        // SAFETY: wrap succeeded, so dav1d owns the box from here on.
        let mut data = unsafe { data.assume_init() };
        let mut out = Vec::new();
        loop {
            // SAFETY: data is a live wrapped input; EAGAIN keeps ownership here.
            let err = unsafe { dav1d_sys::dav1d_send_data(self.ctx, &raw mut data) };
            if err == 0 {
                break;
            }
            if err != DAV1D_ERR_AGAIN {
                // SAFETY: failed sends keep ownership with the caller.
                unsafe { dav1d_sys::dav1d_data_unref(&raw mut data) };
                return Err(SwError::Rejected);
            }
            let drained = out.len();
            self.drain(&mut out)?;
            if out.len() == drained {
                // SAFETY: nothing drained, so the input is still ours.
                unsafe { dav1d_sys::dav1d_data_unref(&raw mut data) };
                return Err(SwError::Rejected);
            }
        }
        self.drain(&mut out)?;
        Ok(out)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        self.drain(&mut out)?;
        Ok(out)
    }
}

impl Drop for Av1Decoder {
    fn drop(&mut self) {
        // SAFETY: context came from a successful open and is dropped once.
        unsafe { dav1d_sys::dav1d_close(&raw mut self.ctx) };
    }
}

/// Owned input bytes released when dav1d drops its last reference.
///
/// # Safety
///
/// `cookie` must be the box address handed to `dav1d_data_wrap`, called once.
unsafe extern "C" fn free_boxed(_buf: *const u8, cookie: *mut c_void) {
    // SAFETY: cookie is the live box by the wrap contract; called once.
    drop(unsafe { Box::from_raw(cookie.cast::<Vec<u8>>()) });
}

/// CPU frame copied from a decoded picture.
///
/// # Errors
///
/// Returns `Err` when the picture is not 8-bit 4:2:0 or its geometry is unusable.
fn convert(picture: &Dav1dPicture) -> Result<CpuFrame, SwError> {
    if picture.p.layout != DAV1D_PIXEL_LAYOUT_I420 || picture.p.bpc != 8 {
        return Err(SwError::BadFrame);
    }
    let width = u32::try_from(picture.p.w).map_err(|_| SwError::BadFrame)?;
    let height = u32::try_from(picture.p.h).map_err(|_| SwError::BadFrame)?;
    let Some(size) = PictureSize::new(width, height) else {
        return Err(SwError::BadFrame);
    };
    let y_stride = usize::try_from(picture.stride[0]).map_err(|_| SwError::BadFrame)?;
    let c_stride = usize::try_from(picture.stride[1]).map_err(|_| SwError::BadFrame)?;
    if y_stride < width as usize || c_stride < width as usize / 2 {
        return Err(SwError::BadFrame);
    }
    let rows = |plane: usize, stride: usize, count: usize, cols: usize| {
        let mut out = Vec::with_capacity(count * cols);
        for row in 0..count {
            // SAFETY: rows are stride bytes apart inside the live picture.
            let src = unsafe { (picture.data[plane] as *const u8).add(row * stride) };
            // SAFETY: each row spans live decoder memory, copied now.
            let bytes = unsafe { std::slice::from_raw_parts(src, cols) };
            out.extend_from_slice(bytes);
        }
        out
    };
    let y = rows(0, y_stride, height as usize, width as usize);
    let u = rows(1, c_stride, height as usize / 2, width as usize / 2);
    let v = rows(2, c_stride, height as usize / 2, width as usize / 2);
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

    use super::Av1Decoder;

    #[test]
    fn open_accepts_thread_counts() {
        assert!(Av1Decoder::open(0).is_ok());
        assert!(Av1Decoder::open(4).is_ok());
    }

    #[test]
    fn flush_without_input_is_empty() {
        let mut decoder = Av1Decoder::open(1).expect("decoder");
        assert!(decoder.flush().expect("flush").is_empty());
    }

    #[test]
    fn garbage_input_fails_without_panic() {
        let mut decoder = Av1Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::from(vec![0xff; 64]));
        let _ = decoder.decode(&packet);
    }

    #[test]
    fn empty_input_fails_without_panic() {
        let mut decoder = Av1Decoder::open(1).expect("decoder");
        let packet = Packet::new(Bytes::new());
        let _ = decoder.decode(&packet);
    }
}
