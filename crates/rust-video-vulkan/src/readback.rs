//! Vulkan maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::time::{Duration, Instant};

use crate::raw::GpuMemory;

use ash::vk;
use bytes::BytesMut;
use rust_video_core::{CopyPump, InlineCopy};

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

/// Readback handle with a copy worker behind one decoder.
pub(crate) struct Readback {
    /// Shared copy pump behind launches.
    pump: CopyPump<Mapped, Done>,
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        Self {
            pump: CopyPump::spawn("vulkan-readback", |mapped: Mapped, started: Instant| {
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
            }),
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
