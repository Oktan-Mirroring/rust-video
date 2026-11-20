//! D3D12 maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::D3d12Error;
use crate::h264::frontend::Crop;
use crate::submit::{visible_rect, Nv12Layout};

use bytes::Bytes;
use rust_video_core::{CopyPump, InlineCopy};
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

/// One copied transfer.
pub(crate) struct Done {
    /// Decode slot behind the output.
    slot: u8,
    /// Copied bytes or the copy failure.
    bytes: Result<Vec<u8>, D3d12Error>,
    /// Mapping behind the copy, released by the submit thread.
    release: Release,
    /// Copy time.
    copy: Duration,
}

/// Mapping release: unmap on the submit thread.
pub(crate) struct Release {
    /// Staging buffer behind the map.
    staging: ID3D12Resource,
}

/// Mapped staging buffer awaiting its copy.
///
/// Single owner at a time: the submit thread maps, the worker
/// copies, the submit thread releases.
pub(crate) struct Mapped {
    /// Decode slot behind the output.
    slot: u8,
    /// Mapped staging bytes.
    data: *const u8,
    /// Mapped length in bytes.
    len: usize,
    /// Staging layout behind the copy.
    layout: Nv12Layout,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Visible rectangle, if the stream crops.
    crop: Option<Crop>,
    /// Mapping behind the copy.
    release: Release,
    /// Not auto-Send: the mapping outlives the transfer by handoff.
    _send: PhantomData<*const ()>,
}

// Sound: the map outlives the transfer; each hop moves sole
// ownership, unmap runs on the submit thread after the copy lands,
// and only the worker dereferences the pointer.
unsafe impl Send for Mapped {}

/// Readback handle with a copy worker behind one decoder.
pub(crate) struct Readback {
    /// Shared copy pump behind launches.
    pump: CopyPump<Mapped, Done>,
    /// Shard threads behind copies; drops after the pump releases its dispatch.
    #[allow(dead_code)]
    pool: ShardPool,
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let pool = ShardPool::spawn();
        let dispatch = pool.dispatch();
        Self {
            pump: CopyPump::spawn("d3d12-readback", move |mapped: Mapped, started: Instant| {
                let mut out = Vec::new();
                let geom = TightenGeom {
                    layout: &mapped.layout,
                    width: mapped.width,
                    height: mapped.height,
                    crop: mapped.crop,
                };
                // Sound: the map spans `len` bytes by construction.
                let result = unsafe { dispatch.tighten(&mut out, mapped.data, mapped.len, geom) };
                Done {
                    slot: mapped.slot,
                    bytes: result.map(|()| out),
                    release: mapped.release,
                    copy: started.elapsed(),
                }
            }),
            pool,
        }
    }

    /// Queue the copy of `mapped`, oldest-first behind earlier jobs.
    ///
    /// Small copies complete inline, as does every copy without a worker.
    ///
    /// # Arguments
    ///
    /// * `mapped` - mapped source awaiting its copy.
    pub(crate) fn launch(&self, mapped: Mapped) -> Option<Done> {
        let len = mapped.len;
        self.pump.launch(mapped, len, InlineCopy::Allowed)
    }

    /// Oldest landed copy, if any arrived.
    pub(crate) fn harvest(&self) -> Option<Done> {
        self.pump.harvest()
    }

    /// Oldest landed copy, waiting for it.
    pub(crate) fn collect(&self) -> Option<Done> {
        self.pump.collect()
    }

    /// Queued copies drain, the worker joins, orphaned maps release.
    pub(crate) fn shutdown(&mut self) {
        self.pump.shutdown_with(|done| done.release.unmap());
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Release {
    /// Staging buffer behind the map.
    pub(crate) fn new(staging: ID3D12Resource) -> Self {
        Self { staging }
    }

    /// Release the mapping on the calling thread.
    pub(crate) fn unmap(self) {
        super::submit::DeviceResources::unmap_readback(&self.staging);
    }
}

impl Mapped {
    /// Mapped staging bytes plus their release.
    pub(crate) fn new(
        slot: u8,
        data: *const u8,
        len: usize,
        layout: Nv12Layout,
        geometry: (u32, u32, Option<Crop>),
        release: Release,
    ) -> Self {
        let (width, height, crop) = geometry;
        Self {
            slot,
            data,
            len,
            layout,
            width,
            height,
            crop,
            release,
            _send: PhantomData,
        }
    }
}

impl Done {
    /// Slot, copied bytes, mapping release, and copy time.
    pub(crate) fn into_parts(self) -> (u8, Result<Vec<u8>, D3d12Error>, Release, Duration) {
        (self.slot, self.bytes, self.release, self.copy)
    }
}

/// Staging geometry behind one tighten.
#[derive(Clone, Copy)]
pub(crate) struct TightenGeom<'a> {
    /// Staging layout behind the copy.
    pub(crate) layout: &'a Nv12Layout,
    /// Coded width in luma samples.
    pub(crate) width: u32,
    /// Coded height in luma samples.
    pub(crate) height: u32,
    /// Visible rectangle, if the stream crops.
    pub(crate) crop: Option<Crop>,
}

