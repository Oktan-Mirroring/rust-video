//! D3D11 maps behind the shared copy pump.

// FFI boundary: the mapped pointer crosses to the worker thread.
#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
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
    /// Shard-copy team behind large packs.
    team: Arc<super::submit::PackTeam>,
}

impl Readback {
    /// Worker over nothing; maps arrive per copy.
    #[must_use]
    pub(crate) fn spawn() -> Self {
        let pool = Arc::new(BufferPool::new());
        let team = Arc::new(super::submit::PackTeam::spawn());
        Self {
            pump: CopyPump::spawn("d3d11-readback", {
                let team = Arc::clone(&team);
                move |mapped: Mapped, started: Instant| {
                    let bytes = BufferPool::pack(&pool, &team, &mapped);
                    Done {
                        bytes,
                        release: mapped.release,
                        copy: started.elapsed(),
                    }
                }
            }),
            team,
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
        self.team.shutdown();
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

/// Returned buffers kept per decoder.
const POOL_CAP: usize = 16;

/// Pooled frame-byte buffers behind worker packs.
struct BufferPool {
    /// Returned buffers awaiting reuse.
    free: Mutex<Vec<Vec<u8>>>,
}

impl BufferPool {
    /// Empty pool.
    fn new() -> Self {
        Self {
            free: Mutex::new(Vec::new()),
        }
    }

    /// `mapped` packed into a reused buffer, empty pool allocates.
    fn pack(
        pool: &Arc<Self>,
        team: &Arc<super::submit::PackTeam>,
        mapped: &Mapped,
    ) -> Result<Bytes, D3d11Error> {
        let mut buf = pool.take(mapped.bytes());
        super::submit::copy_nv12_team(
            &mapped.map,
            mapped.width,
            mapped.height,
            mapped.crop,
            &mut buf,
            team,
        )?;
        Ok(Bytes::from_owner(Pooled::new(buf, Arc::clone(pool))))
    }

    /// Buffer holding `len`, from the pool or fresh.
    fn take(&self, len: usize) -> Vec<u8> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = free.iter().rposition(|buf| buf.capacity() >= len) {
            return free.swap_remove(index);
        }
        Vec::with_capacity(len)
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
