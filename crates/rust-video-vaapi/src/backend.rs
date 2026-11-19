//! VA-API decode context: surfaces, submit, and readback.

#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::buffer::{bytes_of, VaBuffer};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi;
use crate::ffi::{VaBufferId, VaConfigId, VaContextId};
use crate::readback::{Readback, Reader};
use crate::seam::{ContextId, SurfaceId, VaSeam};
use crate::sys;

use rust_video_caps::delivery::SubmitStats;

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
    /// Readback handle joining submits with copies.
    readback: Readback,
    /// Scratch render list, reused across submits.
    render: Vec<VaBufferId>,
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
        let pool = Self::open_pool(display, profile, width, height, count)?;
        let gauges = Arc::new(StatsInner::default());
        let sets = match Self::open_sets(display, pool.context, &gauges) {
            Ok(sets) => sets,
            Err(error) => {
                drop(pool);
                return Err(error);
            }
        };
        let pool = Arc::new(pool);
        let readback = Readback::spawn(&pool, &gauges, width, height);
        Ok(Self {
            pool,
            profile,
            guards: (0..count).map(|_| Arc::new(())).collect(),
            sets,
            next: 0,
            last: vec![None; Self::ASYNC_DEPTH],
            gauges,
            readback,
            render: Vec::new(),
            width,
            height,
            count,
            _thread: PhantomData,
        })
    }

    /// Config, surfaces, and context for `profile` at `width` x `height`.
    fn open_pool(
        display: ffi::VaDisplay,
        profile: c_int,
        width: u32,
        height: u32,
        count: usize,
    ) -> Result<Pool, VaApiError> {
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
        Ok(Pool {
            display,
            config,
            context,
            surfaces,
            count_c,
        })
    }

    /// Reusable parameter-buffer ring over `context`.
    fn open_sets(
        display: ffi::VaDisplay,
        context: VaContextId,
        gauges: &StatsInner,
    ) -> Result<Vec<BufferSet>, VaApiError> {
        let mut sets = Vec::with_capacity(Self::ASYNC_DEPTH);
        for _ in 0..Self::ASYNC_DEPTH {
            sets.push(BufferSet::open(display, context, gauges)?);
        }
        Ok(sets)
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

    /// Stage the transfer of `surface` cropped to `crop`, answered under `generation`.
    pub(crate) fn launch_read(
        &self,
        generation: u64,
        surface: SurfaceId,
        crop: (u32, u32, u32, u32),
    ) {
        self.readback.launch(generation, surface, crop);
    }

    /// Map staged transfers down to `depth - 1` awaiting, oldest first.
    pub(crate) fn map_ready(&self, depth: usize) {
        self.readback.map_ready(depth);
    }

    /// Bytes queued under `generation`, blocking until the transfer lands.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the driver image is not NV12, when its
    /// layout overruns its bytes, or when any call fails.
    pub(crate) fn collect_read(&self, generation: u64) -> Result<Vec<u8>, VaApiError> {
        self.readback.collect(generation)
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
        self.readback.read_sync(surface, crop)
    }

    fn sync(&self, surface: SurfaceId) -> Result<(), VaApiError> {
        let outcome = Reader::sync_surface(self.pool.display(), surface);
        self.gauges.note_synced();
        outcome
    }

    fn poll(&self, surface: SurfaceId) -> bool {
        Reader::poll_surface(self.pool.display(), surface)
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

impl Pool {
    /// Initialized display behind the pool.
    #[must_use]
    pub(crate) fn display(&self) -> ffi::VaDisplay {
        self.display
    }
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
        SubmitStats::snapshot(
            self.submitted.load(Ordering::Relaxed),
            self.peak.load(Ordering::Relaxed),
            self.resets.load(Ordering::Relaxed),
            self.buffer_creates.load(Ordering::Relaxed),
        )
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
