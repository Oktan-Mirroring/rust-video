//! VA-API decode context: surfaces, submit, and readback.

#![allow(unsafe_code)]

use std::cell::{Cell, UnsafeCell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::raw::c_int;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::buffer::{bytes_of, VaBuffer};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi;
use crate::ffi::{VaBufferId, VaConfigId, VaContextId};
use crate::seam::{ContextId, SurfaceId, VaSeam};
use crate::sys;

/// VA-API decode context over a surface pool.
///
/// `Send`, never `Sync`: hand the context to another thread when idle,
/// but run every call on one thread. The contract matches the display
/// behind it; see `VaApiDevice`.
pub struct Backend {
    /// Owned decode pool, shared with live frames.
    pool: Arc<Pool>,
    /// VA-API profile the config was built for.
    profile: c_int,
    /// Export lease per pool slot; a live frame holds a clone.
    guards: Vec<Arc<()>>,
    /// Rotating parameter-buffer sets, one per in-flight picture.
    sets: Vec<BufferSet>,
    /// Next set to submit on.
    next: usize,
    /// Surface last submitted per set, synced before the set is reused.
    last: Vec<Option<SurfaceId>>,
    /// Submit gauges, shared with exported frames syncing outside.
    gauges: Arc<StatsInner>,
    /// Scratch render list, reused across submits.
    render: Vec<VaBufferId>,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Pool size.
    count: usize,
    /// Cached NV12 image format for the created-image transfer.
    nv12: Cell<Option<ffi::VaImageFormat>>,
    /// Thread contract: `Send` without `Sync`.
    _thread: PhantomData<UnsafeCell<()>>,
}

/// CPU copy matching the image memory behind the transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyMethod {
    /// Non-temporal loads for derived video memory.
    Streaming,
    /// Plain copy for created CPU-local images.
    Cached,
}

impl CopyMethod {
    /// True when the method issues streaming loads needing a fence.
    fn needs_fence(self) -> bool {
        self == Self::Streaming
    }
}

impl Backend {
    /// In-flight picture ceiling; each slot owns reusable buffers.
    const ASYNC_DEPTH: usize = 4;

