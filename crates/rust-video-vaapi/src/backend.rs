//! VA-API decode context: surfaces, submit, and readback.
//!
//! The live implementation targets Linux; other platforms decline at
//! `open` with a typed skip.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::os::raw::c_int;
use std::os::raw::c_void;
use std::sync::Arc;

use crate::buffer::{bytes_of, VaBuffer};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi::{self, VaSurfaceId};
use crate::ffi::{VaBufferId, VaConfigId, VaContextId};

/// VA-API decode context over a surface pool.
///
/// `Send`, never `Sync`: hand the context to another thread when idle,
/// but run every call on one thread. The contract matches the display
/// behind it; see `VaApiDevice`.
pub struct Backend {
    /// Initialized display.
    display: ffi::VaDisplay,
    /// VA-API profile the config was built for.
    profile: c_int,
    /// Decode configuration.
    config: VaConfigId,
    /// Decode context.
    context: VaContextId,
    /// Render-target pool.
    surfaces: Vec<VaSurfaceId>,
    /// Export lease per pool slot; a live frame holds a clone.
    guards: Vec<Arc<()>>,
    /// Rotating parameter-buffer sets, one per in-flight picture.
    sets: Vec<BufferSet>,
    /// Next set to submit on.
    next: usize,
    /// Surface last submitted per set, synced before the set is reused.
    last: Vec<Option<VaSurfaceId>>,
    /// Scratch render list, reused across submits.
    render: Vec<VaBufferId>,
    /// Pool size as the driver takes it.
    count_c: c_int,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Pool size.
    count: usize,
    /// Thread contract: `Send` without `Sync`.
    _thread: PhantomData<UnsafeCell<()>>,
}

