//! VA-API decode context: surfaces, submit, and readback.
//!
//! The live implementation targets Linux; other platforms decline at
//! `open` with a typed skip.

#![allow(unsafe_code)]

use std::os::raw::c_int;
#[cfg(target_os = "linux")]
use std::os::raw::c_void;

use crate::device::VaApiDevice;
#[cfg(not(target_os = "linux"))]
use crate::error::SkipReason;
use crate::error::VaApiError;
use crate::ffi::{self, VaSurfaceId};
#[cfg(target_os = "linux")]
use crate::ffi::{VaBufferId, VaConfigId, VaContextId};

/// VA-API decode context over a surface pool.
pub struct Backend {
    /// Initialized display.
    #[cfg(target_os = "linux")]
    display: ffi::VaDisplay,
    /// Decode configuration.
    #[cfg(target_os = "linux")]
    config: VaConfigId,
    /// Decode context.
    #[cfg(target_os = "linux")]
    context: VaContextId,
    /// Render-target pool.
    #[cfg(target_os = "linux")]
    surfaces: Vec<VaSurfaceId>,
    /// Pool size as the driver takes it.
    #[cfg(target_os = "linux")]
    count_c: c_int,
    /// Coded width in luma samples.
    #[cfg(target_os = "linux")]
    width: u32,
    /// Coded height in luma samples.
    #[cfg(target_os = "linux")]
    height: u32,
    /// Pool size.
    count: usize,
}