    /// Context for `profile` at `width` x `height` with `count` surfaces.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver refuses the config, the surfaces,
    /// or the context.
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
        sys::status(unsafe {
            sys::vaCreateConfig(
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
            sys::vaCreateSurfaces(
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
        if let Err(error) = sys::status(surfaces_status) {
            // Sound: `config` was created above.
            unsafe {
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let mut context = 0;
        // Sound: the render-target array outlives context creation.
        let context_status = unsafe {
            sys::vaCreateContext(
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
        if let Err(error) = sys::status(context_status) {
            // Sound: the surfaces and config were created above.
            unsafe {
                sys::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                sys::vaDestroyConfig(display, config);
            }
            return Err(error);
        }
        let gauges = Arc::new(StatsInner::default());
        let mut sets = Vec::with_capacity(Self::ASYNC_DEPTH);
        for _ in 0..Self::ASYNC_DEPTH {
            match BufferSet::open(display, context, &gauges) {
                Ok(set) => sets.push(set),
                Err(error) => {
                    drop(sets);
                    // Sound: the context, surfaces, and config were created above.
                    unsafe {
                        sys::vaDestroyContext(display, context);
                        sys::vaDestroySurfaces(display, surfaces.as_mut_ptr(), count_c);
                        sys::vaDestroyConfig(display, config);
                    }
                    return Err(error);
                }
            }
        }
        let pool = Pool {
            display,
            config,
            context,
            surfaces,
            count_c,
        };
        Ok(Self {
            pool: Arc::new(pool),
            profile,
            guards: (0..count).map(|_| Arc::new(())).collect(),
            sets,
            next: 0,
            last: vec![None; Self::ASYNC_DEPTH],
            gauges,
            render: Vec::new(),
            width,
            height,
            count,
            nv12: Cell::new(None),
            _thread: PhantomData,
        })
    }

    /// Native decode-context handle behind the backend.
    ///
    /// See [`ContextId`](crate::seam::ContextId) for the threading and
    /// lifetime rules.
    #[must_use]
    pub fn context(&self) -> ContextId {
        self.pool.context
    }

    /// Submit gauges since open.
    #[must_use]
    pub fn stats(&self) -> SubmitStats {
        self.gauges.snapshot()
    }

    /// Phase timings since open.
    #[must_use]
    pub fn phase_stats(&self) -> PhaseStats {
        self.gauges.phase_snapshot()
    }

    /// Shared gauges for frames syncing outside the backend.
    #[must_use]
    pub(crate) fn gauges(&self) -> Arc<StatsInner> {
        Arc::clone(&self.gauges)
    }

    /// Owned decode pool, shared with live frames.
    #[must_use]
    pub(crate) fn pool(&self) -> Arc<Pool> {
        Arc::clone(&self.pool)
    }

    /// Pipelined submit over byte-level parameter accessors.
    fn submit_inner<'d, P, F>(
        &mut self,
        target: SurfaceId,
        picture: &[MaybeUninit<u8>],
        matrix: Option<&[MaybeUninit<u8>]>,
        count: usize,
        params: P,
        data: F,
    ) -> Result<(), VaApiError>
    where
        P: Fn(usize) -> &'d [MaybeUninit<u8>],
        F: Fn(usize) -> &'d [u8],
    {
        let started = Instant::now();
        let set = self.next;
        self.next = (self.next + 1) % Self::ASYNC_DEPTH;
        if let Some(previous) = self.last[set].replace(target) {
            self.sync(previous)?;
        }
        self.fill(set, picture, matrix, count, params, data)?;
        // Sound: the context and target are live; the buffers were created above.
        sys::status(unsafe { sys::vaBeginPicture(self.pool.display, self.pool.context, target) })?;
        let outcome = self.render();
        if outcome.is_err() {
            self.gauges.note_reset();
            self.sync_quiet(target);
        } else {
            self.gauges.note_submitted();
            self.gauges.note_submit(target, started);
        }
        outcome
    }

    /// Render list of the begun picture; the end runs on every path.
    fn render(&mut self) -> Result<(), VaApiError> {
        let buffer_count = c_int::try_from(self.render.len()).map_err(|_| Self::range())?;
        let rendered = unsafe {
            sys::vaRenderPicture(
                self.pool.display,
                self.pool.context,
                self.render.as_mut_ptr(),
                buffer_count,
            )
        };
        let end = unsafe { sys::vaEndPicture(self.pool.display, self.pool.context) };
        sys::status(rendered)?;
        sys::status(end)
    }

    /// Rewrite set `set` for the next picture; the render list is rebuilt.
    fn fill<'d, P, F>(
        &mut self,
        set: usize,
        picture: &[MaybeUninit<u8>],
        matrix: Option<&[MaybeUninit<u8>]>,
        count: usize,
        params: P,
        data: F,
    ) -> Result<(), VaApiError>
    where
        P: Fn(usize) -> &'d [MaybeUninit<u8>],
        F: Fn(usize) -> &'d [u8],
    {
        let display = self.pool.display;
        let context = self.pool.context;
        let gauges = Arc::clone(&self.gauges);
        let Some(entry) = self.sets.get_mut(set) else {
            return Err(Self::range());
        };
        entry.grow(display, context, count, &gauges)?;
        let mut recreated = 0;
        recreated += u64::from(entry.picture.rewrite_uninit(picture)?);
        self.render.clear();
        self.render.reserve(2 + 2 * count);
        self.render.push(entry.picture.id());
        if let Some(matrix) = matrix {
            recreated += u64::from(entry.matrix.rewrite_uninit(matrix)?);
            self.render.push(entry.matrix.id());
        }
        for index in 0..count {
            let param = &mut entry.slice_params[index];
            recreated += u64::from(param.rewrite_uninit(params(index))?);
            let param_id = param.id();
            let slot = &mut entry.slice_data[index];
            recreated += u64::from(slot.rewrite(data(index))?);
            let slot_id = slot.id();
            self.render.push(param_id);
            self.render.push(slot_id);
        }
        gauges.note_buffer_creates(recreated);
        Ok(())
    }

    /// Dimensions outside the driver range.
    fn range() -> VaApiError {
        VaApiError::Unsupported("dimensions outside driver range".to_owned())
    }

    /// Cropped NV12 copy of an image.
    fn copy_nv12(
        &self,
        image: &ffi::VaImage,
        crop: (u32, u32, u32, u32),
        method: CopyMethod,
    ) -> Result<Vec<u8>, VaApiError> {
        if image.format.fourcc != ffi::VA_FOURCC_NV12 || image.num_planes < 2 {
            return Err(VaApiError::Unsupported("driver image not nv12".to_owned()));
        }
        let (x, y, width, height) = crop;
        // Sound: the image buffer is live until `vaDestroyImage`.
        let mut data: *mut c_void = std::ptr::null_mut();
        sys::status(unsafe { sys::vaMapBuffer(self.pool.display, image.buf, &raw mut data) })?;
        let outcome = Self::copy_planes(
            data as *const u8,
            u64::from(image.data_size),
            image.pitches,
            image.offsets,
            (x, y, width, height),
            method,
        );
        // Sound: the buffer was mapped above.
        unsafe {
            sys::vaUnmapBuffer(self.pool.display, image.buf);
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
        method: CopyMethod,
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
        let dst_base = out.as_mut_ptr();
        if x == 0 && width > 0 && width == pitches[0] && width == pitches[1] {
            Self::copy_whole_planes(data, pitches, offsets, crop, dst_base, method)?;
            // SAFETY: `copy_whole_planes` initialized all `bytes` elements (`luma + chroma`).
            unsafe {
                out.set_len(bytes);
            }
            return Ok(out);
        }
        let width_usize = usize::try_from(width).map_err(|_| bad())?;
        let mut written = 0usize;
        // SAFETY: every row span was bounds-checked against `len` and `bytes` above.
        unsafe {
            for row in 0..height {
                let offset = usize::try_from(
                    u64::from(offsets[0])
                        + u64::from(pitches[0]) * u64::from(y + row)
                        + u64::from(x),
                )
                .map_err(|_| bad())?;
                Self::copy_span(data.add(offset), dst_base.add(written), width_usize, method);
                written += width_usize;
            }
            for row in 0..chroma_height {
                let offset = usize::try_from(
                    u64::from(offsets[1])
                        + u64::from(pitches[1]) * u64::from(y / 2 + row)
                        + u64::from(x),
                )
                .map_err(|_| bad())?;
                Self::copy_span(data.add(offset), dst_base.add(written), width_usize, method);
                written += width_usize;
            }
            if method.needs_fence() {
                Self::fence_uswc();
            }
            debug_assert_eq!(written, bytes);
            out.set_len(bytes);
        }
        Ok(out)
    }

    /// Full-width planes in two copies; rows are contiguous by pitch match.
    fn copy_whole_planes(
        data: *const u8,
        pitches: [u32; 3],
        offsets: [u32; 3],
        crop: (u32, u32, u32, u32),
        dst: *mut u8,
        method: CopyMethod,
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
        // SAFETY: both spans were bounds-checked against `len` and `dst` capacity by caller.
        unsafe {
            Self::copy_span(data.add(y_offset), dst, luma, method);
            Self::copy_span(data.add(uv_offset), dst.add(luma), chroma, method);
            if method.needs_fence() {
                Self::fence_uswc();
            }
        }
        Ok(())
    }

    /// Copy of `len` bytes from mapped image memory `src` to cached `dst`.
    ///
    /// # Safety
    ///
    /// `src` and `dst` must be valid, non-overlapping regions of `len` bytes.
    #[inline]
    unsafe fn copy_span(src: *const u8, dst: *mut u8, len: usize, method: CopyMethod) {
        if method == CopyMethod::Cached {
            // SAFETY: pointer invariants upheld by caller.
            unsafe {
                std::ptr::copy_nonoverlapping(src, dst, len);
            }
            return;
        }
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse4.1") {
                // SAFETY: SSE4.1 verified above; pointer invariants upheld by caller.
                unsafe {
                    Self::copy_uswc_sse41(src, dst, len);
                }
                return;
            }
        }
        // SAFETY: pointer invariants upheld by caller.
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst, len);
        }
    }

    /// SSE4.1 `MOVNTDQA` streaming load loop for USWC readback memory.
    ///
    /// # Safety
    ///
    /// Caller must ensure SSE4.1 is available and `src`/`dst` span `len` non-overlapping bytes.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "sse4.1")]
    unsafe fn copy_uswc_sse41(src: *const u8, dst: *mut u8, len: usize) {
        use std::arch::x86_64::{__m128i, _mm_stream_load_si128};

        // SAFETY: caller guarantees `len` valid bytes at `src`.
        let src_bytes = unsafe { std::slice::from_raw_parts(src, len) };
        let (head, vectors, tail) = src_bytes.align_to::<__m128i>();
        // SAFETY: caller guarantees `len` valid bytes at `dst`; the three spans partition it.
        unsafe {
            std::ptr::copy_nonoverlapping(head.as_ptr(), dst, head.len());
        }
        let mut out = head.len();
        let mut chunks = vectors.chunks_exact(4);
        for chunk in &mut chunks {
            // SAFETY: `chunk` holds four aligned vectors; `dst[out..out + 64]` is valid.
            unsafe {
                let base = chunk.as_ptr();
                let v0 = _mm_stream_load_si128(base);
                let v1 = _mm_stream_load_si128(base.add(1));
                let v2 = _mm_stream_load_si128(base.add(2));
                let v3 = _mm_stream_load_si128(base.add(3));
                let slot = dst.add(out);
                Self::store_vector(slot, v0);
                Self::store_vector(slot.add(16), v1);
                Self::store_vector(slot.add(32), v2);
                Self::store_vector(slot.add(48), v3);
            }
            out += 64;
        }
        for vector in chunks.remainder() {
            // SAFETY: `vector` is one aligned vector; `dst[out..out + 16]` is valid.
            unsafe {
                let loaded = _mm_stream_load_si128(std::ptr::from_ref(vector));
                Self::store_vector(dst.add(out), loaded);
            }
            out += 16;
        }
        // SAFETY: consumed spans plus `tail` equal `len` bytes inside both regions.
        unsafe {
            std::ptr::copy_nonoverlapping(tail.as_ptr(), dst.add(out), tail.len());
        }
    }

    /// Cached store of one streamed vector as bytes.
    ///
    /// # Safety
    ///
    /// `dst` must span 16 valid bytes.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    unsafe fn store_vector(dst: *mut u8, vector: std::arch::x86_64::__m128i) {
        // SAFETY: `__m128i` is 16 bytes; `dst` spans 16 bytes; `[u8; 16]` needs no alignment.
        unsafe {
            let bytes = std::mem::transmute::<std::arch::x86_64::__m128i, [u8; 16]>(vector);
            dst.cast::<[u8; 16]>().write_unaligned(bytes);
        }
    }

    /// Memory fence serializing non-temporal streaming loads.
    #[inline]
    unsafe fn fence_uswc() {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `mfence` is baseline x86_64 SSE2.
        unsafe {
            std::arch::x86_64::_mm_mfence();
        }
    }

    /// NV12 bytes of `surface` cropped to `crop`, after its sync.
    fn read_image(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        let (x, y, width, height) = crop;
        if x.saturating_add(width) > self.width || y.saturating_add(height) > self.height {
            return Err(VaApiError::Unsupported(
                "crop outside coded size".to_owned(),
            ));
        }
        // SAFETY: the display and surface are live.
        let mut image = MaybeUninit::<ffi::VaImage>::uninit();
        let derived = unsafe { sys::vaDeriveImage(self.pool.display, surface, image.as_mut_ptr()) };
        if derived == ffi::VA_STATUS_SUCCESS {
            // SAFETY: `vaDeriveImage` succeeded, initializing `image`.
            let image = unsafe { image.assume_init() };
            let outcome = self.copy_nv12(&image, crop, CopyMethod::Streaming);
            // SAFETY: `image` was derived above.
            unsafe {
                sys::vaDestroyImage(self.pool.display, image.image_id);
            }
            return outcome;
        }
        self.read_get_image(surface, crop)
    }

    /// Fallback NV12 readback via `vaCreateImage` + `vaGetImage` when `vaDeriveImage` refuses.
    fn read_get_image(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        let (x, y, width, height) = crop;
        let width_c = c_int::try_from(width).map_err(|_| Self::range())?;
        let height_c = c_int::try_from(height).map_err(|_| Self::range())?;
        let x_c = c_int::try_from(x).map_err(|_| Self::range())?;
        let y_c = c_int::try_from(y).map_err(|_| Self::range())?;
        let mut format = self.nv12_format()?;
        let mut image = MaybeUninit::<ffi::VaImage>::uninit();
        // SAFETY: display is initialized; `format` and `image` are valid for the call.
        sys::status(unsafe {
            sys::vaCreateImage(
                self.pool.display,
                &raw mut format,
                width_c,
                height_c,
                image.as_mut_ptr(),
            )
        })?;
        // SAFETY: `vaCreateImage` succeeded, initializing `image`.
        let image = unsafe { image.assume_init() };
        // SAFETY: `surface` and `image.image_id` are live; crop fits within `self.width` x `self.height`.
        let got = sys::status(unsafe {
            sys::vaGetImage(
                self.pool.display,
                surface,
                x_c,
                y_c,
                width,
                height,
                image.image_id,
            )
        });
        let outcome =
            got.and_then(|()| self.copy_nv12(&image, (0, 0, width, height), CopyMethod::Cached));
        // SAFETY: `image` was created above.
        unsafe {
            sys::vaDestroyImage(self.pool.display, image.image_id);
        }
        outcome
    }

    /// NV12 image format supported by the display.
    fn nv12_format(&self) -> Result<ffi::VaImageFormat, VaApiError> {
        if let Some(format) = self.nv12.get() {
            return Ok(format);
        }
        let format = self.query_nv12_format()?;
        self.nv12.set(Some(format));
        Ok(format)
    }

    /// NV12 image format probed from the driver.
    fn query_nv12_format(&self) -> Result<ffi::VaImageFormat, VaApiError> {
        let display = self.pool.display;
        // SAFETY: `formats` is sized to the driver's reported capacity.
        unsafe {
            let capacity = usize::try_from(sys::vaMaxNumImageFormats(display)).unwrap_or(0);
            let mut formats = vec![ffi::VaImageFormat::default(); capacity];
            let mut count = 0;
            sys::status(sys::vaQueryImageFormats(
                display,
                formats.as_mut_ptr(),
                &raw mut count,
            ))?;
            let reported = usize::try_from(count).unwrap_or(0).min(formats.len());
            formats[..reported]
                .iter()
                .find(|format| format.fourcc == ffi::VA_FOURCC_NV12)
                .copied()
                .ok_or_else(|| VaApiError::Unsupported("nv12 image format absent".to_owned()))
        }
    }
}

impl VaSeam for Backend {
    fn submit_slices<'d, T, F>(
        &mut self,
        target: SurfaceId,
        picture: &[MaybeUninit<u8>],
        matrix: Option<&[MaybeUninit<u8>]>,
        slices: &'d [T],
        data: F,
    ) -> Result<(), VaApiError>
    where
        F: Fn(usize) -> &'d [u8],
    {
        self.submit_inner(
            target,
            picture,
            matrix,
            slices.len(),
            |index| bytes_of(&slices[index]),
            data,
        )
    }