impl Backend {
    /// In-flight picture ceiling; each slot owns reusable buffers.
    const ASYNC_DEPTH: usize = 4;

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
        VaApiError::status(unsafe {
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
        if let Err(error) = VaApiError::status(surfaces_status) {
            // Sound: `config` was created above.
            unsafe {
                ffi::vaDestroyConfig(display, config);
            }
            return Err(error);
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
        if let Err(error) = VaApiError::status(context_status) {
            // Sound: the surfaces and config were created above.
            unsafe {
                ffi::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                ffi::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let mut sets = Vec::with_capacity(Self::ASYNC_DEPTH);
        for _ in 0..Self::ASYNC_DEPTH {
            match BufferSet::open(display, context) {
                Ok(set) => sets.push(set),
                Err(error) => {
                    drop(sets);
                    // Sound: the context, surfaces, and config were created above.
                    unsafe {
                        ffi::vaDestroyContext(display, context);
                        ffi::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                        ffi::vaDestroyConfig(display, config);
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            display,
            profile,
            config,
            context,
            surfaces,
            guards: (0..count).map(|_| Arc::new(())).collect(),
            sets,
            next: 0,
            last: vec![None; Self::ASYNC_DEPTH],
            render: Vec::new(),
            count_c,
            width,
            height,
            count,
            _thread: PhantomData,
        })
    }

    /// Pool surfaces.
    #[must_use]
    pub fn surfaces(&self) -> &[VaSurfaceId] {
        &self.surfaces
    }

    /// Pool size.
    #[must_use]
    pub fn pool_size(&self) -> usize {
        self.count
    }

    /// Coded size in luma samples.
    #[must_use]
    pub fn coded_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// VA-API profile the config was built for.
    #[must_use]
    pub fn profile(&self) -> c_int {
        self.profile
    }

    /// Lease for pool slot `index`, keeping it out of the free set.
    #[must_use]
    pub fn surface_guard(&self, index: u32) -> Option<Arc<()>> {
        usize::try_from(index)
            .ok()
            .and_then(|slot| self.guards.get(slot).cloned())
    }

    /// True when a live frame holds pool slot `index`.
    #[must_use]
    pub fn surface_live(&self, index: u32) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|slot| self.guards.get(slot))
            .is_some_and(|guard| Arc::strong_count(guard) > 1)
    }

    /// Decode one picture into `target`, returning before it finishes.
    ///
    /// The picture joins at most `ASYNC_DEPTH` in-flight submits; the
    /// set it reuses was synced against its own previous surface, so
    /// the buffers are safe to rewrite. Callers sync at consumption:
    /// `read_nv12` and the DMA-BUF export both sync first. Slice
    /// payloads arrive through `data` so the caller keeps no pairing
    /// allocation; `data` must answer every index below `slices`.
    ///
    /// # Errors
    ///
    /// Returns `Err` off Linux, or when any VA-API call fails.
    pub fn submit<'d, F>(
        &mut self,
        target: VaSurfaceId,
        picture: &ffi::VaPictureParameterBufferH264,
        matrix: &ffi::VaIqMatrixBufferH264,
        slices: &[ffi::VaSliceParameterBufferH264],
        data: F,
    ) -> Result<(), VaApiError>
    where
        F: Fn(usize) -> &'d [u8],
    {
        let set = self.next;
        self.next = (self.next + 1) % Self::ASYNC_DEPTH;
        if let Some(previous) = self.last[set].replace(target) {
            self.sync(previous)?;
        }
        self.fill(set, picture, matrix, slices, data)?;
        // Sound: the context and target are live; the buffers were created above.
        VaApiError::status(unsafe { ffi::vaBeginPicture(self.display, self.context, target) })?;
        let buffer_count = c_int::try_from(self.render.len()).map_err(|_| Self::range())?;
        let rendered = unsafe {
            ffi::vaRenderPicture(
                self.display,
                self.context,
                self.render.as_mut_ptr(),
                buffer_count,
            )
        };
        let end = unsafe { ffi::vaEndPicture(self.display, self.context) };
        VaApiError::status(rendered)?;
        VaApiError::status(end)
    }

    /// Block until `surface` finishes decoding.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver reports the surface failed.
    pub fn sync(&self, surface: VaSurfaceId) -> Result<(), VaApiError> {
        // Sound: the surface belongs to the pool.
        VaApiError::status(unsafe { ffi::vaSyncSurface(self.display, surface) })
    }

    /// Block until `surface` finishes, dropping any failure.
    pub fn sync_quiet(&self, surface: VaSurfaceId) {
        let _ = self.sync(surface);
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
        self.sync(surface)?;
        let (x, y, width, height) = crop;
        if x.saturating_add(width) > self.width || y.saturating_add(height) > self.height {
            return Err(VaApiError::Unsupported(
                "crop outside coded size".to_owned(),
            ));
        }
        // Sound: the display and surface are live.
        let mut image = std::mem::MaybeUninit::<ffi::VaImage>::uninit();
        VaApiError::status(unsafe {
            ffi::vaDeriveImage(self.display, surface, image.as_mut_ptr())
        })?;
        // Sound: `vaDeriveImage` succeeded, so the image is initialized.
        let image = unsafe { image.assume_init() };
        let outcome = self.copy_nv12(&image, crop);
        // Sound: the image was derived above.
        unsafe {
            ffi::vaDestroyImage(self.display, image.image_id);
        }
        outcome
    }

    /// Rewrite set `set` for the next picture; the render list is rebuilt.
    fn fill<'d, F>(
        &mut self,
        set: usize,
        picture: &ffi::VaPictureParameterBufferH264,
        matrix: &ffi::VaIqMatrixBufferH264,
        slices: &[ffi::VaSliceParameterBufferH264],
        data: F,
    ) -> Result<(), VaApiError>
    where
        F: Fn(usize) -> &'d [u8],
    {
        let display = self.display;
        let context = self.context;
        let Some(entry) = self.sets.get_mut(set) else {
            return Err(Self::range());
        };
        entry.grow(display, context, slices.len())?;
        entry.picture.rewrite(bytes_of(picture))?;
        entry.matrix.rewrite(bytes_of(matrix))?;
        self.render.clear();
        self.render.reserve(2 + 2 * slices.len());
        self.render.push(entry.picture.id());
        self.render.push(entry.matrix.id());
        for (index, params) in slices.iter().enumerate() {
            let param = &mut entry.slice_params[index];
            param.rewrite(bytes_of(params))?;
            let param_id = param.id();
            let slot = &mut entry.slice_data[index];
            slot.rewrite(data(index))?;
            let slot_id = slot.id();
            self.render.push(param_id);
            self.render.push(slot_id);
        }
        Ok(())
    }

    /// Dimensions outside the driver range.
    fn range() -> VaApiError {
        VaApiError::Unsupported("dimensions outside driver range".to_owned())
    }

    /// Cropped NV12 copy of a derived image.
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
        VaApiError::status(unsafe { ffi::vaMapBuffer(self.display, image.buf, &raw mut data) })?;
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
        if x == 0 && width > 0 && width == pitches[0] && width == pitches[1] {
            return Self::copy_whole_planes(data, pitches, offsets, crop, &mut out).map(|()| out);
        }
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

    /// Full-width planes in two copies; rows are contiguous by the pitch match.
    fn copy_whole_planes(
        data: *const u8,
        pitches: [u32; 3],
        offsets: [u32; 3],
        crop: (u32, u32, u32, u32),
        out: &mut Vec<u8>,
    ) -> Result<(), VaApiError> {
        let bad = || VaApiError::Unsupported("driver image layout".to_owned());
        let (_, y, width, height) = crop;
        let luma = usize::try_from(u64::from(width) * u64::from(height)).map_err(|_| bad())?;
        let chroma =
            usize::try_from(u64::from(width) * u64::from(height / 2)).map_err(|_| bad())?;
        let y_offset =
            usize::try_from(u64::from(offsets[0]) + u64::from(pitches[0]) * u64::from(y))
                .map_err(|_| bad())?;
        let uv_offset =
            usize::try_from(u64::from(offsets[1]) + u64::from(pitches[1]) * u64::from(y / 2))
                .map_err(|_| bad())?;
        // Sound: both spans were bounds-checked against `len` by the caller.
        unsafe {
            out.extend_from_slice(std::slice::from_raw_parts(data.add(y_offset), luma));
            out.extend_from_slice(std::slice::from_raw_parts(data.add(uv_offset), chroma));
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.sets));
        // Sound: context, surfaces, and config were created in `open`.
        unsafe {
            ffi::vaDestroyContext(self.display, self.context);
            ffi::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            ffi::vaDestroyConfig(self.display, self.config);
        }
    }
}

// Sound: a move hands exclusive ownership to the new thread; the
// context destroys wherever the value drops, never shared.
unsafe impl Send for Backend {}

/// Reusable parameter buffers for one in-flight picture.
struct BufferSet {
    /// Picture-parameter buffer.
    picture: VaBuffer,
    /// Inverse-quantization-matrix buffer.
    matrix: VaBuffer,
    /// Slice-parameter buffers, grown to the picture slice count.
    slice_params: Vec<VaBuffer>,
    /// Slice-data buffers, grown to the picture slice count.
    slice_data: Vec<VaBuffer>,
}

impl BufferSet {
    /// Set with fresh picture and matrix buffers, no slice buffers yet.
    fn open(display: ffi::VaDisplay, context: VaContextId) -> Result<Self, VaApiError> {
        let picture = VaBuffer::create(
            display,
            context,
            ffi::VA_PICTURE_PARAMETER_BUFFER_TYPE,
            &[0; size_of::<ffi::VaPictureParameterBufferH264>()],
        )?;
        let matrix = VaBuffer::create(
            display,
            context,
            ffi::VA_IQ_MATRIX_BUFFER_TYPE,
            &[0; size_of::<ffi::VaIqMatrixBufferH264>()],
        )?;
        Ok(Self {
            picture,
            matrix,
            slice_params: Vec::new(),
            slice_data: Vec::new(),
        })
    }