/// Sharded pitched tighten over one decoder's pool threads.
pub(crate) struct ShardDispatch {
    /// One queue per shard thread; empty when a spawn failed.
    tx: Vec<Sender<ShardJob>>,
}

impl ShardDispatch {
    /// Shard threads behind one decoder.
    const SHARDS: usize = 4;

    /// Copies below this size run serially.
    const SHARD_MIN: usize = 256 * 1024;

    /// Pitched `src` bytes tightened into `out` under `geom`, sharded over the pool.
    ///
    /// Small, degraded-pool, or short-staging copies run the serial path,
    /// which reports the bounds failure.
    ///
    /// # Safety
    ///
    /// `src` must span `len` readable bytes of a live mapping.
    pub(crate) unsafe fn tighten(
        &self,
        out: &mut Vec<u8>,
        src: *const u8,
        len: usize,
        geom: TightenGeom<'_>,
    ) -> Result<(), D3d12Error> {
        let layout = geom.layout;
        let tight = Nv12Layout::tight_len(geom.width, geom.height, geom.crop);
        let (left, top, visible_width, visible_height) =
            visible_rect(geom.width, geom.height, geom.crop);
        let copied = u64::from(visible_width)
            .saturating_mul(u64::from(visible_height) + u64::from(visible_height / 2));
        let copied_len = usize::try_from(copied).unwrap_or(tight);
        let planes = [
            PlaneTighten {
                left,
                top,
                width: visible_width,
                rows: visible_height,
                dst_base: 0,
            },
            PlaneTighten {
                left,
                top: top / 2,
                width: visible_width,
                rows: visible_height / 2,
                dst_base: u64::from(visible_width) * u64::from(visible_height),
            },
        ];
        let fits = planes
            .iter()
            .zip(layout.planes.iter())
            .all(|(plane_geom, plane)| plane_geom.fits(plane, len));
        if copied_len < Self::SHARD_MIN || self.tx.len() < Self::SHARDS || !fits {
            // Sound: `src` spans `len` bytes by the caller contract.
            let staged = unsafe { std::slice::from_raw_parts(src, len) };
            return layout
                .tighten(staged, geom.width, geom.height, geom.crop)
                .map(|bytes| {
                    *out = bytes;
                });
        }
        out.clear();
        out.reserve(tight);
        let dst = out.spare_capacity_mut().as_mut_ptr().cast::<u8>();
        let arrived = Arc::new(Barrier::new(Self::SHARDS + 1));
        for (index, tx) in self.tx.iter().enumerate() {
            // Sound: shards partition each plane's rows; the barrier joins before `set_len`.
            let job = ShardJob {
                // Sound: both planes fit by the check above over reserved capacity.
                luma: unsafe { planes[0].span(src, dst, &layout.planes[0], index) },
                // Sound: both planes fit by the check above over reserved capacity.
                chroma: unsafe { planes[1].span(src, dst, &layout.planes[1], index) },
                arrived: Arc::clone(&arrived),
            };
            // Receivers live until pool drop, past every borrow.
            let _ = tx.send(job);
        }
        arrived.wait();
        // Sound: every shard landed inside reserved capacity.
        unsafe {
            out.set_len(copied_len);
        }
        Ok(())
    }