    fn read_nv12(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        let wait_start = Instant::now();
        let ready = self.poll(surface);
        self.sync(surface)?;
        let waited = wait_start.elapsed();
        let copy_start = Instant::now();
        let outcome = self.read_image(surface, crop);
        self.gauges
            .note_read(surface, ready, waited, copy_start.elapsed());
        outcome
    }

    fn sync(&self, surface: SurfaceId) -> Result<(), VaApiError> {
        // Sound: the surface belongs to the pool.
        let outcome = sys::status(unsafe { sys::vaSyncSurface(self.pool.display, surface) });
        self.gauges.note_synced();
        outcome
    }

    fn poll(&self, surface: SurfaceId) -> bool {
        let mut status = 0;
        // Sound: the surface belongs to the pool; the status outlives the call.
        let outcome =
            unsafe { sys::vaQuerySurfaceStatus(self.pool.display, surface, &raw mut status) };
        outcome == ffi::VA_STATUS_SUCCESS
            && (status == ffi::VA_SURFACE_READY || status == ffi::VA_SURFACE_SKIPPED)
    }

    fn surfaces(&self) -> &[SurfaceId] {
        &self.pool.surfaces
    }

    fn surface_guard(&self, index: u32) -> Option<Arc<()>> {
        usize::try_from(index)
            .ok()
            .and_then(|slot| self.guards.get(slot).cloned())
    }

