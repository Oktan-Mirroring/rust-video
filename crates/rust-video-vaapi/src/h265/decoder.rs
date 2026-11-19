//! H.265 decode behind the caps `Decoder` trait.

use crate::backend::Backend;
use crate::buffer::bytes_of;
use crate::device::VaApiDevice;
use crate::error::VaApiError;
use crate::ffi::VaSliceParameterBufferHevc;
use crate::h264::decoder::{Emit, VaApiFrame};
use crate::h264::frontend::SurfaceSlot;
use crate::h265::frontend::{Frontend, PendingPicture, Picture};
use crate::h265::params::Params;
use crate::inflight::{PendingQueue, PendingSlot};
use crate::seam::VaSeam;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, SharedDevice};
use rust_video_caps::transcode::{GpuCaps, ShareMechanism};

/// H.265 decoder over a VA-API display.
///
/// Stays on its thread: the borrowed display is `Send` without
/// `Sync`, so the borrow keeps the decoder off other threads.
pub struct H265Decoder<'a> {
    /// Display owning the decode context.
    device: &'a VaApiDevice,
    /// Annex B frontend: pictures ordered against the DPB.
    frontend: Frontend,
    /// Live decode context, created from the first sequence set.
    backend: Option<Backend>,
    /// VRAM budget sized to the surface pool.
    shared: Option<SharedDevice>,
    /// Next frame generation number.
    generation: u64,
    /// Decoded surfaces awaiting sync plus copy.
    pending: PendingQueue,
    /// Scratch slice parameters, reused across pictures.
    slice_params: Vec<VaSliceParameterBufferHevc>,
    /// Scratch list-zero indices, reused across slices.
    list0: Vec<Option<usize>>,
    /// Scratch list-one indices, reused across slices.
    list1: Vec<Option<usize>>,
    /// Scratch temporary list, reused across slices.
    temp: Vec<Option<usize>>,
}

impl<'a> H265Decoder<'a> {
    /// Decoder over `device`; the context starts on the first sequence set.
    #[must_use]
    pub fn open(device: &'a VaApiDevice) -> Self {
        Self {
            device,
            frontend: Frontend::new(),
            backend: None,
            shared: None,
            generation: 0,
            pending: PendingQueue::new(PendingQueue::DEFAULT_DEPTH),
            slice_params: Vec::new(),
            list0: Vec::new(),
            list1: Vec::new(),
            temp: Vec::new(),
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

    /// Live decode context, absent before the first sequence set.
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

    /// Submit gauges of the live context, absent before the first sequence set.
    #[must_use]
    pub fn submit_stats(&self) -> Option<rust_video_caps::delivery::SubmitStats> {
        self.backend.as_ref().map(Backend::stats)
    }

    /// Phase timings of the live context, absent before the first sequence set.
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
            let drained = ingest.drained();
            match (ingest.drained_size(), ingest.drained_crop()) {
                (Some(size), Some(crop)) => {
                    self.emit_all(&mut frames)?;
                    self.read_immediate(drained, &mut frames, size, crop, readback)?;
                }
                _ if drained.is_empty() => {}
                _ => {
                    return Err(VaApiError::Parse(
                        "reconfig dropped decoded pictures".to_owned(),
                    ));
                }
            }
            self.maybe_drop_backend();
        }
        for pending in ingest.pictures() {
            self.decode_picture(pending, &mut frames, readback)?;
        }
        Ok(frames)
    }

    /// Flush held frames, reading back pixels when `readback` holds.
    fn flush_inner(&mut self, readback: bool) -> Result<Vec<VaApiFrame>, VaApiError> {
        let mut frames = Vec::new();
        if let Some(pending) = self.frontend.take_pending() {
            self.decode_picture(pending, &mut frames, readback)?;
        }
        let drained = self.frontend.drain();
        if !drained.is_empty() {
            self.read_slots(drained, &mut frames, readback)?;
        }
        self.emit_all(&mut frames)?;
        Ok(frames)
    }

    /// Decode `pending` into `frames`.
    fn decode_picture(
        &mut self,
        pending: PendingPicture,
        frames: &mut Vec<VaApiFrame>,
        readback: bool,
    ) -> Result<(), VaApiError> {
        self.ensure_backend()?;
        let Some((picture, bumped)) = self.frontend.begin(pending)? else {
            return Ok(());
        };
        self.read_slots(bumped, frames, readback)?;
        let slot = self.acquire_pipelined(frames)?;
        self.submit_picture(&picture, slot)?;
        let outputs = self.frontend.commit(picture, slot)?;
        self.read_slots(outputs, frames, readback)
    }