    /// Dispatch clone for the copy closure.
    pub(crate) fn dispatch(&self) -> ShardDispatch {
        ShardDispatch {
            tx: self.tx.clone(),
        }
    }
}

/// One plane's tight geometry behind row partitioning.
struct PlaneTighten {
    /// Visible left edge in samples.
    left: u32,
    /// Visible top edge in rows.
    top: u32,
    /// Visible row width in bytes.
    width: u32,
    /// Visible row count.
    rows: u32,
    /// Tight base offset in bytes.
    dst_base: u64,
}

impl PlaneTighten {
    /// True when the plane's last row ends inside `len` staging bytes.
    fn fits(&self, plane: &crate::submit::Nv12Plane, len: usize) -> bool {
        if self.rows == 0 {
            return true;
        }
        let end = plane
            .offset
            .saturating_add(
                u64::from(plane.pitch)
                    .saturating_mul(u64::from(self.top.saturating_add(self.rows - 1))),
            )
            .saturating_add(u64::from(self.left))
            .saturating_add(u64::from(self.width));
        end <= len as u64
    }

    /// Row span behind shard `index` of four.
    ///
    /// # Safety
    ///
    /// `src` must span the plane's staged rows and `dst` the tight rows;
    /// both hold past a [`fits`](Self::fits) check over reserved capacity.
    unsafe fn span(
        &self,
        src: *const u8,
        dst: *mut u8,
        plane: &crate::submit::Nv12Plane,
        index: usize,
    ) -> ShardSpan {
        let shards = u32::try_from(ShardDispatch::SHARDS).unwrap_or(u32::MAX);
        let at = u32::try_from(index).unwrap_or(u32::MAX);
        let base = self.rows / shards.max(1);
        let first = base.saturating_mul(at);
        let last = if index + 1 == ShardDispatch::SHARDS {
            self.rows
        } else {
            first.saturating_add(base)
        };
        let pitch = u64::from(plane.pitch);
        let src_off = plane
            .offset
            .saturating_add(pitch.saturating_mul(u64::from(self.top.saturating_add(first))))
            .saturating_add(u64::from(self.left));
        let dst_off = self
            .dst_base
            .saturating_add(u64::from(self.width) * u64::from(first));
        // Sound: shards partition `[0, rows)`; spans validated by the caller.
        unsafe {
            ShardSpan {
                src: src.add(usize::try_from(src_off).unwrap_or(0)),
                dst: dst.add(usize::try_from(dst_off).unwrap_or(0)),
                pitch: usize::try_from(pitch).unwrap_or(0),
                width: self.width as usize,
                rows: last.saturating_sub(first) as usize,
            }
        }
    }
}

/// One plane's row span inside a shard job.
struct ShardSpan {
    /// First row source inside the staging map.
    src: *const u8,
    /// First row target inside reserved spare capacity.
    dst: *mut u8,
    /// Staged row stride in bytes.
    pitch: usize,
    /// Tight row width in bytes.
    width: usize,
    /// Row count.
    rows: usize,
}

/// One shard of a pitched tighten.
struct ShardJob {
    /// Luma row span.
    luma: ShardSpan,
    /// Chroma row span.
    chroma: ShardSpan,
    /// Joins the shards plus the caller.
    arrived: Arc<Barrier>,
}

// Sound: shards own disjoint rows; the barrier joins before any reuse.
unsafe impl Send for ShardJob {}

