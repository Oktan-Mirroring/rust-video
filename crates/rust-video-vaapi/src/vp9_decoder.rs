//! VP9 decode behind the caps `Decoder` trait.

use crate::backend::Backend;
use crate::buffer::bytes_of;
use crate::decoder::{Emit, VaApiFrame};
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::frontend::SurfaceSlot;
use crate::inflight::{PendingQueue, PendingSlot};
use crate::seam::VaSeam;
use crate::vp9_frontend::{Begun, Frontend, PendingFrame, Picture, NUM_SLOTS};
use crate::vp9_params::Params;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, SharedDevice};
use rust_video_caps::transcode::{GpuCaps, ShareMechanism};

/// Surface pool size: eight reference slots plus decode margin.
const POOL_SIZE: usize = NUM_SLOTS + 4;

/// VP9 decoder over a VA-API display.
///
/// Stays on its thread: the borrowed display is `Send` without
/// `Sync`, so the borrow keeps the decoder off other threads.
pub struct Vp9Decoder<'a> {
    /// Display owning the decode context.
    device: &'a VaApiDevice,
    /// Chunk frontend: frames ordered against the reference slots.
    frontend: Frontend,
    /// Live decode context, created from the first frame.
    backend: Option<Backend>,
    /// VRAM budget sized to the surface pool.
    shared: Option<SharedDevice>,
    /// Next frame generation number.
    generation: u64,
    /// Decoded surfaces awaiting sync plus copy.
    pending: PendingQueue,
}

impl<'a> Vp9Decoder<'a> {
    /// Decoder over `device`; the context starts on the first frame.
    #[must_use]
    pub fn open(device: &'a VaApiDevice) -> Self {
        Self {
            device,
            frontend: Frontend::new(),
            backend: None,
            shared: None,
            generation: 0,
            pending: PendingQueue::new(PendingQueue::DEFAULT_DEPTH),
        }
    }

