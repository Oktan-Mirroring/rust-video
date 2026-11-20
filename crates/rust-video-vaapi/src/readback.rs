//! Split-phase NV12 readback: the submit thread maps, a worker copies.
//!
//! Driver calls stay on one thread; the worker runs the CPU copy
//! plus nothing else, so submits overlap copies with no lock.

#![allow(unsafe_code)]

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::mem::MaybeUninit;
use std::os::raw::{c_int, c_void};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::backend::{Pool, StatsInner};
use crate::error::VaApiError;
use crate::ffi;
use crate::seam::SurfaceId;
use crate::sys;

use rust_video_caps::reason::{ParseReason as CapsParse, UnsupportedReason as CapsUnsupported};
use rust_video_core::micros;

/// Copied transfer answer route.
enum Complete {
    /// Insert under the key for a later collect.
    Key(u64),
    /// Answer the one-shot reader directly.
    Direct(SyncSender<Done>),
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

/// One copied transfer.
struct Done {
    /// Copied bytes or the copy failure.
    bytes: Result<Vec<u8>, VaApiError>,
    /// Mapped image behind the copy, absent when mapping failed.
    release: Option<Release>,
    /// Copy microseconds.
    copy_us: u64,
    /// Surface behind the picture.
    surface: SurfaceId,
    /// True when the surface was already complete at sync.
    ready: bool,
    /// Engine-wait time.
    waited: Duration,
    /// Derive plus map microseconds.
    map_us: u64,
}

impl Done {
    /// Failed transfer of `surface`, with nothing to release.
    fn failed(surface: SurfaceId, error: VaApiError) -> Self {
        Self {
            bytes: Err(error),
            release: None,
            copy_us: 0,
            surface,
            ready: false,
            waited: Duration::ZERO,
            map_us: 0,
        }
    }
}

/// One queued copy.
struct Job {
    /// Mapped image to copy.
    mapped: Mapped,
    /// Answer route.
    complete: Complete,
}

impl Job {
    /// Copy of `mapped`, answered via `complete`.
    fn new(mapped: Mapped, complete: Complete) -> Self {
        Self { mapped, complete }
    }
}

/// Mapped image awaiting its copy.
///
/// Single owner at a time: the submit thread maps, the worker
/// copies, the submit thread releases.
struct Mapped {
    /// Surface behind the picture.
    surface: SurfaceId,
    /// True when the surface was already complete at sync.
    ready: bool,
    /// Engine-wait time.
    waited: Duration,
    /// Derive plus map microseconds.
    map_us: u64,
    /// Image behind the mapping.
    release: Release,
    /// Mapped bytes.
    data: *const u8,
    /// Mapped byte count.
    len: u64,
    /// Row pitch per plane.
    pitches: [u32; 3],
    /// Byte offset per plane.
    offsets: [u32; 3],
    /// Crop rectangle as `(x, y, width, height)`.
    crop: (u32, u32, u32, u32),
    /// CPU copy matching the image memory.
    method: CopyMethod,
}

// Sound: the mapping outlives the transfer; each hop moves sole
// ownership, and only the worker dereferences `data`.
unsafe impl Send for Mapped {}

/// Readback handle over one decode pool.
///
/// Launch stages a transfer; map-ready maps the oldest past the
/// holdback, so the sync lands one frame before the collect and
/// the engine finishes first. Collect blocks until its generation
/// lands. A spawn failure degrades to synchronous copies.
pub(crate) struct Readback {
    /// Copy jobs, absent after shutdown or a spawn failure.
    tx: Option<SyncSender<Job>>,
    /// Staged transfers awaiting their map, oldest first.
    pending: Mutex<VecDeque<Staged>>,
    /// Landed copies plus their signal.
    shared: Arc<Shared>,
    /// Worker thread, absent after shutdown or a spawn failure.
    worker: Option<JoinHandle<()>>,
    /// Main-thread mapper behind launches.
    local: Reader,
}

impl Readback {
    /// Queued copies before a launch waits.
    const DEPTH: usize = 8;