/// Persistent shard threads behind one decoder's copies.
pub(crate) struct ShardPool {
    /// Shard dispatch shared with the copy closure.
    dispatch: ShardDispatch,
    /// Shard threads, joined at drop.
    workers: Vec<JoinHandle<()>>,
}

impl ShardPool {
    /// Shard threads; copies run serially when a spawn fails.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let mut tx = Vec::with_capacity(ShardDispatch::SHARDS);
        let mut workers = Vec::with_capacity(ShardDispatch::SHARDS);
        let mut failed = false;
        for index in 0..ShardDispatch::SHARDS {
            let (job_tx, job_rx) = channel::<ShardJob>();
            let spawned = std::thread::Builder::new()
                .name(format!("d3d12-copy-{index}"))
                .spawn(move || {
                    for job in job_rx {
                        for span in [job.luma, job.chroma] {
                            for row in 0..span.rows {
                                // Sound: spans partition one validated mapping plus reserved capacity.
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        span.src.add(row.saturating_mul(span.pitch)),
                                        span.dst.add(row.saturating_mul(span.width)),
                                        span.width,
                                    );
                                }
                            }
                        }
                        job.arrived.wait();
                    }
                });
            let Ok(worker) = spawned else {
                failed = true;
                break;
            };
            tx.push(job_tx);
            workers.push(worker);
        }
        if failed {
            tx.clear();
        }
        Self {
            dispatch: ShardDispatch { tx },
            workers,
        }
    }

    /// Dispatch clone for the copy closure.
    pub(crate) fn dispatch(&self) -> ShardDispatch {
        self.dispatch.dispatch()
    }
}