    /// Live context, creating it from the active sequence set.
    fn ensure_backend(&mut self) -> Result<(), VaApiError> {
        if self.backend.is_some() {
            return Ok(());
        }
        let sps = self
            .frontend
            .sps()
            .cloned()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))?;
        let profile = Params::sps_profile(&sps)?;
        let (width, height) = self
            .frontend
            .coded_size()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))?;
        let count = self.frontend.max_dpb_frames().unwrap_or(16) + 4;
        let backend = Backend::open(self.device, profile, width, height, count)?;
        self.shared = Some(SharedDevice::new("vaapi", u64::MAX));
        self.backend = Some(backend);
        Ok(())
    }

    /// Drop the context unless the new sequence reuses its pool.
    fn maybe_drop_backend(&mut self) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let reuse = self
            .frontend
            .sps()
            .cloned()
            .and_then(|sps| Params::sps_profile(&sps).ok())
            .is_some_and(|profile| {
                profile == backend.profile()
                    && self.frontend.coded_size() == Some(backend.coded_size())
                    && self.frontend.max_dpb_frames().unwrap_or(16) + 4 == backend.pool_size()
            });
        if reuse {
            log::debug!("va-api: reconfig reuses the surface pool");
        } else {
            self.backend = None;
            self.shared = None;
        }
    }

    /// Free pool slot: neither DPB-backed nor leased to a live frame.
    ///
    /// `NoFreeSlot` when live frames pin every DPB-free slot;
    /// `DpbFull` when no slot is DPB-free.
    fn acquire_slot(&self) -> Result<SurfaceSlot, VaApiError> {
        let pool = self.backend.as_ref().map_or(0, Backend::pool_size);
        SurfaceSlot::acquire_free(
            pool,
            None,
            |index| self.frontend.slot_in_use(index),
            |index| {
                self.backend
                    .as_ref()
                    .is_some_and(|backend| backend.surface_live(index))
            },
        )
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
        let surface = self
            .backend
            .as_ref()
            .and_then(|backend| backend.surfaces().get(slot.index() as usize).copied())
            .ok_or_else(|| {
                VaApiError::BrokenReference(format!("slot {} outside pool", slot.index()))
            })?;
        let backend = self
            .backend
            .as_ref()
            .ok_or_else(|| VaApiError::Parse("decode before sequence set".to_owned()))?;
        let params = Params::new(self.frontend.dpb(), backend.surfaces());
        let picture_params = params.picture(
            picture.sps(),
            picture.pps(),
            picture.pic(),
            picture.rps(),
            surface,
        )?;
        let matrix = Params::matrix(picture.sps(), picture.pps());
        self.slice_params.clear();
        let slices = picture.slices();
        for (index, slice) in slices.iter().enumerate() {
            picture.rps().lists_into(
                slice.header(),
                slice.tail(),
                &mut self.list0,
                &mut self.list1,
                &mut self.temp,
            )?;
            self.slice_params.push(params.slice(
                slice,
                &self.list0,
                &self.list1,
                index + 1 == slices.len(),
            )?);
        }
        debug_assert_eq!(self.slice_params.len(), slices.len());
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| VaApiError::Parse("decode before sequence set".to_owned()))?;
        backend.submit_slices(
            surface,
            bytes_of(&picture_params),
            matrix.as_ref().map(bytes_of),
            &self.slice_params,
            |index| picture.slices()[index].data(),
        )
    }

    /// Output frames for `slots` in order.
    fn read_slots(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        readback: bool,
    ) -> Result<(), VaApiError> {
        let crop = self.crop()?;
        let (_, _, width, height) = crop;
        self.read_cropped(slots, frames, (width, height), crop, readback)
    }

    /// Output frames for `slots` under an explicit crop rectangle.
    fn read_cropped(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
        readback: bool,
    ) -> Result<(), VaApiError> {
        if readback && !slots.is_empty() {
            self.stage_emit(slots, size, crop)?;
            return self.emit_ready(frames);
        }
        self.read_immediate(slots, frames, size, crop, readback)
    }

    /// Output frames for `slots` now, syncing before the next submit.
    fn read_immediate(
        &mut self,
        slots: Vec<SurfaceSlot>,
        frames: &mut Vec<VaApiFrame>,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
        readback: bool,
    ) -> Result<(), VaApiError> {
        let (backend, shared) = self.active()?;
        self.generation = VaApiFrame::emit_into(
            self.device,
            backend,
            shared,
            Emit::new(slots, size, crop, readback),
            frames,
            self.generation,
        )?;
        Ok(())
    }

    /// Visible rectangle of the active sequence.
    fn crop(&self) -> Result<(u32, u32, u32, u32), VaApiError> {
        self.frontend
            .visible_rect()
            .ok_or_else(|| VaApiError::Parse("picture before sequence set".to_owned()))
    }

    /// Live context and budget.
    fn active(&self) -> Result<(&Backend, &SharedDevice), VaApiError> {
        match (self.backend.as_ref(), self.shared.as_ref()) {
            (Some(backend), Some(shared)) => Ok((backend, shared)),
            _ => Err(VaApiError::Parse("decode before sequence set".to_owned())),
        }
    }
}

impl Decoder for H265Decoder<'_> {
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
