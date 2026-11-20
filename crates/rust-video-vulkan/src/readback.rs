//! Vulkan maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::raw::GpuMemory;

use bytes::BytesMut;
use rust_video_core::{CopyPump, InlineCopy};

/// One copied transfer.
pub(crate) struct Done {
    /// Filled staging bytes.
    bytes: BytesMut,
    /// Copy time.
    copy: Duration,
}

/// Mapped readback buffer awaiting its copy.
///
/// Single owner at a time: the launch hands the session-life map
/// to the worker, which copies; the collect then lands the bytes.
pub(crate) struct Mapped {
    /// Staging bytes to fill.
    out: BytesMut,
    /// Mapped source bytes.
    data: *const u8,
    /// Copy length in bytes.
    len: usize,
    /// Not auto-Send: the mapping outlives the transfer by handoff.
    _send: PhantomData<*const ()>,
}

// Sound: the session-life map outlives the transfer; each hop
// moves sole ownership, and only the worker dereferences the pointer.
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
            pump: CopyPump::spawn(
                "vulkan-readback",
                move |mapped: Mapped, started: Instant| {
                    let mut out = mapped.out;
                    // Sound: the map spans `len` bytes by construction.
                    unsafe {
                        dispatch.copy(&mut out, mapped.data, mapped.len);
                    }
                    Done {
                        bytes: out,
                        copy: started.elapsed(),
                    }
                },
            ),
            pool,
        }
    }

    /// Queue the copy of `mapped`, oldest-first behind earlier jobs.
    ///
    /// Small copies complete inline when `inline` allows; without a
    /// worker every copy completes inline.
    ///
    /// # Arguments
    ///
    /// * `mapped` - mapped source awaiting its copy.
    /// * `inline` - small copies complete inline when true.
    pub(crate) fn launch(&self, mapped: Mapped, inline: bool) -> Option<Done> {
        let len = mapped.len;
        self.pump.launch(mapped, len, InlineCopy::from(inline))
    }

    /// Oldest landed copy, if any arrived.
    pub(crate) fn harvest(&self) -> Option<Done> {
        self.pump.harvest()
    }

    /// Oldest landed copy, waiting for it.
    pub(crate) fn collect(&self) -> Option<Done> {
        self.pump.collect()
    }

    /// Oldest landed copy, waiting up to `timeout` for it.
    pub(crate) fn collect_timeout(&self, timeout: Duration) -> Option<Done> {
        self.pump.collect_timeout(timeout)
    }

    /// Queued copies drain and the worker joins.
    pub(crate) fn shutdown(&mut self) {
        self.pump.shutdown_with(|_done| {});
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Mapped {
    /// Mapped staging bytes over the session-life map.
    pub(crate) fn new(out: BytesMut, data: *const u8, len: usize) -> Self {
        Self {
            out,
            data,
            len,
            _send: PhantomData,
        }
    }
}

impl Done {
    /// Filled bytes and copy time.
    pub(crate) fn into_parts(self) -> (BytesMut, Duration) {
        (self.bytes, self.copy)
    }
}

/// Sharded host copy over one decoder's pool threads.
pub(crate) struct ShardDispatch {
    /// One queue per shard thread; empty when a spawn failed.
    tx: Vec<Sender<ShardJob>>,
}

impl ShardDispatch {
    /// Shard threads behind one decoder.
    const SHARDS: usize = 4;

    /// Copies below this size run serially.
    const SHARD_MIN: usize = 256 * 1024;

    /// `len` mapped bytes into `out`, sharded over the pool.
    ///
    /// # Safety
    ///
    /// `src` must span `len` readable bytes of a live mapping.
    pub(crate) unsafe fn copy(&self, out: &mut BytesMut, src: *const u8, len: usize) {
        if len < Self::SHARD_MIN || self.tx.len() < Self::SHARDS {
            // Sound: `src` spans `len` bytes by the caller contract.
            unsafe {
                GpuMemory::copy_mapped(out, src, len);
            }
            return;
        }
        out.clear();
        out.reserve(len);
        let dst = out.spare_capacity_mut().as_mut_ptr().cast::<u8>();
        let arrived = Arc::new(Barrier::new(Self::SHARDS + 1));
        let base = len / Self::SHARDS;
        for (index, tx) in self.tx.iter().enumerate() {
            let offset = index * base;
            let take = if index + 1 == Self::SHARDS {
                len - offset
            } else {
                base
            };
            // Sound: shards partition `[0, len)`; the barrier joins before `set_len`.
            let job = ShardJob {
                src: unsafe { src.add(offset) },
                dst: unsafe { dst.add(offset) },
                len: take,
                arrived: Arc::clone(&arrived),
            };
            // Receivers live until pool drop, past every borrow.
            let _ = tx.send(job);
        }
        arrived.wait();
        // Sound: every shard landed inside reserved capacity.
        unsafe {
            out.set_len(len);
        }
    }
}

/// One shard of a host copy.
struct ShardJob {
    /// Shard source inside the mapping.
    src: *const u8,
    /// Shard target inside reserved spare capacity.
    dst: *mut u8,
    /// Shard length in bytes.
    len: usize,
    /// Joins the shards plus the caller.
    arrived: Arc<Barrier>,
}

// Sound: shards own disjoint slices; the barrier joins before any reuse.
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
                .name(format!("vulkan-copy-{index}"))
                .spawn(move || {
                    for job in job_rx {
                        // Sound: shards partition one live mapping plus reserved capacity.
                        unsafe {
                            std::ptr::copy_nonoverlapping(job.src, job.dst, job.len);
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
        ShardDispatch {
            tx: self.dispatch.tx.clone(),
        }
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

#[cfg(test)]
mod tests {
    use super::ShardPool;

    use bytes::BytesMut;

    /// One sharded copy lands byte-exact.
    #[test]
    fn sharded_copy_matches_serial() {
        let pool = ShardPool::spawn();
        let src: Vec<u8> = (0..=255u8).cycle().take(3_110_400).collect();
        let mut out = BytesMut::new();
        unsafe {
            pool.dispatch().copy(&mut out, src.as_ptr(), src.len());
        }
        assert_eq!(out.as_ref(), src.as_slice());
    }

    /// Copies below the threshold land byte-exact.
    #[test]
    fn small_copy_matches_serial() {
        let pool = ShardPool::spawn();
        let src: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
        let mut out = BytesMut::new();
        unsafe {
            pool.dispatch().copy(&mut out, src.as_ptr(), src.len());
        }
        assert_eq!(out.as_ref(), src.as_slice());
    }

    /// Empty copies write nothing.
    #[test]
    fn empty_copy_writes_nothing() {
        let pool = ShardPool::spawn();
        let src: Vec<u8> = Vec::new();
        let mut out = BytesMut::new();
        unsafe {
            pool.dispatch().copy(&mut out, src.as_ptr(), 0);
        }
        assert!(out.is_empty());
    }
}