impl Drop for ShardPool {
    fn drop(&mut self) {
        self.dispatch.tx.clear();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Returned buffers kept per session.
const POOL_CAP: usize = 16;

/// Pooled frame-byte buffers behind sync readback.
pub(crate) struct BufferPool {
    /// Returned buffers awaiting reuse.
    free: Mutex<Vec<Vec<u8>>>,
}

impl BufferPool {
    /// Empty pool.
    pub(crate) fn new() -> Self {
        Self {
            free: Mutex::new(Vec::new()),
        }
    }

    /// Buffer holding `len`, from the pool or fresh.
    pub(crate) fn take(&self, len: usize) -> Vec<u8> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = free.iter().rposition(|buf| buf.capacity() >= len) {
            return free.swap_remove(index);
        }
        Vec::with_capacity(len)
    }

    /// `buf` as shared bytes returning to `pool` on last drop.
    pub(crate) fn wrap(pool: &Arc<Self>, buf: Vec<u8>) -> Bytes {
        Bytes::from_owner(Pooled::new(buf, Arc::clone(pool)))
    }

    /// `buf` returned for reuse, past the cap dropped.
    fn give(&self, buf: Vec<u8>) {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if free.len() < POOL_CAP {
            free.push(buf);
        }
    }
}

/// Pooled owner behind frame bytes; the buffer returns on last drop.
struct Pooled {
    /// Packed bytes, absent after the return.
    buf: Option<Vec<u8>>,
    /// Pool behind the buffer.
    pool: Arc<BufferPool>,
}

impl Pooled {
    /// Owner of `buf` returning to `pool`.
    fn new(buf: Vec<u8>, pool: Arc<BufferPool>) -> Self {
        Self {
            buf: Some(buf),
            pool,
        }
    }
}

impl AsRef<[u8]> for Pooled {
    fn as_ref(&self) -> &[u8] {
        self.buf.as_ref().map_or(&[], Vec::as_slice)
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            self.pool.give(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ShardPool, TightenGeom};
    use crate::submit::{Nv12Layout, Nv12Plane};

    /// Pitched layout over `width` by `height` with padded pitches.
    fn layout(width: u32, height: u32) -> Nv12Layout {
        let pitch = width.next_multiple_of(256);
        let luma_rows = u64::from(pitch) * u64::from(height);
        let chroma_rows = u64::from(pitch) * u64::from(height / 2);
        Nv12Layout {
            planes: [
                Nv12Plane {
                    offset: 0,
                    pitch,
                    rows: height,
                },
                Nv12Plane {
                    offset: luma_rows,
                    pitch,
                    rows: height / 2,
                },
            ],
            total: luma_rows + chroma_rows,
        }
    }

    /// Staging bytes with a per-row marker under `layout`.
    fn staged(layout: &Nv12Layout, width: u32, height: u32) -> Vec<u8> {
        let mut staged = vec![0xAAu8; usize::try_from(layout.total).unwrap_or(0)];
        for (plane, rows) in layout.planes.iter().zip([height, height / 2]) {
            for row in 0..rows {
                let start = usize::try_from(plane.offset).unwrap_or(0)
                    + row as usize * usize::try_from(plane.pitch).unwrap_or(0);
                let marker = (row & 0xFF) as u8;
                for col in 0..width as usize {
                    staged[start + col] = marker;
                }
            }
        }
        staged
    }

    /// One sharded tighten lands byte-exact over odd heights.
    #[test]
    fn sharded_tighten_matches_serial() {
        let pool = ShardPool::spawn();
        let (width, height) = (640, 365);
        let layout = layout(width, height);
        let staged = staged(&layout, width, height);
        let mut serial = Vec::new();
        layout
            .tighten_into(&staged, width, height, None, &mut serial)
            .expect("serial tighten");
        let mut sharded = Vec::new();
        let geom = TightenGeom {
            layout: &layout,
            width,
            height,
            crop: None,
        };
        unsafe {
            pool.dispatch()
                .tighten(&mut sharded, staged.as_ptr(), staged.len(), geom)
                .expect("sharded tighten");
        }
        assert_eq!(sharded, serial);
    }

    /// Sharded tighten honors the crop window.
    #[test]
    fn sharded_tighten_applies_crop() {
        let pool = ShardPool::spawn();
        let (width, height) = (640, 368);
        let layout = layout(width, height);
        let staged = staged(&layout, width, height);
        let crop = crate::h264::frontend::Crop {
            left: 4,
            right: 4,
            top: 2,
            bottom: 2,
        };
        let mut serial = Vec::new();
        layout
            .tighten_into(&staged, width, height, Some(crop), &mut serial)
            .expect("serial tighten");
        let mut sharded = Vec::new();
        let geom = TightenGeom {
            layout: &layout,
            width,
            height,
            crop: Some(crop),
        };
        unsafe {
            pool.dispatch()
                .tighten(&mut sharded, staged.as_ptr(), staged.len(), geom)
                .expect("sharded tighten");
        }
        assert_eq!(sharded, serial);
    }

    /// Copies below the threshold land byte-exact through the serial path.
    #[test]
    fn small_copy_matches_serial() {
        let pool = ShardPool::spawn();
        let (width, height) = (64, 32);
        let layout = layout(width, height);
        let staged = staged(&layout, width, height);
        let mut serial = Vec::new();
        layout
            .tighten_into(&staged, width, height, None, &mut serial)
            .expect("serial tighten");
        let mut small = Vec::new();
        let geom = TightenGeom {
            layout: &layout,
            width,
            height,
            crop: None,
        };
        unsafe {
            pool.dispatch()
                .tighten(&mut small, staged.as_ptr(), staged.len(), geom)
                .expect("small tighten");
        }
        assert_eq!(small, serial);
    }

    /// Short staging reports the bounds failure instead of sharding.
    #[test]
    fn short_staging_errors() {
        let pool = ShardPool::spawn();
        let (width, height) = (1920, 1080);
        let layout = layout(width, height);
        let staged = vec![0u8; 1024];
        let mut out = Vec::new();
        let geom = TightenGeom {
            layout: &layout,
            width,
            height,
            crop: None,
        };
        let result = unsafe {
            pool.dispatch()
                .tighten(&mut out, staged.as_ptr(), staged.len(), geom)
        };
        assert!(result.is_err());
    }
}