    /// Worker over `pool`, submitting gauges to `gauges`.
    ///
    /// # Arguments
    ///
    /// * `pool` - decode pool behind the transfers.
    /// * `gauges` - submit gauges of the owning context.
    /// * `width` - coded width in luma samples.
    /// * `height` - coded height in luma samples.
    #[must_use]
    pub(crate) fn spawn(
        pool: &Arc<Pool>,
        gauges: &Arc<StatsInner>,
        width: u32,
        height: u32,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let local = Reader::new(Arc::clone(pool), Arc::clone(gauges), width, height);
        let (tx, rx) = sync_channel::<Job>(Self::DEPTH);
        let landed = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("vaapi-readback".to_owned())
            .spawn(move || {
                for job in rx {
                    let started = Instant::now();
                    let bytes = Reader::copy_planes(
                        job.mapped.data,
                        job.mapped.len,
                        job.mapped.pitches,
                        job.mapped.offsets,
                        job.mapped.crop,
                        job.mapped.method,
                    );
                    let done = Done {
                        bytes,
                        release: Some(job.mapped.release),
                        copy_us: micros(started.elapsed()),
                        surface: job.mapped.surface,
                        ready: job.mapped.ready,
                        waited: job.mapped.waited,
                        map_us: job.mapped.map_us,
                    };
                    match job.complete {
                        Complete::Key(generation) => {
                            landed
                                .results
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .insert(generation, done);
                            landed.ready.notify_all();
                        }
                        Complete::Direct(answer) => {
                            let _ = answer.send(done);
                        }
                    }
                }
            })
            .ok();
        Self {
            tx: worker.is_some().then_some(tx),
            pending: Mutex::new(VecDeque::new()),
            shared,
            worker,
            local,
        }
    }

