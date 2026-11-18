//! Split-phase NV12 readback: the submit thread maps, a worker copies.
//!
//! Map and unmap stay on the submit thread; the worker runs the CPU
//! copy plus nothing else, so copies overlap submits with no lock.
//! A spawn failure degrades to synchronous copies.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;
use std::time::Instant;

use bytes::Bytes;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, D3D11_MAPPED_SUBRESOURCE,
};

use crate::error::D3d11Error;
use crate::h264::Crop;

/// Queued copies before a launch waits.
const DEPTH: usize = 8;

/// One copied transfer.
pub(crate) struct Done {
    /// Copied bytes or the copy failure.
    bytes: Result<Bytes, D3d11Error>,
    /// Mapping behind the copy, released by the submit thread.
    release: Release,
    /// Copy microseconds.
    copy_us: u64,
}

/// Mapping release: unmap on the submit thread.
pub(crate) struct Release {
    /// Immediate context behind the map.
    context: ID3D11DeviceContext,
    /// Staging texture behind the map.
    staging: ID3D11Texture2D,
}

/// Mapped staging texture awaiting its copy.
///
/// Single owner at a time: the submit thread maps, the worker
/// copies, the submit thread releases.
pub(crate) struct Mapped {
    /// Mapped staging bytes.
    map: D3D11_MAPPED_SUBRESOURCE,
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
    /// Mapped staging texture to copy.
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
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let (tx, rx) = sync_channel::<Job>(DEPTH);
        let (done_tx, done_rx) = sync_channel::<Done>(DEPTH);
        let worker = std::thread::Builder::new()
            .name("d3d11-readback".to_owned())
            .spawn(move || {
                for job in rx {
                    let started = Instant::now();
                    let bytes = super::submit::copy_nv12(
                        &job.transfer.map,
                        job.transfer.width,
                        job.transfer.height,
                        job.transfer.crop,
                    );
                    let done = Done {
                        bytes,
                        release: job.transfer.release,
                        copy_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
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
        }
    }

    /// Queue the copy of `mapped`, oldest-first behind earlier jobs.
    ///
    /// Returns the completed copy inline without a worker.
    pub(crate) fn launch(&self, mapped: Mapped) -> Option<Done> {
        let started = Instant::now();
        let copy_inline = |mapped: Mapped| {
            let bytes =
                super::submit::copy_nv12(&mapped.map, mapped.width, mapped.height, mapped.crop);
            Done {
                bytes,
                release: mapped.release,
                copy_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            }
        };
        let Some(tx) = self.tx.as_ref() else {
            return Some(copy_inline(mapped));
        };
        match tx.send(Job { transfer: mapped }) {
            Ok(()) => None,
            Err(returned) => Some(copy_inline(returned.0.transfer)),
        }
    }

    /// Oldest landed copy, if any arrived.
    pub(crate) fn harvest(&self) -> Option<Done> {
        let rx = self.rx.as_ref()?;
        rx.try_recv().ok()
    }

    /// Oldest landed copy, waiting for it.
    pub(crate) fn collect(&self) -> Option<Done> {
        let rx = self.rx.as_ref()?;
        rx.recv().ok()
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
    /// Mapped texture plus the context that mapped it.
    pub(crate) fn new(context: ID3D11DeviceContext, staging: ID3D11Texture2D) -> Self {
        Self { context, staging }
    }

    /// Release the mapping on the calling thread.
    pub(crate) fn unmap(self) {
        // Sound: the staging texture mapped on this thread.
        unsafe {
            self.context.Unmap(&self.staging, 0);
        }
    }
}

impl Mapped {
    /// Mapped staging bytes plus their release.
    pub(crate) fn new(
        map: D3D11_MAPPED_SUBRESOURCE,
        width: u32,
        height: u32,
        crop: Option<Crop>,
        release: Release,
    ) -> Self {
        Self {
            map,
            width,
            height,
            crop,
            release,
            _send: PhantomData,
        }
    }
}

impl Done {
    /// Copied bytes, mapping release, and copy microseconds.
    pub(crate) fn into_parts(self) -> (Result<Bytes, D3d11Error>, Release, u64) {
        (self.bytes, self.release, self.copy_us)
    }
}
