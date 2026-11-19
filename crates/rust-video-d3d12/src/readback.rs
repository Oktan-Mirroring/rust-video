//! D3D12 maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::time::{Duration, Instant};

use crate::error::D3d12Error;
use crate::h264::frontend::Crop;
use crate::submit::Nv12Layout;

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
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        Self {
            pump: CopyPump::spawn("d3d12-readback", |mapped: Mapped, started: Instant| {
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
            }),
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