    fn surface_live(&self, index: u32) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|slot| self.guards.get(slot))
            .is_some_and(|guard| Arc::strong_count(guard) > 1)
    }

    fn pool_size(&self) -> usize {
        self.count
    }

    fn profile(&self) -> c_int {
        self.profile
    }

    fn coded_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Buffers first: they belong to the context, which the pool
        // below may keep alive for outstanding frames.
        drop(std::mem::take(&mut self.sets));
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
    fn open(
        display: ffi::VaDisplay,
        context: VaContextId,
        gauges: &StatsInner,
    ) -> Result<Self, VaApiError> {
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
        gauges.note_buffer_creates(2);
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
        gauges: &StatsInner,
    ) -> Result<(), VaApiError> {
        let mut created = 0;
        while self.slice_params.len() < count {
            self.slice_params.push(VaBuffer::create(
                display,
                context,
                ffi::VA_SLICE_PARAMETER_BUFFER_TYPE,
                &[0; size_of::<ffi::VaSliceParameterBufferH264>()],
            )?);
            created += 1;
        }
        while self.slice_data.len() < count {
            self.slice_data.push(VaBuffer::create(
                display,
                context,
                ffi::VA_SLICE_DATA_BUFFER_TYPE,
                &[0],
            )?);
            created += 1;
        }
        gauges.note_buffer_creates(created);
        Ok(())
    }
}