impl Backend {
    /// Context for `profile` at `width` x `height` with `count` surfaces.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Linux, or when the driver refuses the config,
    /// the surfaces, or the context.
    pub fn open(
        device: &VaApiDevice,
        profile: c_int,
        width: u32,
        height: u32,
        count: usize,
    ) -> Result<Self, VaApiError> {
        #[cfg(target_os = "linux")]
        {
            let display = device.display();
            let width_c = c_int::try_from(width).map_err(|_| Self::range())?;
            let height_c = c_int::try_from(height).map_err(|_| Self::range())?;
            let count_c = c_int::try_from(count).map_err(|_| Self::range())?;
            let count_u = u32::try_from(count).map_err(|_| Self::range())?;
            let mut attrib = ffi::VaConfigAttrib {
                attr_type: ffi::VA_CONFIG_ATTRIB_RT_FORMAT,
                value: ffi::VA_RT_FORMAT_YUV420,
            };
            let mut config = 0;
            // Sound: the display is initialized; the attribute and the
            // out-parameter are live for the call.
            Self::check(unsafe {
                ffi::vaCreateConfig(
                    display,
                    profile,
                    ffi::VA_ENTRYPOINT_VLD,
                    &raw mut attrib,
                    1,
                    &raw mut config,
                )
            })?;
            let mut surfaces = vec![0; count];
            // Sound: the surface array holds `count` live entries.
            let surfaces_status = unsafe {
                ffi::vaCreateSurfaces(
                    display,
                    ffi::VA_RT_FORMAT_YUV420,
                    width,
                    height,
                    surfaces.as_mut_ptr(),
                    count_u,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if Self::check(surfaces_status).is_err() {
                // Sound: `config` was created above.
                unsafe {
                    ffi::vaDestroyConfig(display, config);
                }
                return Err(VaApiError::Call(surfaces_status));
            }
            let mut context = 0;
            // Sound: the render-target array outlives context creation.
            let context_status = unsafe {
                ffi::vaCreateContext(
                    display,
                    config,
                    width_c,
                    height_c,
                    ffi::VA_PROGRESSIVE,
                    surfaces.as_mut_ptr(),
                    count_c,
                    &raw mut context,
                )
            };
            if Self::check(context_status).is_err() {
                // Sound: the surfaces and config were created above.
                unsafe {
                    ffi::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                    ffi::vaDestroyConfig(display, config);
                }
                return Err(VaApiError::Call(context_status));
            }
            Ok(Self {
                display,
                config,
                context,
                surfaces,
                count_c,
                width,
                height,
                count,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (device, profile, width, height, count);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Pool surfaces.
    #[must_use]
    pub fn surfaces(&self) -> &[VaSurfaceId] {
        #[cfg(target_os = "linux")]
        {
            &self.surfaces
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = self;
            &[]
        }
    }

    /// Pool size.
    #[must_use]
    pub fn pool_size(&self) -> usize {
        self.count
    }

    /// Decode one picture into `target`, blocking until done.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Linux, or when any VA-API call fails.
    pub fn submit(
        &self,
        target: VaSurfaceId,
        picture: &ffi::VaPictureParameterBufferH264,
        matrix: &ffi::VaIqMatrixBufferH264,
        slices: &[(&ffi::VaSliceParameterBufferH264, &[u8])],
    ) -> Result<(), VaApiError> {
        #[cfg(target_os = "linux")]
        {
            let mut buffers = Vec::with_capacity(2 + 2 * slices.len());
            let outcome = self.submit_inner(target, picture, matrix, slices, &mut buffers);
            for buffer in buffers {
                // Sound: every pushed id came from a successful `vaCreateBuffer`.
                unsafe {
                    ffi::vaDestroyBuffer(self.display, buffer);
                }
            }
            outcome
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (self, target, picture, matrix, slices);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// NV12 bytes of `surface` cropped to `crop` `(x, y, width, height)`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Linux, when the driver image is not NV12,
    /// when its layout overruns its bytes, or when any call fails.
    pub fn read_nv12(
        &self,
        surface: VaSurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        #[cfg(target_os = "linux")]
        {
            let (x, y, width, height) = crop;
            if x.saturating_add(width) > self.width || y.saturating_add(height) > self.height {
                return Err(VaApiError::Unsupported(
                    "crop outside coded size".to_owned(),
                ));
            }
            // Sound: the display and surface are live.
            let mut image = std::mem::MaybeUninit::<ffi::VaImage>::uninit();
            Self::check(unsafe { ffi::vaDeriveImage(self.display, surface, image.as_mut_ptr()) })?;
            // Sound: `vaDeriveImage` succeeded, so the image is initialized.
            let image = unsafe { image.assume_init() };
            let outcome = self.copy_nv12(&image, crop);
            // Sound: the image was derived above.
            unsafe {
                ffi::vaDestroyImage(self.display, image.image_id);
            }
            outcome
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (self, surface, crop);
            Err(SkipReason::PlatformUnsupported.into())
        }
    }

    /// Submit body; buffer cleanup belongs to the caller.
    #[cfg(target_os = "linux")]
    fn submit_inner(
        &self,
        target: VaSurfaceId,
        picture: &ffi::VaPictureParameterBufferH264,
        matrix: &ffi::VaIqMatrixBufferH264,
        slices: &[(&ffi::VaSliceParameterBufferH264, &[u8])],
        buffers: &mut Vec<VaBufferId>,
    ) -> Result<(), VaApiError> {
        buffers.push(self.create_buffer(
            ffi::VA_PICTURE_PARAMETER_BUFFER_TYPE,
            size_of_val(picture),
            std::ptr::from_ref(picture).cast(),
        )?);
        buffers.push(self.create_buffer(
            ffi::VA_IQ_MATRIX_BUFFER_TYPE,
            size_of_val(matrix),
            std::ptr::from_ref(matrix).cast(),
        )?);
        for (params, data) in slices {
            buffers.push(self.create_buffer(
                ffi::VA_SLICE_PARAMETER_BUFFER_TYPE,
                size_of_val(*params),
                std::ptr::from_ref(*params).cast(),
            )?);
            buffers.push(self.create_buffer(
                ffi::VA_SLICE_DATA_BUFFER_TYPE,
                data.len(),
                data.as_ptr().cast(),
            )?);
        }
        // Sound: the context and target are live; the buffers were created above.
        Self::check(unsafe { ffi::vaBeginPicture(self.display, self.context, target) })?;
        let buffer_count = c_int::try_from(buffers.len()).map_err(|_| Self::range())?;
        let rendered = unsafe {
            ffi::vaRenderPicture(
                self.display,
                self.context,
                buffers.as_mut_ptr(),
                buffer_count,
            )
        };
        let end = unsafe { ffi::vaEndPicture(self.display, self.context) };
        Self::check(rendered)?;
        Self::check(end)?;
        // Sound: the surface was just submitted.
        Self::check(unsafe { ffi::vaSyncSurface(self.display, target) })
    }

    /// Buffer holding `len` bytes at `data`.
    #[cfg(target_os = "linux")]
    fn create_buffer(
        &self,
        buffer_type: c_int,
        len: usize,
        data: *const c_void,
    ) -> Result<VaBufferId, VaApiError> {
        let mut buffer = 0;
        let len_u = u32::try_from(len).map_err(|_| Self::range())?;
        // Sound: the context is live; `data` holds `len` bytes.
        Self::check(unsafe {
            ffi::vaCreateBuffer(
                self.display,
                self.context,
                buffer_type,
                len_u,
                1,
                data.cast_mut(),
                &raw mut buffer,
            )
        })?;
        Ok(buffer)
    }

    /// Dimensions outside the driver range.
    #[cfg(target_os = "linux")]
    fn range() -> VaApiError {
        VaApiError::Unsupported("dimensions outside driver range".to_owned())
    }

    /// Cropped NV12 copy of a derived image.
    #[cfg(target_os = "linux")]
    fn copy_nv12(
        &self,
        image: &ffi::VaImage,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        if image.format.fourcc != ffi::VA_FOURCC_NV12 || image.num_planes < 2 {
            return Err(VaApiError::Unsupported("driver image not nv12".to_owned()));
        }
        let (x, y, width, height) = crop;
        // Sound: the image buffer is live until `vaDestroyImage`.
        let mut data: *mut c_void = std::ptr::null_mut();
        Self::check(unsafe { ffi::vaMapBuffer(self.display, image.buf, &raw mut data) })?;
        let outcome = Self::copy_planes(
            data as *const u8,
            u64::from(image.data_size),
            image.pitches,
            image.offsets,
            (x, y, width, height),
        );
        // Sound: the buffer was mapped above.
        unsafe {
            ffi::vaUnmapBuffer(self.display, image.buf);
        }
        outcome
    }

    /// Plane rows for the crop rectangle.
    #[cfg(target_os = "linux")]
    fn copy_planes(
        data: *const u8,
        len: u64,
        pitches: [u32; 3],
        offsets: [u32; 3],
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        let bad = || VaApiError::Unsupported("driver image layout".to_owned());
        let (x, y, width, height) = crop;
        let chroma_height = height / 2;
        let row_end = u64::from(x).checked_add(u64::from(width)).ok_or_else(bad)?;
        if row_end > u64::from(pitches[0]) || row_end > u64::from(pitches[1]) {
            return Err(bad());
        }
        let end_y = u64::from(offsets[0])
            .checked_add(u64::from(pitches[0]) * u64::from(y.saturating_add(height)))
            .ok_or_else(bad)?;
        let end_uv = u64::from(offsets[1])
            .checked_add(u64::from(pitches[1]) * u64::from(y / 2 + chroma_height))
            .ok_or_else(bad)?;
        if end_y > len || end_uv > len {
            return Err(bad());
        }
        let bytes = usize::try_from(
            u64::from(width) * u64::from(height) + u64::from(width) * u64::from(chroma_height),
        )
        .map_err(|_| bad())?;
        let mut out = Vec::with_capacity(bytes);
        let width_usize = usize::try_from(width).map_err(|_| bad())?;
        // Sound: every row below was bounds-checked against `len` above.
        unsafe {
            for row in 0..height {
                let offset = usize::try_from(
                    u64::from(offsets[0])
                        + u64::from(pitches[0]) * u64::from(y + row)
                        + u64::from(x),
                )
                .map_err(|_| bad())?;
                out.extend_from_slice(std::slice::from_raw_parts(data.add(offset), width_usize));
            }
            for row in 0..chroma_height {
                let offset = usize::try_from(
                    u64::from(offsets[1])
                        + u64::from(pitches[1]) * u64::from(y / 2 + row)
                        + u64::from(x),
                )
                .map_err(|_| bad())?;
                out.extend_from_slice(std::slice::from_raw_parts(data.add(offset), width_usize));
            }
        }
        Ok(out)
    }

    /// VA-API status as a result.
    #[cfg(target_os = "linux")]
    fn check(status: ffi::VaStatus) -> Result<(), VaApiError> {
        if status == ffi::VA_STATUS_SUCCESS {
            Ok(())
        } else {
            Err(VaApiError::Call(status))
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for Backend {
    fn drop(&mut self) {
        // Sound: context, surfaces, and config were created in `open`.
        unsafe {
            ffi::vaDestroyContext(self.display, self.context);
            ffi::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            ffi::vaDestroyConfig(self.display, self.config);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Backend;

    #[test]
    fn copy_planes_crops_rows_from_pitches() {
        let y = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let uv = [9u8, 10, 11, 12];
        let mut image = Vec::new();
        image.extend_from_slice(&y);
        image.extend_from_slice(&uv);
        let out = Backend::copy_planes(
            image.as_ptr(),
            image.len() as u64,
            [4, 4, 0],
            [0, 8, 0],
            (0, 0, 4, 2),
        )
        .expect("in-bounds crop");
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

        let cropped = Backend::copy_planes(
            image.as_ptr(),
            image.len() as u64,
            [4, 4, 0],
            [0, 8, 0],
            (2, 0, 2, 2),
        )
        .expect("offset crop");
        assert_eq!(cropped, vec![3, 4, 7, 8, 11, 12]);
    }

    #[test]
    fn copy_planes_rejects_overruns() {
        let image = [0u8; 12];
        assert!(
            Backend::copy_planes(image.as_ptr(), 12, [4, 4, 0], [0, 8, 0], (0, 0, 8, 2)).is_err()
        );
        assert!(
            Backend::copy_planes(image.as_ptr(), 12, [4, 4, 0], [0, 40, 0], (0, 0, 4, 2)).is_err()
        );
    }
}
