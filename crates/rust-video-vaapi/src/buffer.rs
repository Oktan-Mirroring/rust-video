//! Owned VA-API data buffers.

#![allow(unsafe_code)]

use std::os::raw::{c_int, c_void};

use crate::error::VaApiError;
use crate::ffi;

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
        VaApiError::status(unsafe {
            ffi::vaCreateBuffer(
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
        VaApiError::status(unsafe {
            ffi::vaCreateBuffer(
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
    /// the length the parameter buffer names, never the tail.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the map or the recreation.
    pub(crate) fn rewrite(&mut self, data: &[u8]) -> Result<(), VaApiError> {
        if data.len() > self.len {
            *self = Self::create(self.display, self.context, self.buffer_type, data)?;
            return Ok(());
        }
        // Sound: the buffer is live until the context drops.
        let mut mapped: *mut c_void = std::ptr::null_mut();
        VaApiError::status(unsafe { ffi::vaMapBuffer(self.display, self.id, &raw mut mapped) })?;
        if !data.is_empty() {
            // Sound: the mapping spans the creation size, which fits `data`.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), mapped.cast(), data.len());
            }
        }
        // Sound: the buffer was mapped above.
        VaApiError::status(unsafe { ffi::vaUnmapBuffer(self.display, self.id) })
    }
}

impl Drop for VaBuffer {
    fn drop(&mut self) {
        // Sound: the id came from a successful `vaCreateBuffer`.
        unsafe {
            ffi::vaDestroyBuffer(self.display, self.id);
        }
    }
}

/// Byte view of a plain parameter struct.
pub(crate) fn bytes_of<T>(value: &T) -> &[u8] {
    // Sound: every parameter struct is `#[repr(C)]` plain data.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(value).cast(), size_of_val(value)) }
}
