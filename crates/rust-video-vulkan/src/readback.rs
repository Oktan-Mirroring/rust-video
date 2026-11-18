//! Split-phase readback: the collect thread maps, a worker copies.
//!
//! Map and unmap stay on the collect thread; the worker runs the CPU
//! copy plus nothing else, so copies overlap submits with no lock.
//! A spawn failure degrades to synchronous copies.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ash::vk;
use bytes::BytesMut;

use crate::raw::GpuMemory;

/// Queued copies before a launch waits.
const DEPTH: usize = 8;

/// Copies below this size run inline on the collect thread.
const INLINE_COPY_BYTES: usize = 256 * 1024;

/// One copied transfer.
pub(crate) struct Done {
    /// Filled staging bytes.
    bytes: BytesMut,
    /// Mapping behind the copy, released by the collect thread.
    release: Release,
    /// Copy time.
    copy: Duration,
}

/// Mapping release: unmap on the collect thread.
#[derive(Clone)]
pub(crate) struct Release {
    /// Device behind the map.
    device: ash::Device,
    /// Memory behind the map.
    memory: vk::DeviceMemory,
}

/// Mapped readback buffer awaiting its copy.
///
/// Single owner at a time: the collect thread maps, the worker
/// copies, the collect thread releases.
pub(crate) struct Mapped {
    /// Staging bytes to fill.
    out: BytesMut,
    /// Mapped source bytes.
    data: *const u8,
    /// Copy length in bytes.
    len: usize,
    /// Mapping behind the copy.
    release: Release,
    /// Not auto-Send: the mapping outlives the transfer by handoff.
    _send: PhantomData<*const ()>,
}

// Sound: the map outlives the transfer; each hop moves sole
// ownership, unmap runs on the collect thread after the copy lands,
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
            .name("vulkan-readback".to_owned())
            .spawn(move || {
                for job in rx {
                    let started = Instant::now();
                    let mut out = job.transfer.out;
                    // Sound: the map spans `len` bytes by construction.
                    unsafe {
                        GpuMemory::copy_mapped(&mut out, job.transfer.data, job.transfer.len);
                    }
                    let done = Done {
                        bytes: out,
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
    /// Small copies complete inline when `inline` allows; without a
    /// worker every copy completes inline.
    pub(crate) fn launch(&self, mapped: Mapped, inline: bool) -> Option<Done> {
        let started = Instant::now();
        let copy_inline = |mapped: Mapped| {
            let mut out = mapped.out;
            // Sound: the map spans `len` bytes by construction.
            unsafe {
                GpuMemory::copy_mapped(&mut out, mapped.data, mapped.len);
            }
            Done {
                bytes: out,
                release: mapped.release,
                copy: started.elapsed(),
            }
        };
        if inline && mapped.len < INLINE_COPY_BYTES && self.inflight() == 0 {
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

    /// Oldest landed copy, waiting up to `timeout` for it.
    pub(crate) fn collect_timeout(&self, timeout: Duration) -> Option<Done> {
        let rx = self.rx.as_ref()?;
        let done = rx.recv_timeout(timeout).ok()?;
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
    /// Device plus memory behind the map.
    pub(crate) fn new(device: ash::Device, memory: vk::DeviceMemory) -> Self {
        Self { device, memory }
    }

    /// Release the mapping on the calling thread.
    pub(crate) fn unmap(self) {
        // Sound: the memory mapped on this thread.
        unsafe {
            self.device.unmap_memory(self.memory);
        }
    }
}

impl Mapped {
    /// Mapped staging bytes plus their release.
    pub(crate) fn new(out: BytesMut, data: *const u8, len: usize, release: Release) -> Self {
        Self {
            out,
            data,
            len,
            release,
            _send: PhantomData,
        }
    }
}

impl Done {
    /// Filled bytes, mapping release, and copy time.
    pub(crate) fn into_parts(self) -> (BytesMut, Release, Duration) {
        (self.bytes, self.release, self.copy)
    }
}
