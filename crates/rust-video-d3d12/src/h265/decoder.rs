//! H.265 decode over a live video session.

use std::sync::Arc;

use super::dpb::{HevcDecoded, HevcDpb};
use super::frontend::{Ingest as HevcIngest, Picture as HevcPicture};
use crate::decode::{DecodeQueues, DecodeSession};
use crate::decoder::D3d12Frame;
use crate::decoder_os::{emit_frame, session_surface_export, surface_layout, Active};
use crate::device::D3d12Device;
use crate::error::{D3d12Error, ParseReason, SkipReason};
use crate::guid::Codec;
use crate::h264::frontend::Crop;
use crate::pins::SlotPins;
use crate::submit::Ticket;
use crate::texture::GpuSurface;

use rust_video_caps::common::PictureSize;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, SharedDevice};
use rust_video_caps::transcode::GpuCaps;
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

/// HEVC decoder over a D3D12 device.
pub struct HevcDecoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d12Device,
    /// Annex B ingest: sets plus picture assembly.
    ingest: HevcIngest,
    /// Live picture buffer, created from the first sequence set.
    dpb: Option<HevcDpb>,
    /// Live decode session, created from the first sequence set.
    session: Option<DecodeSession>,
    /// VRAM budget sized to the adapter memory.
    shared: SharedDevice,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-picture failures.
    resets: u64,
    /// Peak outputs queued before readback in one call.
    inflight_max: usize,
    /// Live-surface pin counts over the decoder pool.
    pins: Arc<SlotPins>,
}

impl<'a> HevcDecoder<'a> {
    /// Decoder over `device`; the session starts on the first picture.
    #[must_use]
    pub fn open(device: &'a D3d12Device) -> Self {
        Self {
            device,
            ingest: HevcIngest::new(),
            dpb: None,
            session: None,
            shared: device.shared().clone(),
            active: None,
            generation: 0,
            resets: 0,
            inflight_max: 0,
            pins: Arc::new(SlotPins::new()),
        }
    }

    /// Video decode queue behind submissions, absent before the first picture builds the session.
    ///
    /// Shares the session queue; threading follows [`D3d12Device`].
    #[must_use]
    pub fn video_queue(&self) -> Option<&ID3D12CommandQueue> {
        self.session.as_ref().map(DecodeSession::video_queue)
    }

    /// Direct queue behind downloads, absent before the first picture builds the session.
    ///
    /// Shares the session queue; threading follows [`D3d12Device`].
    #[must_use]
    pub fn copy_queue(&self) -> Option<&ID3D12CommandQueue> {
        self.session.as_ref().map(DecodeSession::copy_queue)
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs queued before readback in one call.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.inflight_max
    }