/// Owned decode pool: config, context, and surfaces.
///
/// Shared with live frames: dropping the backend destroys the pool
/// only once the last frame holding it drops.
pub(crate) struct Pool {
    /// Initialized display.
    display: ffi::VaDisplay,
    /// Decode configuration.
    config: VaConfigId,
    /// Decode context.
    context: VaContextId,
    /// Render-target pool.
    surfaces: Vec<SurfaceId>,
    /// Pool size as the driver takes it.
    count_c: c_int,
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Sound: context, surfaces, and config were created in `open`.
        unsafe {
            sys::vaDestroyContext(self.display, self.context);
            sys::vaDestroySurfaces(self.display, self.surfaces.as_mut_ptr(), self.count_c);
            sys::vaDestroyConfig(self.display, self.config);
        }
    }
}

// Sound: moves hand ownership to the new thread; the last drop
// destroys the driver objects, never shared.
unsafe impl Send for Pool {}

// Sound: every field is read-only after `open`; shared references
// only copy handles out.
unsafe impl Sync for Pool {}

/// Submit stamps plus overlap proof behind one decode context.
#[derive(Debug, Default)]
struct PhaseLog {
    /// Submit instant per surface.
    submits: HashMap<SurfaceId, Instant>,
    /// Previous copy end.
    last_copy_end: Option<Instant>,
    /// Waits proving a previous copy ran with this surface in flight.
    overlapped: u64,
}