    /// Frames decoded from `packet` with device surfaces attached.
    ///
    /// The frames skip the NV12 readback: `nv12` is empty and `to_cpu`
    /// is absent. Each frame pins its pool slot until dropped, so a
    /// caller holding more frames than the pool holds back fails with
    /// `NoFreeSlot` when live frames pin every free slot, or `DpbFull`
    /// when the picture buffer itself is full; `decode` copies pixels
    /// and releases immediately.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.decode_inner(packet, false)
    }

    /// Device owning the decode context.
    #[must_use]
    pub fn device(&self) -> &VaApiDevice {
        self.device
    }

    /// Live decode context, absent before the first frame.
    ///
    /// The borrow ties the context to the decoder; see
    /// [`Backend`](crate::Backend) for the native handles behind it.
    #[must_use]
    pub fn context(&self) -> Option<&Backend> {
        self.backend.as_ref()
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.flush_inner(false)
    }

    /// Submit gauges of the live context, absent before the first frame.
    #[must_use]
    pub fn submit_stats(&self) -> Option<rust_video_caps::delivery::SubmitStats> {
        self.backend.as_ref().map(Backend::stats)
    }

    /// Phase timings of the live context, absent before the first frame.
    #[must_use]
    pub fn phase_stats(&self) -> Option<crate::backend::PhaseStats> {
        self.backend.as_ref().map(Backend::phase_stats)
    }

    /// Submitted-ahead ceiling, 1..=4.
    #[must_use]
    pub fn pipeline_depth(&self) -> usize {
        self.pending.depth()
    }

    /// Submitted-ahead ceiling, clamped to 1..=4.
    ///
    /// # Arguments
    ///
    /// * `depth` - submits ahead of a sync; one syncs every picture first.
    pub fn set_pipeline_depth(&mut self, depth: usize) {
        self.pending.set_depth(depth);
    }

    /// Decode `packet`, reading back pixels when `readback` holds.
    fn decode_inner(
        &mut self,
        packet: &Packet,
        readback: bool,
    ) -> Result<Vec<VaApiFrame>, VaApiError> {
        let owned = packet.bytes();
        let mut ingest = self.frontend.ingest(&owned)?;
        let mut frames = Vec::new();
        if ingest.reconfigured() {
            self.emit_all(&mut frames)?;
            self.maybe_drop_backend();
        }
        for pending in ingest.frames() {
            self.decode_frame(pending, &mut frames, readback)?;
        }
        Ok(frames)
    }

    /// Flush held frames, reading back pixels when `readback` holds.
    fn flush_inner(&mut self, readback: bool) -> Result<Vec<VaApiFrame>, VaApiError> {
        let mut frames = Vec::new();
        self.emit_all(&mut frames)?;
        let _ = readback;
        Ok(frames)
    }

    /// Decode `pending` into `frames`.
    fn decode_frame(
        &mut self,
        pending: PendingFrame,
        frames: &mut Vec<VaApiFrame>,
        readback: bool,
    ) -> Result<(), VaApiError> {
        self.ensure_backend()?;
        match self.frontend.begin(pending)? {
            Begun::Existing { slot, size } => {
                self.read_slots(vec![slot], frames, size, readback)?;
            }
            Begun::Picture(picture) => {
                let picture = picture.as_ref();
                let slot = self.acquire_pipelined(frames)?;
                self.submit_picture(picture, slot)?;
                let size = (picture.header().frame_width, picture.header().frame_height);
                let shown = picture.header().show_frame();
                self.frontend.commit(picture, slot);
                if shown {
                    self.read_slots(vec![slot], frames, size, readback)?;
                }
            }
        }
        Ok(())
    }

    /// Live context, creating it from the active frame size.
    fn ensure_backend(&mut self) -> Result<(), VaApiError> {
        if self.backend.is_some() {
            return Ok(());
        }
        let profile = self
            .frontend
            .profile()
            .ok_or_else(|| VaApiError::Parse("frame before first header".to_owned()))?;
        let profile = Params::profile(profile)?;
        let (width, height) = self
            .frontend
            .coded_size()
            .ok_or_else(|| VaApiError::Parse("frame before first header".to_owned()))?;
        let backend = Backend::open(self.device, profile, width, height, POOL_SIZE)?;
        self.shared = Some(SharedDevice::new("vaapi", u64::MAX));
        self.backend = Some(backend);
        Ok(())
    }

    /// Drop the context unless the new size reuses its pool.
    fn maybe_drop_backend(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let reuse = self
            .frontend
            .profile()
            .and_then(|profile| Params::profile(profile).ok())
            .is_some_and(|profile| {
                profile == backend.profile()
                    && self.frontend.coded_size() == Some(backend.coded_size())
                    && POOL_SIZE == backend.pool_size()
            });
        if reuse {
            log::debug!("va-api: reconfig reuses the surface pool");
        } else {
            self.backend = None;
            self.shared = None;
        }
    }

    /// Free pool slot: neither reference-backed nor leased to a live frame.
    ///
    /// `NoFreeSlot` when live frames pin every reference-free slot;
    /// `DpbFull` when no slot is reference-free.
    fn acquire_slot(&self) -> Result<SurfaceSlot, VaApiError> {
        let pool = self.backend.as_ref().map_or(0, Backend::pool_size);
        let end = u32::try_from(pool).map_err(|_| VaApiError::NoFreeSlot)?;
        let mut pinned = false;
        for index in 0..end {
            let slot = SurfaceSlot::new(index);
            if self.frontend.slot_in_use(slot.index()) {
                continue;
            }
            if self
                .backend
                .as_ref()
                .is_some_and(|backend| backend.surface_live(slot.index()))
            {
                pinned = true;
                continue;
            }
            return Ok(slot);
        }
        Err(if pinned {
            VaApiError::NoFreeSlot
        } else {
            VaApiError::DpbFull
        })
    }

    /// Free pool slot, emitting staged reads when the pool is pinned.
    fn acquire_pipelined(
        &mut self,
        frames: &mut Vec<VaApiFrame>,
    ) -> Result<SurfaceSlot, VaApiError> {
        loop {
            match self.acquire_slot() {
                Ok(slot) => return Ok(slot),
                Err(error) if self.pending.is_empty() => return Err(error),
                Err(_) => self.emit_oldest(frames)?,
            }
        }
    }

    /// Stage `slots` behind the holdback, reserving generations.
    fn stage_emit(
        &mut self,
        slots: Vec<SurfaceSlot>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
    ) -> Result<(), VaApiError> {
        let staged = {
            let (backend, _) = self.active()?;
            slots
                .into_iter()
                .map(|slot| {
                    let guard = backend.surface_guard(slot.index()).ok_or_else(|| {
                        VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
                    })?;
                    let surface = backend
                        .surfaces()
                        .get(slot.index() as usize)
                        .copied()
                        .ok_or_else(|| {
                            VaApiError::BrokenReference(format!(
                                "slot {} outside pool",
                                slot.index()
                            ))
                        })?;
                    Ok((slot, guard, surface))
                })
                .collect::<Result<Vec<_>, VaApiError>>()?
        };
        let mut launched = Vec::with_capacity(staged.len());
        for (slot, guard, surface) in staged {
            let generation = self.generation;
            self.generation = self.generation.wrapping_add(1);
            launched.push((generation, surface));
            self.pending
                .push(PendingSlot::new(slot, size, crop, generation, guard));
        }
        let (backend, _) = self.active()?;
        for (generation, surface) in launched {
            backend.launch_read(generation, surface, crop);
        }
        backend.map_ready(self.pending.depth());
        Ok(())
    }

    /// Frames for staged slots past the holdback.
    fn emit_ready(&mut self, frames: &mut Vec<VaApiFrame>) -> Result<(), VaApiError> {
        while let Some(staged) = self.pending.pop_ready() {
            self.emit_one(&staged, frames)?;
        }
        Ok(())
    }

    /// Frames for every staged slot, oldest first.
    fn emit_all(&mut self, frames: &mut Vec<VaApiFrame>) -> Result<(), VaApiError> {
        while let Some(staged) = self.pending.pop_oldest() {
            self.emit_one(&staged, frames)?;
        }
        Ok(())
    }

    /// Frame for the oldest staged slot, if any.
    fn emit_oldest(&mut self, frames: &mut Vec<VaApiFrame>) -> Result<(), VaApiError> {
        if let Some(staged) = self.pending.pop_oldest() {
            self.emit_one(&staged, frames)?;
        }
        Ok(())
    }

    /// Frame for one staged slot.
    fn emit_one(
        &mut self,
        staged: &PendingSlot,
        frames: &mut Vec<VaApiFrame>,
    ) -> Result<(), VaApiError> {
        let (backend, shared) = self.active()?;
        let bytes = backend.collect_read(staged.generation())?;
        VaApiFrame::emit_collected(
            backend,
            shared,
            staged.size(),
            staged.crop(),
            bytes,
            frames,
            staged.generation(),
        )?;
        Ok(())
    }

    /// Submit `picture` on `slot`.
    fn submit_picture(&mut self, picture: &Picture, slot: SurfaceSlot) -> Result<(), VaApiError> {
        let backend = self
            .backend
            .as_ref()
            .ok_or_else(|| VaApiError::Parse("decode before first header".to_owned()))?;
        let surface = backend
            .surfaces()
            .get(slot.index() as usize)
            .copied()
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        let params = Params::new(backend.surfaces());
        let picture_params = params.picture(picture.header(), picture.refs())?;
        let slice_params = Params::slice(picture.header(), picture.data().len())?;
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| VaApiError::Parse("decode before first header".to_owned()))?;
        backend.submit_slices(
            surface,
            bytes_of(&picture_params),
            None,
            std::slice::from_ref(&slice_params),
            |_| picture.data(),
        )
    }

    /// Output frames for `slots` at `size` in order.
    fn read_slots(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        size: (u32, u32),
        readback: bool,
    ) -> Result<(), VaApiError> {
        if readback && !slots.is_empty() {
            self.stage_emit(slots, size, (0, 0, size.0, size.1))?;
            return self.emit_ready(frames);
        }
        let (backend, shared) = self.active()?;
        self.generation = VaApiFrame::emit_into(
            self.device,
            backend,
            shared,
            Emit::new(slots, size, (0, 0, size.0, size.1), readback),
            frames,
            self.generation,
        )?;
        Ok(())
    }

    /// Live context and budget.
    fn active(&self) -> Result<(&Backend, &SharedDevice), VaApiError> {
        match (self.backend.as_ref(), self.shared.as_ref()) {
            (Some(backend), Some(shared)) => Ok((backend, shared)),
            _ => Err(VaApiError::Parse("decode before first header".to_owned())),
        }
    }
}

impl Decoder for Vp9Decoder<'_> {
    type Frame = VaApiFrame;
    type Error = VaApiError;

    fn caps(&self) -> GpuCaps {
        let share = self
            .device
            .dmabuf_export()
            .then_some(ShareMechanism::DmaBuf);
        GpuCaps::new(share, false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.decode_inner(packet, true)
    }

    fn flush(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        self.flush_inner(true)
    }
}
