//! Split-phase NV12 readback: the submit thread maps, a worker copies.
//!
//! Map and unmap stay on the submit thread; the worker runs the CPU
//! copy plus nothing else, so copies overlap submits with no lock.
//! A spawn failure degrades to synchronous copies.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

use crate::error::D3d12Error;
use crate::h264::frontend::Crop;
use crate::submit::Nv12Layout;

/// Queued copies before a launch waits.
const DEPTH: usize = 8;

/// Copies below this size run inline on the submit thread.
const INLINE_COPY_BYTES: usize = 256 * 1024;

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

/// One queued copy.
struct Job {
    /// Mapped staging buffer to copy.
    transfer: Mapped,
}

/// Readback handle with a copy worker behind one decoder.
pub(crate) struct Readback {
    /// Copy jobs, absent after shutdown or a spawn failure.
    tx: Option<SyncSender<Job>>,
    /// Landed copies in send order.
    rx: Option<std::sync::mpsc::Receiver<Done>>,
    /// Worker thread, absent after shutdown or a spawn failure.
    worker: Option<JoinHandle<()>>,
    /// Queued copies not yet received.
    inflight: AtomicUsize,
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let (tx, rx) = sync_channel::<Job>(DEPTH);
        let (done_tx, done_rx) = sync_channel::<Done>(DEPTH);
        let worker = std::thread::Builder::new()
            .name("d3d12-readback".to_owned())
            .spawn(move || {
                for job in rx {
                    let started = Instant::now();
                    // Sound: the map spans `len` bytes by construction.
                    let staged =
                        unsafe { std::slice::from_raw_parts(job.transfer.data, job.transfer.len) };
                    let bytes = job.transfer.layout.tighten(
                        staged,
                        job.transfer.width,
                        job.transfer.height,
                        job.transfer.crop,
                    );
                    let done = Done {
                        slot: job.transfer.slot,
                        bytes,
                        release: job.transfer.release,
                        copy: started.elapsed(),
                    };
                    if done_tx.send(done).is_err() {
                        return;
                    }
                }
            })
            .ok();
        Self {
            tx: worker.is_some().then_some(tx),
            rx: worker.is_some().then_some(done_rx),
            worker,
            inflight: AtomicUsize::new(0),
        }
    }

    /// Queue the copy of `mapped`, oldest-first behind earlier jobs.
    ///
    /// Small copies complete inline, as does every copy without a worker.
    pub(crate) fn launch(&self, mapped: Mapped) -> Option<Done> {
        let started = Instant::now();
        let copy_inline = |mapped: Mapped| {
            // Sound: the map spans `len` bytes by construction.
            let staged = unsafe { std::slice::from_raw_parts(mapped.data, mapped.len) };
            let bytes = mapped
                .layout
                .tighten(staged, mapped.width, mapped.height, mapped.crop);
            Done {
                slot: mapped.slot,
                bytes,
                release: mapped.release,
                copy: started.elapsed(),
            }
        };
        if mapped.len < INLINE_COPY_BYTES && self.inflight() == 0 {
            return Some(copy_inline(mapped));
        }
        let Some(tx) = self.tx.as_ref() else {
            return Some(copy_inline(mapped));
        };
        match tx.send(Job { transfer: mapped }) {
            Ok(()) => {
                self.inflight.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(returned) => Some(copy_inline(returned.0.transfer)),
        }
    }

    /// Queued copies not yet received.
    fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Relaxed)
    }

    /// Oldest landed copy, if any arrived.
    pub(crate) fn harvest(&self) -> Option<Done> {
        let rx = self.rx.as_ref()?;
        let done = rx.try_recv().ok()?;
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }

    /// Oldest landed copy, waiting for it.
    pub(crate) fn collect(&self) -> Option<Done> {
        let rx = self.rx.as_ref()?;
        let done = rx.recv().ok()?;
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }

    /// Queued copies drain, the worker joins, orphaned maps release.
    pub(crate) fn shutdown(&mut self) {
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(rx) = self.rx.take() {
            while let Ok(done) = rx.try_recv() {
                done.release.unmap();
            }
        }
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