/// Phase timings since backend open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhaseStats {
    /// Readback calls.
    frames: u64,
    /// Submit microseconds.
    submit_us: u64,
    /// Engine-wait microseconds.
    wait_us: u64,
    /// Readback-copy microseconds.
    copy_us: u64,
    /// Submit-to-ready microseconds.
    latency_us: u64,
    /// Waits finding the surface already complete.
    polled_ready: u64,
    /// Waits proving a previous copy ran with this surface in flight.
    overlapped: u64,
}

impl PhaseStats {
    /// Readback calls.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Submit microseconds.
    #[must_use]
    pub const fn submit_us(&self) -> u64 {
        self.submit_us
    }

    /// Engine-wait microseconds.
    #[must_use]
    pub const fn wait_us(&self) -> u64 {
        self.wait_us
    }

    /// Readback-copy microseconds.
    #[must_use]
    pub const fn copy_us(&self) -> u64 {
        self.copy_us
    }

    /// Submit-to-ready microseconds.
    #[must_use]
    pub const fn latency_us(&self) -> u64 {
        self.latency_us
    }

    /// Waits finding the surface already complete.
    #[must_use]
    pub const fn polled_ready(&self) -> u64 {
        self.polled_ready
    }

    /// Waits proving a previous copy ran with this surface in flight.
    #[must_use]
    pub const fn overlapped(&self) -> u64 {
        self.overlapped
    }
}

