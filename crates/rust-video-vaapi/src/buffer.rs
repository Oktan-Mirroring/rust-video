//! Owned VA-API data buffers.

#![allow(unsafe_code)]

use std::mem::MaybeUninit;
use std::os::raw::{c_int, c_void};

use crate::error::VaApiError;
use crate::ffi;
use crate::sys;

/// Owned VA-API data buffer, destroyed on drop.
pub(crate) struct VaBuffer {
    /// Display owning the buffer.
    display: ffi::VaDisplay,
    /// Context owning the buffer.
    context: ffi::VaContextId,
    /// Buffer handle.
    id: ffi::VaBufferId,
    /// Buffer kind, for recreation on rewrite.
    buffer_type: c_int,
    /// Creation size in bytes.
    len: usize,
}

impl VaBuffer {
    /// Buffer of `buffer_type` holding a copy of `data`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the buffer.
    pub(crate) fn create(
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
        buffer_type: c_int,
        data: &[u8],
    ) -> Result<Self, VaApiError> {
        let mut id = 0;
        let len = u32::try_from(data.len())
            .map_err(|_| VaApiError::Unsupported("buffer outside driver range".to_owned()))?;
        // Sound: the context is live; `data` holds `len` bytes.
        sys::status(unsafe {
            sys::vaCreateBuffer(
                display,
                context,
                buffer_type,
                len,
                1,
                data.as_ptr().cast_mut().cast(),
                &raw mut id,
            )
        })?;
        Ok(Self {
            display,
            context,
            id,
            buffer_type,
            len: data.len(),
        })
    }

    /// Buffer of `buffer_type` holding a copy of raw parameter bytes `data`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the buffer.
    pub(crate) fn create_uninit(
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
        buffer_type: c_int,
        data: &[MaybeUninit<u8>],
    ) -> Result<Self, VaApiError> {
        let mut id = 0;
        let len = u32::try_from(data.len())
            .map_err(|_| VaApiError::Unsupported("buffer outside driver range".to_owned()))?;
        // SAFETY: the context is live; `data` spans `len` bytes of driver-layout storage.
        sys::status(unsafe {
            sys::vaCreateBuffer(
                display,
                context,
                buffer_type,
                len,
                1,
                data.as_ptr().cast_mut().cast(),
                &raw mut id,
            )
        })?;
        Ok(Self {
            display,
            context,
            id,
            buffer_type,
            len: data.len(),
        })
    }

    /// Empty buffer of `buffer_type` with `len` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the buffer.
    pub(crate) fn empty(
        display: ffi::VaDisplay,
        context: ffi::VaContextId,
        buffer_type: c_int,
        len: usize,
    ) -> Result<Self, VaApiError> {
        let mut id = 0;
        let size = u32::try_from(len)
            .map_err(|_| VaApiError::Unsupported("buffer outside driver range".to_owned()))?;
        // Sound: the context is live; a null payload means uninitialized.
        sys::status(unsafe {
            sys::vaCreateBuffer(
                display,
                context,
                buffer_type,
                size,
                1,
                std::ptr::null_mut(),
                &raw mut id,
            )
        })?;
        Ok(Self {
            display,
            context,
            id,
            buffer_type,
            len,
        })
    }

    /// Buffer handle.
    #[must_use]
    pub(crate) fn id(&self) -> ffi::VaBufferId {
        self.id
    }

    /// Rewrite the buffer with `data`, recreating it only when `data` overflows.
    ///
    /// Shorter payloads map over the leading bytes; the driver reads
    /// the length the parameter buffer names, never the tail. The
    /// return reports a recreation.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the map or the recreation.
    pub(crate) fn rewrite(&mut self, data: &[u8]) -> Result<bool, VaApiError> {
        if data.len() > self.len {
            *self = Self::create(self.display, self.context, self.buffer_type, data)?;
            return Ok(true);
        }
        // Sound: the buffer is live until the context drops.
        let mut mapped: *mut c_void = std::ptr::null_mut();
        sys::status(unsafe { sys::vaMapBuffer(self.display, self.id, &raw mut mapped) })?;
        if !data.is_empty() {
            // Sound: the mapping spans the creation size, which fits `data`.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast(), data.len());
            }
        }
        // Sound: the buffer was mapped above.
        sys::status(unsafe { sys::vaUnmapBuffer(self.display, self.id) })?;
        Ok(false)
    }

    /// Rewrite the buffer from raw parameter bytes `data`, recreating on overflow.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the map or the recreation.
    pub(crate) fn rewrite_uninit(&mut self, data: &[MaybeUninit<u8>]) -> Result<bool, VaApiError> {
        if data.len() > self.len {
            *self = Self::create_uninit(self.display, self.context, self.buffer_type, data)?;
            return Ok(true);
        }
        // SAFETY: the buffer is live until the context drops.
        let mut mapped: *mut c_void = std::ptr::null_mut();
        sys::status(unsafe { sys::vaMapBuffer(self.display, self.id, &raw mut mapped) })?;
        if !data.is_empty() {
            // SAFETY: the mapping spans at least `data.len()` bytes; `MaybeUninit<u8>` preserves padding soundness.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    mapped.cast::<MaybeUninit<u8>>(),
                    data.len(),
                );
            }
        }
        // SAFETY: the buffer was mapped above.
        sys::status(unsafe { sys::vaUnmapBuffer(self.display, self.id) })?;
        Ok(false)
    }
}

impl Drop for VaBuffer {
    fn drop(&mut self) {
        // Sound: the id came from a successful `vaCreateBuffer`.
        unsafe {
            sys::vaDestroyBuffer(self.display, self.id);
        }
    }
}

/// Raw byte view of a `#[repr(C)]` parameter struct, preserving padding soundness.
pub(crate) fn bytes_of<T>(value: &T) -> &[MaybeUninit<u8>] {
    // SAFETY: `MaybeUninit<u8>` has alignment 1 and permits any initialized or padding byte in `T`.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(value).cast(), size_of_val(value)) }
}
