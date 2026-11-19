//! D3D11 maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::time::{Duration, Instant};

use crate::error::D3d11Error;
use crate::h264::Crop;

use bytes::Bytes;
use rust_video_core::{CopyPump, InlineCopy};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, D3D11_MAPPED_SUBRESOURCE,
};

/// One copied transfer.
pub(crate) struct Done {
    /// Copied bytes or the copy failure.
    bytes: Result<Bytes, D3d11Error>,
    /// Mapping behind the copy, released by the submit thread.
    release: Release,
    /// Copy time.
    copy: Duration,
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
            pump: CopyPump::spawn("d3d11-readback", |mapped: Mapped, started: Instant| {
                let bytes =
                    super::submit::copy_nv12(&mapped.map, mapped.width, mapped.height, mapped.crop);
                Done {
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
        let len = mapped.bytes();
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

    /// Readback bytes behind the map.
    fn bytes(&self) -> usize {
        let (mut width, mut height) = (u64::from(self.width), u64::from(self.height));
        if let Some(crop) = self.crop {
            width = width.saturating_sub(u64::from(crop.left) + u64::from(crop.right));
            height = height.saturating_sub(u64::from(crop.top) + u64::from(crop.bottom));
        }
        let bytes = width.saturating_mul(height).saturating_mul(3) / 2;
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }
}

impl Done {
    /// Copied bytes, mapping release, and copy time.
    pub(crate) fn into_parts(self) -> (Result<Bytes, D3d11Error>, Release, Duration) {
        (self.bytes, self.release, self.copy)
    }
}