/// Shared submit gauges behind one decode context.
#[derive(Debug, Default)]
pub(crate) struct StatsInner {
    /// Pictures submitted.
    submitted: AtomicU64,
    /// Surfaces synced, backend or export path.
    synced: AtomicU64,
    /// Peak submitted-minus-synced.
    peak: AtomicUsize,
    /// Failed submits quieted.
    resets: AtomicU64,
    /// Driver buffers created.
    buffer_creates: AtomicU64,
    /// Readback calls.
    frames: AtomicU64,
    /// Submit microseconds.
    submit_us: AtomicU64,
    /// Engine-wait microseconds.
    wait_us: AtomicU64,
    /// Readback-copy microseconds.
    copy_us: AtomicU64,
    /// Submit-to-ready microseconds.
    latency_us: AtomicU64,
    /// Waits finding the surface already complete.
    polled_ready: AtomicU64,
    /// Submit stamps plus overlap proof.
    phases: Mutex<PhaseLog>,
}

impl StatsInner {
    /// One submitted picture; the peak follows submitted-minus-synced.
    fn note_submitted(&self) {
        let submitted = self.submitted.fetch_add(1, Ordering::Relaxed) + 1;
        let synced = self.synced.load(Ordering::Relaxed);
        let inflight = usize::try_from(submitted.saturating_sub(synced)).unwrap_or(usize::MAX);
        self.peak.fetch_max(inflight, Ordering::Relaxed);
    }

    /// One synced surface.
    pub(crate) fn note_synced(&self) {
        self.synced.fetch_add(1, Ordering::Relaxed);
    }

    /// One failed submit quieted.
    fn note_reset(&self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
    }

    /// `count` driver buffers created.
    fn note_buffer_creates(&self, count: u64) {
        self.buffer_creates.fetch_add(count, Ordering::Relaxed);
    }

    /// One submit of `target` started at `started`.
    pub(crate) fn note_submit(&self, target: SurfaceId, started: Instant) {
        let ended = Instant::now();
        self.submit_us.fetch_add(
            Self::micros(ended.saturating_duration_since(started)),
            Ordering::Relaxed,
        );
        self.lock_phases().submits.insert(target, ended);
    }

    /// One readback of `surface`, polled `ready`, waited `wait`, copied `copy`.
    pub(crate) fn note_read(
        &self,
        surface: SurfaceId,
        ready: bool,
        wait: Duration,
        copy: Duration,
    ) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.wait_us
            .fetch_add(Self::micros(wait), Ordering::Relaxed);
        self.copy_us
            .fetch_add(Self::micros(copy), Ordering::Relaxed);
        if ready {
            self.polled_ready.fetch_add(1, Ordering::Relaxed);
        }
        let mut phases = self.lock_phases();
        let copy_end = Instant::now();
        if let Some(submitted) = phases.submits.get(&surface).copied() {
            self.latency_us.fetch_add(
                Self::micros(copy_end.saturating_duration_since(submitted)),
                Ordering::Relaxed,
            );
            if !ready && phases.last_copy_end.is_some_and(|end| end > submitted) {
                phases.overlapped += 1;
            }
        }
        phases.last_copy_end = Some(copy_end);
    }

    /// Whole microseconds, saturating.
    fn micros(duration: Duration) -> u64 {
        u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
    }

    /// Submit stamps plus overlap proof, poison-tolerant.
    fn lock_phases(&self) -> MutexGuard<'_, PhaseLog> {
        self.phases.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gauge snapshot.
    fn snapshot(&self) -> SubmitStats {
        SubmitStats {
            submitted: self.submitted.load(Ordering::Relaxed),
            peak: self.peak.load(Ordering::Relaxed),
            resets: self.resets.load(Ordering::Relaxed),
            buffer_creates: self.buffer_creates.load(Ordering::Relaxed),
        }
    }

    /// Phase-timing snapshot.
    fn phase_snapshot(&self) -> PhaseStats {
        PhaseStats {
            frames: self.frames.load(Ordering::Relaxed),
            submit_us: self.submit_us.load(Ordering::Relaxed),
            wait_us: self.wait_us.load(Ordering::Relaxed),
            copy_us: self.copy_us.load(Ordering::Relaxed),
            latency_us: self.latency_us.load(Ordering::Relaxed),
            polled_ready: self.polled_ready.load(Ordering::Relaxed),
            overlapped: self.lock_phases().overlapped,
        }
    }
}