    /// Stage the transfer of `surface` cropped to `crop`, answered under `generation`.
    pub(crate) fn launch(&self, generation: u64, surface: SurfaceId, crop: (u32, u32, u32, u32)) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Staged::new(generation, surface, crop));
    }

    /// Map staged transfers down to `depth - 1` awaiting, oldest first.
    pub(crate) fn map_ready(&self, depth: usize) {
        let threshold = depth.max(1) - 1;
        loop {
            let staged = {
                let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                if pending.len() < threshold {
                    return;
                }
                pending.pop_front()
            };
            let Some(staged) = staged else {
                return;
            };
            self.map_and_queue(staged);
        }
    }

    /// Map the staged transfer, queueing its copy.
    fn map_and_queue(&self, staged: Staged) {
        let mapped = match self.local.prepare(staged.surface, staged.crop) {
            Ok(mapped) => mapped,
            Err(error) => {
                self.finish_key(staged.generation, Done::failed(staged.surface, error));
                return;
            }
        };
        if let Some(tx) = self.tx.as_ref() {
            match tx.send(Job::new(mapped, Complete::Key(staged.generation))) {
                Ok(()) => {}
                Err(returned) => {
                    self.finish_key(staged.generation, Reader::copy_inline(&returned.0.mapped));
                }
            }
            return;
        }
        self.finish_key(staged.generation, Reader::copy_inline(&mapped));
    }

    /// Map staged transfers through `generation`, oldest first.
    fn map_through(&self, generation: u64) {
        loop {
            let staged = {
                let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                match pending.front() {
                    Some(front) if front.generation <= generation => pending.pop_front(),
                    _ => None,
                }
            };
            let Some(staged) = staged else {
                return;
            };
            self.map_and_queue(staged);
        }
    }

    /// Bytes queued under `generation`, blocking until the copy lands.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver image is not NV12, when its
    /// layout overruns its bytes, or when any call fails.
    pub(crate) fn collect(&self, generation: u64) -> Result<Vec<u8>, VaApiError> {
        self.map_through(generation);
        let mut landed = self
            .shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(done) = landed.remove(&generation) {
                drop(landed);
                return self.finish(done);
            }
            landed = self
                .shared
                .ready
                .wait(landed)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// NV12 bytes of `surface` cropped to `crop`, inline with the caller.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver image is not NV12, when its
    /// layout overruns its bytes, or when any call fails.
    pub(crate) fn read_sync(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Vec<u8>, VaApiError> {
        loop {
            let mapped = self.local.prepare(surface, crop)?;
            let Some(tx) = self.tx.as_ref() else {
                return self.finish(Reader::copy_inline(&mapped));
            };
            let (answer, recv) = sync_channel(1);
            match tx.send(Job::new(mapped, Complete::Direct(answer))) {
                Ok(()) => {}
                Err(returned) => {
                    return self.finish(Reader::copy_inline(&returned.0.mapped));
                }
            }
            if let Ok(done) = recv.recv() {
                return self.finish(done);
            }
        }
    }

    /// Landed `done` under `generation`.
    fn finish_key(&self, generation: u64, done: Done) {
        self.shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(generation, done);
        self.shared.ready.notify_all();
    }

    /// Released bytes of `done`, gauged.
    fn finish(&self, done: Done) -> Result<Vec<u8>, VaApiError> {
        let mut unmap_us = 0;
        if let Some(release) = done.release {
            unmap_us = micros(self.local.release(release));
        }
        self.local.gauges.note_read(
            done.surface,
            done.ready,
            done.waited,
            Duration::from_micros(done.map_us + done.copy_us + unmap_us),
        );
        done.bytes
    }

    /// Queued copies drain, the worker joins, orphaned maps release.
    fn shutdown(&mut self) {
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let mut landed = self
            .shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (_, done) in landed.drain() {
            if let Some(release) = done.release {
                self.local.release(release);
            }
        }
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// NV12 transfer mapper over one decode pool.
pub(crate) struct Reader {
    /// Owned decode pool, shared with live frames.
    pool: Arc<Pool>,
    /// Submit gauges of the owning context.
    gauges: Arc<StatsInner>,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Cached NV12 image format for the created-image transfer.
    nv12: Cell<Option<ffi::VaImageFormat>>,
}

impl Reader {
    /// Transfer mapper over `pool`, submitting gauges to `gauges`.
    fn new(pool: Arc<Pool>, gauges: Arc<StatsInner>, width: u32, height: u32) -> Self {
        Self {
            pool,
            gauges,
            width,
            height,
            nv12: Cell::new(None),
        }
    }

    /// True when `surface` already finished, without blocking.
    ///
    /// Errors read as unfinished; the later sync surfaces them.
    #[must_use]
    pub(crate) fn poll_surface(display: ffi::VaDisplay, surface: SurfaceId) -> bool {
        let mut status = 0;
        // Sound: the surface belongs to the pool; the status outlives the call.
        let outcome = unsafe { sys::vaQuerySurfaceStatus(display, surface, &raw mut status) };
        outcome == ffi::VA_STATUS_SUCCESS
            && (status == ffi::VA_SURFACE_READY || status == ffi::VA_SURFACE_SKIPPED)
    }

    /// Block until `surface` finishes decoding.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver reports the surface failed.
    pub(crate) fn sync_surface(
        display: ffi::VaDisplay,
        surface: SurfaceId,
    ) -> Result<(), VaApiError> {
        // Sound: the surface belongs to the pool.
        sys::status(unsafe { sys::vaSyncSurface(display, surface) })
    }

    /// Synced mapping of `surface` cropped to `crop`.
    fn prepare(
        &self,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<Mapped, VaApiError> {
        let (x, y, width, height) = crop;
        if x.saturating_add(width) > self.width || y.saturating_add(height) > self.height {
            return Err(VaApiError::Parse(CapsParse::FrameCropOutsideCodedSize));
        }
        let display = self.pool.display();
        let wait_start = Instant::now();
        let ready = Self::poll_surface(display, surface);
        let synced = Self::sync_surface(display, surface);
        self.gauges.note_synced();
        synced?;
        let waited = wait_start.elapsed();
        let started = Instant::now();
        let derived = Self::derive(display, surface)?;
        let (image, method, copy_crop) = match derived {
            Some(image) => (image, CopyMethod::Streaming, crop),
            None => (
                self.fallback_image(display, surface, crop)?,
                CopyMethod::Cached,
                (0, 0, width, height),
            ),
        };
        // Sound: the image buffer is live until `vaDestroyImage`.
        let mut data: *mut c_void = std::ptr::null_mut();
        if let Err(error) =
            sys::status(unsafe { sys::vaMapBuffer(display, image.buf, &raw mut data) })
        {
            // Sound: `image` is live.
            unsafe {
                sys::vaDestroyImage(display, image.image_id);
            }
            return Err(error);
        }
        Ok(Mapped {
            surface,
            ready,
            waited,
            map_us: micros(started.elapsed()),
            release: Release {
                buf: image.buf,
                image_id: image.image_id,
            },
            data: data as *const u8,
            len: u64::from(image.data_size),
            pitches: image.pitches,
            offsets: image.offsets,
            crop: copy_crop,
            method,
        })
    }

    /// Unmapped plus destroyed `release`.
    fn release(&self, release: Release) -> Duration {
        let started = Instant::now();
        let display = self.pool.display();
        // Sound: the buffer was mapped above; the image is live.
        unsafe {
            sys::vaUnmapBuffer(display, release.buf);
            sys::vaDestroyImage(display, release.image_id);
        }
        started.elapsed()
    }

    /// Copied `mapped`, inline with the caller.
    fn copy_inline(mapped: &Mapped) -> Done {
        let started = Instant::now();
        let bytes = Self::copy_planes(
            mapped.data,
            mapped.len,
            mapped.pitches,
            mapped.offsets,
            mapped.crop,
            mapped.method,
        );
        Done {
            bytes,
            release: Some(mapped.release),
            copy_us: micros(started.elapsed()),
            surface: mapped.surface,
            ready: mapped.ready,
            waited: mapped.waited,
            map_us: mapped.map_us,
        }
    }

    /// Derived image of `surface`, absent when the driver refuses.
    fn derive(
        display: ffi::VaDisplay,
        surface: SurfaceId,
    ) -> Result<Option<ffi::VaImage>, VaApiError> {
        // Sound: the display and surface are live.
        let mut image = MaybeUninit::<ffi::VaImage>::uninit();
        let derived = unsafe { sys::vaDeriveImage(display, surface, image.as_mut_ptr()) };
        if derived != ffi::VA_STATUS_SUCCESS {
            return Ok(None);
        }
        // Sound: `vaDeriveImage` succeeded, initializing `image`.
        let image = unsafe { image.assume_init() };
        if image.format.fourcc != ffi::VA_FOURCC_NV12 || image.num_planes < 2 {
            // Sound: `image` was derived above.
            unsafe {
                sys::vaDestroyImage(display, image.image_id);
            }
            return Err(VaApiError::Unsupported(
                CapsUnsupported::Nv12WithoutTwoPlanes,
            ));
        }
        Ok(Some(image))
    }

    /// NV12 image of `surface` cropped to `crop` via `vaCreateImage` + `vaGetImage`.
    fn fallback_image(
        &self,
        display: ffi::VaDisplay,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) -> Result<ffi::VaImage, VaApiError> {
        let (x, y, width, height) = crop;
        let width_c = c_int::try_from(width).map_err(|_| Self::range())?;
        let height_c = c_int::try_from(height).map_err(|_| Self::range())?;
        let x_c = c_int::try_from(x).map_err(|_| Self::range())?;
        let y_c = c_int::try_from(y).map_err(|_| Self::range())?;
        let mut format = self.nv12_format()?;
        let mut image = MaybeUninit::<ffi::VaImage>::uninit();
        // Sound: display is initialized; `format` and `image` are valid for the call.
        sys::status(unsafe {
            sys::vaCreateImage(
                display,
                &raw mut format,
                width_c,
                height_c,
                image.as_mut_ptr(),
            )
        })?;
        // Sound: `vaCreateImage` succeeded, initializing `image`.
        let image = unsafe { image.assume_init() };
        // Sound: `surface` and `image.image_id` are live; the crop fits the coded size.
        let got = sys::status(unsafe {
            sys::vaGetImage(display, surface, x_c, y_c, width, height, image.image_id)
        });
        if got.is_err() || image.format.fourcc != ffi::VA_FOURCC_NV12 || image.num_planes < 2 {
            // Sound: `image` was created above.
            unsafe {
                sys::vaDestroyImage(display, image.image_id);
            }
            return got.and(Err(VaApiError::Unsupported(
                CapsUnsupported::Nv12WithoutTwoPlanes,
            )));
        }
        Ok(image)
    }

    /// NV12 image format supported by the display.
    fn nv12_format(&self) -> Result<ffi::VaImageFormat, VaApiError> {
        if let Some(format) = self.nv12.get() {
            return Ok(format);
        }
        let format = Self::query_nv12_format(self.pool.display())?;
        self.nv12.set(Some(format));
        Ok(format)
    }

    /// NV12 image format probed from the driver.
    fn query_nv12_format(display: ffi::VaDisplay) -> Result<ffi::VaImageFormat, VaApiError> {
        // Sound: `formats` is sized to the driver's reported capacity.
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
                .ok_or_else(|| VaApiError::NoNv12ImageFormat)
        }
    }

    /// Dimensions outside the driver range.
    fn range() -> VaApiError {
        VaApiError::Parse(CapsParse::Range {
            syntax: "dimensions",
        })
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
        let bad = || VaApiError::Unsupported(CapsUnsupported::PlaneShortOfStride);
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
        let bad = || VaApiError::Unsupported(CapsUnsupported::PlaneShortOfStride);
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
}

/// Mapped image behind a copy.
#[derive(Clone, Copy)]
struct Release {
    /// Mapped image data buffer.
    buf: ffi::VaBufferId,
    /// Mapped image handle.
    image_id: ffi::VaImageId,
}

/// Landed copies plus their signal.
#[derive(Default)]
struct Shared {
    /// Copies by generation.
    results: Mutex<HashMap<u64, Done>>,
    /// Landed-copy signal.
    ready: Condvar,
}

/// One staged transfer awaiting its map.
#[derive(Clone, Copy)]
struct Staged {
    /// Generation answering the transfer.
    generation: u64,
    /// Surface behind the picture.
    surface: SurfaceId,
    /// Crop rectangle as `(x, y, width, height)`.
    crop: (u32, u32, u32, u32),
}

impl Staged {
    /// Transfer of `surface` cropped to `crop`, answered under `generation`.
    fn new(generation: u64, surface: SurfaceId, crop: (u32, u32, u32, u32)) -> Self {
        Self {
            generation,
            surface,
            crop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CopyMethod, Reader};

    #[test]
    fn copy_planes_crops_rows_from_pitches() {
        let y = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let uv = [9u8, 10, 11, 12];
        let mut image = Vec::new();
        image.extend_from_slice(&y);
        image.extend_from_slice(&uv);
        for method in [CopyMethod::Streaming, CopyMethod::Cached] {
            let out = Reader::copy_planes(
                image.as_ptr(),
                image.len() as u64,
                [4, 4, 0],
                [0, 8, 0],
                (0, 0, 4, 2),
                method,
            )
            .expect("in-bounds crop");
            assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

            let cropped = Reader::copy_planes(
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
        assert!(Reader::copy_planes(
            image.as_ptr(),
            12,
            [4, 4, 0],
            [0, 8, 0],
            (0, 0, 8, 2),
            CopyMethod::Streaming,
        )
        .is_err());
        assert!(Reader::copy_planes(
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
            let whole = Reader::copy_planes(
                image.as_ptr(),
                9,
                [3, 3, 0],
                [0, 6, 0],
                (0, 0, 3, 2),
                method,
            )
            .expect("whole planes");
            assert_eq!(whole, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
            let offset = Reader::copy_planes(
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