    /// Decode `picture`, queueing its outputs for readback into `pending`.
    fn submit_picture(
        &mut self,
        picture: &HevcPicture,
        pending: &mut Vec<(u8, Ticket, Active)>,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.consume_pending(pending, frames)?;
            pending.clear();
            self.reconfigure(active, frames)?;
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                if !matches!(error, D3d12Error::NoFreeSlot) {
                    self.reset_stream();
                }
                return Err(error);
            }
        };
        let mut failed = None;
        {
            let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
            // Submit before any copy: outputs may name the submission slot.
            match session.submit_hevc(&decoded.submission) {
                Ok(ticket) => {
                    if let Err(error) = session.wait_video(ticket) {
                        failed = Some(error);
                    } else {
                        for output in &decoded.outputs {
                            match session.start_download(output.slot()) {
                                Ok(ticket) => pending.push((output.slot(), ticket, active)),
                                Err(error) => {
                                    failed = Some(error);
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(error) => failed = Some(error),
            }
        }
        if let Some(error) = failed {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// Readback for queued `pending` outputs into `frames`.
    fn consume_pending(
        &mut self,
        pending: &[(u8, Ticket, Active)],
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        for (slot, ticket, active) in pending {
            let nv12 = session.finish_download(*slot, *ticket)?;
            let export = session.shared_handle(*slot);
            let frame = emit_frame(&self.shared, nv12, export, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, buffer, and sequence after a mid-picture failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.active = None;
        self.resets = self.resets.wrapping_add(1);
    }

    /// Peak outputs queued before readback against `depth`.
    fn note_inflight(&mut self, depth: usize) {
        self.inflight_max = self.inflight_max.max(depth);
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(
        &mut self,
        active: Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        if let Some(previous) = self.active {
            self.drain_live(&previous, frames)?;
        }
        self.open_session(active)
    }

    /// Session plus picture buffer for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d12Error> {
        let queues = self.session.take().map(DecodeSession::into_queues);
        let pins = Arc::new(SlotPins::new());
        let (session, dpb) =
            open_sequence_hevc(self.device, &self.ingest, Arc::clone(&pins), queues, active)?;
        self.session = Some(session);
        self.pins = pins;
        self.dpb = Some(dpb);
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `picture`'s parameter sets.
    fn sequence(&self, picture: &HevcPicture) -> Result<Active, D3d12Error> {
        sequence_of_hevc(&self.ingest, picture)
    }

    /// Submission plus display-order outputs for `picture`.
    fn decode_dpb(&mut self, picture: &HevcPicture) -> Result<HevcDecoded, D3d12Error> {
        let dpb = self.dpb.as_mut().ok_or(D3d12Error::NoSession)?;
        submit_hevc_dpb(&self.ingest, dpb, picture)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        let outputs = dpb.drain();
        let mut pending = Vec::with_capacity(outputs.len());
        for output in &outputs {
            pending.push((output.slot(), session.start_download(output.slot())?));
        }
        let depth = pending.len();
        for (slot, ticket) in pending {
            let nv12 = session.finish_download(slot, ticket)?;
            let export = session.shared_handle(slot);
            let frame = emit_frame(&self.shared, nv12, export, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        self.note_inflight(depth);
        Ok(())
    }

    /// Decode `picture` into GPU `surfaces`.
    fn texture_picture(
        &mut self,
        picture: &HevcPicture,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let outcome = self.texture_inner(picture, surfaces);
        if let Err(error) = &outcome {
            if !matches!(error, D3d12Error::NoFreeSlot) {
                self.reset_stream();
            }
        }
        outcome
    }

    /// Decode `picture` into GPU `surfaces` without resetting on failure.
    fn texture_inner(
        &mut self,
        picture: &HevcPicture,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.reconfigure_texture(active, surfaces)?;
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                if !matches!(error, D3d12Error::NoFreeSlot) {
                    self.reset_stream();
                }
                return Err(error);
            }
        };
        if let Err(error) = self.submit_textures(&decoded, &active, surfaces) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `decoded` submission plus display-order outputs into GPU `surfaces`.
    fn submit_textures(
        &mut self,
        decoded: &HevcDecoded,
        active: &Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any surface handoff: outputs may name the submission slot.
        let ticket = session.submit_hevc(&decoded.submission)?;
        session.wait_video(ticket)?;
        if !session.cropped() {
            for output in &decoded.outputs {
                let slot = output.slot();
                let resource = session.texture(slot)?;
                let shared = session_surface_export(session, slot);
                let layout = surface_layout(active, slot)?;
                let pin = self.pins.pin(slot);
                let wait = session.video_wait(ticket);
                surfaces.push(GpuSurface::new(
                    resource,
                    layout,
                    self.generation,
                    Some(pin),
                    Some(wait),
                    shared,
                ));
                self.generation = self.generation.wrapping_add(1);
            }
            return Ok(());
        }
        let mut pending = Vec::with_capacity(decoded.outputs.len());
        for output in &decoded.outputs {
            pending.push((output.slot(), session.start_crop(output.slot())?));
        }
        for (slot, ticket) in pending {
            let resource = session.finish_crop(slot, ticket)?;
            let shared = session_surface_export(session, slot);
            let layout = surface_layout(active, slot)?;
            let pin = self.pins.pin(slot);
            let wait = session.copy_wait(ticket);
            surfaces.push(GpuSurface::new(
                resource,
                layout,
                self.generation,
                Some(pin),
                Some(wait),
                shared,
            ));
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure_texture(
        &mut self,
        active: Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        if let Some(previous) = self.active {
            self.drain_live_texture(&previous, surfaces)?;
        }
        self.open_session(active)
    }

    /// Live DPB outputs into GPU `surfaces`.
    fn drain_live_texture(
        &mut self,
        active: &Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        let outputs = dpb.drain();
        if !session.cropped() {
            for output in &outputs {
                let slot = output.slot();
                let resource = session.texture(slot)?;
                let shared = session_surface_export(session, slot);
                let layout = surface_layout(active, slot)?;
                let pin = self.pins.pin(slot);
                surfaces.push(GpuSurface::new(
                    resource,
                    layout,
                    self.generation,
                    Some(pin),
                    None,
                    shared,
                ));
                self.generation = self.generation.wrapping_add(1);
            }
            return Ok(());
        }
        let mut pending = Vec::with_capacity(outputs.len());
        for output in &outputs {
            pending.push((output.slot(), session.start_crop(output.slot())?));
        }
        for (slot, ticket) in pending {
            let resource = session.finish_crop(slot, ticket)?;
            let shared = session_surface_export(session, slot);
            let layout = surface_layout(active, slot)?;
            let pin = self.pins.pin(slot);
            let wait = session.copy_wait(ticket);
            surfaces.push(GpuSurface::new(
                resource,
                layout,
                self.generation,
                Some(pin),
                Some(wait),
                shared,
            ));
            self.generation = self.generation.wrapping_add(1);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` without an HEVC profile or when the backend
    /// cannot decode `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - compressed picture data.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d12Error> {
        if !self.device.supports(Codec::Hevc) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::Hevc }.into());
        }
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut surfaces = Vec::new();
        for picture in &pictures {
            self.texture_picture(picture, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d12Error> {
        let mut surfaces = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            self.texture_picture(&picture, &mut surfaces)?;
        }
        if let Some(active) = self.active {
            self.drain_live_texture(&active, &mut surfaces)?;
        }
        Ok(surfaces)
    }
}

impl Decoder for HevcDecoder<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn caps(&self) -> GpuCaps {
        let share = self.device.share_mechanism().filter(|_| {
            self.session
                .as_ref()
                .is_none_or(DecodeSession::exports_shared)
        });
        GpuCaps::new(share, share.is_some(), false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
        if !self.device.supports(Codec::Hevc) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::Hevc }.into());
        }
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for picture in &pictures {
            if let Err(error) = self.submit_picture(picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture(&picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

/// Session plus picture buffer for `next`, reusing submit `queues`.
///
/// # Errors
///
/// Returns `Err` without a decode profile, past the adapter caps, when
/// the driver refuses the session, or without the sequence set.
///
/// # Arguments
///
/// * `device` - D3D12 device behind the session.
/// * `ingest` - parameter sets behind the sequence.
/// * `pins` - live-surface pin counts over the decoder pool.
/// * `queues` - submit queues from the dropped session, absent on first open.
/// * `next` - sequence behind the session.
pub(crate) fn open_sequence_hevc(
    device: &D3d12Device,
    ingest: &HevcIngest,
    pins: Arc<SlotPins>,
    queues: Option<DecodeQueues>,
    next: Active,
) -> Result<(DecodeSession, HevcDpb), D3d12Error> {
    let session = DecodeSession::open(
        device,
        Codec::Hevc,
        next.width,
        next.height,
        next.crop,
        queues,
    )?;
    let sps = ingest.sps(next.sps_id).ok_or(D3d12Error::NoSession)?;
    Ok((session, HevcDpb::new(sps, pins)))
}

/// Active sequence behind `picture`'s parameter sets.
///
/// # Errors
///
/// Returns `Err` without the picture sets, or when the crop empties
/// the picture.
pub(crate) fn sequence_of_hevc(
    ingest: &HevcIngest,
    picture: &HevcPicture,
) -> Result<Active, D3d12Error> {
    let pps = ingest.pps(picture.pps_id()).ok_or(D3d12Error::NoSession)?;
    let sps = ingest.sps(pps.sps_id).ok_or(D3d12Error::NoSession)?;
    let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
        .ok_or(D3d12Error::Parse(ParseReason::CropEmptiesPicture))?;
    Ok(Active {
        sps_id: sps.id,
        width: sps.width,
        height: sps.height,
        crop: sps.crop.map(|crop| Crop {
            left: crop.left,
            right: crop.right,
            top: crop.top,
            bottom: crop.bottom,
        }),
        visible,
    })
}

/// Submission plus display-order outputs for `picture`.
///
/// # Errors
///
/// Returns `Err` without the picture sets, or when the picture
/// buffer rejects `picture`.
pub(crate) fn submit_hevc_dpb(
    ingest: &HevcIngest,
    dpb: &mut HevcDpb,
    picture: &HevcPicture,
) -> Result<HevcDecoded, D3d12Error> {
    let pps = ingest.pps(picture.pps_id()).ok_or(D3d12Error::NoSession)?;
    let sps = ingest.sps(pps.sps_id).ok_or(D3d12Error::NoSession)?;
    dpb.decode(picture, sps, pps)
}