/// Submit gauges since backend open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubmitStats {
    /// Pictures submitted.
    submitted: u64,
    /// Peak pictures submitted ahead of sync.
    peak: usize,
    /// Failed submits quieted.
    resets: u64,
    /// Driver buffers created by this backend since open.
    buffer_creates: u64,
}

impl SubmitStats {
    /// Pictures submitted.
    #[must_use]
    pub const fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Peak pictures submitted ahead of sync.
    #[must_use]
    pub const fn peak(&self) -> usize {
        self.peak
    }

    /// Failed submits quieted.
    #[must_use]
    pub const fn resets(&self) -> u64 {
        self.resets
    }

    /// Driver buffers created by this backend since open.
    #[must_use]
    pub const fn buffer_creates(&self) -> u64 {
        self.buffer_creates
    }
}

#[cfg(test)]
mod tests {
    use super::{Backend, CopyMethod};

    #[test]
    fn copy_planes_crops_rows_from_pitches() {
        let y = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let uv = [9u8, 10, 11, 12];
        let mut image = Vec::new();
        image.extend_from_slice(&y);
        image.extend_from_slice(&uv);
        for method in [CopyMethod::Streaming, CopyMethod::Cached] {
            let out = Backend::copy_planes(
                image.as_ptr(),
                image.len() as u64,
                [4, 4, 0],
                [0, 8, 0],
                (0, 0, 4, 2),
                method,
            )
            .expect("in-bounds crop");
            assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

            let cropped = Backend::copy_planes(
                image.as_ptr(),
                image.len() as u64,
                [4, 4, 0],
                [0, 8, 0],
                (2, 0, 2, 2),
                method,
            )
            .expect("offset crop");
            assert_eq!(cropped, vec![3, 4, 7, 8, 11, 12]);
        }
    }

    #[test]
    fn copy_planes_rejects_overruns() {
        let image = [0u8; 12];
        assert!(Backend::copy_planes(
            image.as_ptr(),
            12,
            [4, 4, 0],
            [0, 8, 0],
            (0, 0, 8, 2),
            CopyMethod::Streaming,
        )
        .is_err());
        assert!(Backend::copy_planes(
            image.as_ptr(),
            12,
            [4, 4, 0],
            [0, 40, 0],
            (0, 0, 4, 2),
            CopyMethod::Cached,
        )
        .is_err());
    }

    #[test]
    fn whole_plane_path_matches_row_path() {
        let mut image = Vec::new();
        image.extend_from_slice(&[1u8, 2, 3, 4, 5, 6]);
        image.extend_from_slice(&[7u8, 8, 9]);
        for method in [CopyMethod::Streaming, CopyMethod::Cached] {
            let whole = Backend::copy_planes(
                image.as_ptr(),
                9,
                [3, 3, 0],
                [0, 6, 0],
                (0, 0, 3, 2),
                method,
            )
            .expect("whole planes");
            assert_eq!(whole, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
            let offset = Backend::copy_planes(
                image.as_ptr(),
                9,
                [3, 3, 0],
                [0, 6, 0],
                (0, 1, 3, 1),
                method,
            )
            .expect("offset crop");
            assert_eq!(offset, vec![4, 5, 6]);
        }
    }
}