    /// Slice buffers for `count` slices, creating the shortfall.
    fn grow(
        &mut self,
        display: ffi::VaDisplay,
        context: VaContextId,
        count: usize,
    ) -> Result<(), VaApiError> {
        while self.slice_params.len() < count {
            self.slice_params.push(VaBuffer::create(
                display,
                context,
                ffi::VA_SLICE_PARAMETER_BUFFER_TYPE,
                &[0; size_of::<ffi::VaSliceParameterBufferH264>()],
            )?);
        }
        while self.slice_data.len() < count {
            self.slice_data.push(VaBuffer::create(
                display,
                context,
                ffi::VA_SLICE_DATA_BUFFER_TYPE,
                &[0],
            )?);
        }
        Ok(())
    }
}

#[cfg(test)]
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

    #[test]
    fn whole_plane_path_matches_row_path() {
        let mut image = Vec::new();
        image.extend_from_slice(&[1u8, 2, 3, 4, 5, 6]);
        image.extend_from_slice(&[7u8, 8, 9]);
        let whole = Backend::copy_planes(image.as_ptr(), 9, [3, 3, 0], [0, 6, 0], (0, 0, 3, 2))
            .expect("whole planes");
        assert_eq!(whole, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let offset = Backend::copy_planes(image.as_ptr(), 9, [3, 3, 0], [0, 6, 0], (0, 1, 3, 1));
        assert!(offset.is_ok());
    }
}
