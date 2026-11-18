//! H.264, HEVC, and AV1 decode over live video sessions.

use std::sync::Arc;

use crate::av1::{Frame as Av1Frame, Ingest as Av1Ingest};
use crate::av1_dpb::{Av1Decoded, Av1Dpb};
use crate::decode::{DecodeQueues, DecodeSession, SharedExport};
use crate::decoder::D3d12Frame;
use crate::device::D3d12Device;
use crate::dpb::{Decoded, Dpb};
use crate::error::{D3d12Error, ParseReason, SkipReason, UnsupportedReason};
use crate::guid::Codec;
use crate::h264::{Crop, Ingest, Picture};
use crate::hevc::{Ingest as HevcIngest, Picture as HevcPicture};
use crate::hevc_dpb::{HevcDecoded, HevcDpb};
use crate::os::FrameOs;
use crate::pins::SlotPins;
use crate::submit::Ticket;
use crate::texture::{GpuSurface, SurfaceLayout};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, SharedDevice};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

/// Active sequence behind the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Active {
    /// Sequence set id.
    sps_id: u8,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Visible rectangle, absent when uncropped.
    crop: Option<Crop>,
    /// Visible size in luma samples.
    visible: PictureSize,
}

impl Active {
    /// True when `other` shares the coded extent.
    pub(crate) fn same_extent(self, other: Active) -> bool {
        self.width == other.width && self.height == other.height
    }
}

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

/// AV1 decoder over a D3D12 device.
pub struct Av1Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d12Device,
    /// OBU ingest: sequence header plus frame assembly.
    ingest: Av1Ingest,
    /// Live reference slots, created from the first sequence header.
    dpb: Option<Av1Dpb>,
    /// Live decode session, created from the first sequence header.
    session: Option<DecodeSession>,
    /// VRAM budget sized to the adapter memory.
    shared: SharedDevice,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-frame failures.
    resets: u64,
    /// Peak outputs queued before readback in one call.
    inflight_max: usize,
    /// Live-surface pin counts over the decoder pool.
    pins: Arc<SlotPins>,
}

impl<'a> Av1Decoder<'a> {
    /// Decoder over `device`; the session starts on the first frame.
    #[must_use]
    pub fn open(device: &'a D3d12Device) -> Self {
        Self {
            device,
            ingest: Av1Ingest::new(),
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

    /// Video decode queue behind submissions, absent before the first frame builds the session.
    ///
    /// Shares the session queue; threading follows [`D3d12Device`].
    #[must_use]
    pub fn video_queue(&self) -> Option<&ID3D12CommandQueue> {
        self.session.as_ref().map(DecodeSession::video_queue)
    }

    /// Direct queue behind downloads, absent before the first frame builds the session.
    ///
    /// Shares the session queue; threading follows [`D3d12Device`].
    #[must_use]
    pub fn copy_queue(&self) -> Option<&ID3D12CommandQueue> {
        self.session.as_ref().map(DecodeSession::copy_queue)
    }

    /// Stream resets after mid-frame failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs queued before readback in one call.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.inflight_max
    }

    /// Decode `frame`, queueing its outputs for readback into `pending`.
    fn submit_frame(
        &mut self,
        frame: &Av1Frame,
        pending: &mut Vec<(u8, Ticket, Active)>,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.consume_pending(pending, frames)?;
            pending.clear();
            self.reconfigure(active, frames)?;
        } else if live != Some(active) {
            self.active = Some(active);
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                if !matches!(error, D3d12Error::NoFreeSlot) {
                    self.reset_stream();
                }
                return Err(error);
            }
        };
        if let Err(error) = self.submit_downloads(&decoded, active, pending) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `decoded` submission through the session plus queued downloads into `pending`.
    fn submit_downloads(
        &mut self,
        decoded: &Av1Decoded,
        active: Active,
        pending: &mut Vec<(u8, Ticket, Active)>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any copy: outputs may name the submission slot.
        if let Some(submission) = decoded.submission.as_ref() {
            let ticket = session.submit_av1(submission)?;
            session.wait_video(ticket)?;
        }
        for output in &decoded.outputs {
            let ticket = session.start_download(output.slot())?;
            pending.push((output.slot(), ticket, active));
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
            let nv12 = visible_nv12(nv12, active)?;
            let export = session.shared_handle(*slot);
            let frame = emit_frame(&self.shared, nv12, export, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, slots, and sequence after a mid-frame failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.active = None;
        self.ingest.reset_refs();
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

    /// Session plus reference slots for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d12Error> {
        let queues = self.session.take().map(DecodeSession::into_queues);
        let pins = Arc::new(SlotPins::new());
        let (session, dpb) =
            open_sequence_av1(self.device, &self.ingest, Arc::clone(&pins), queues, active)?;
        self.session = Some(session);
        self.pins = pins;
        self.dpb = Some(dpb);
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `frame`'s headers.
    fn sequence(&self, frame: &Av1Frame) -> Result<Active, D3d12Error> {
        frame_active_av1(&self.ingest, frame, self.active)
    }

    /// Submission plus display-order outputs for `frame`.
    fn decode_dpb(&mut self, frame: &Av1Frame) -> Result<Av1Decoded, D3d12Error> {
        let dpb = self.dpb.as_mut().ok_or(D3d12Error::NoSession)?;
        submit_av1_dpb(&self.ingest, dpb, frame)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let (Some(_live), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        let outputs = Av1Dpb::drain();
        let mut pending = Vec::with_capacity(outputs.len());
        for output in &outputs {
            pending.push((output.slot(), session.start_download(output.slot())?));
        }
        let depth = pending.len();
        for (slot, ticket) in pending {
            let nv12 = session.finish_download(slot, ticket)?;
            let nv12 = visible_nv12(nv12, active)?;
            let export = session.shared_handle(slot);
            let frame = emit_frame(&self.shared, nv12, export, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        self.note_inflight(depth);
        Ok(())
    }

    /// Decode `frame` into GPU `surfaces`.
    fn texture_frame(
        &mut self,
        frame: &Av1Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let outcome = self.texture_inner(frame, surfaces);
        if let Err(error) = &outcome {
            if !matches!(error, D3d12Error::NoFreeSlot) {
                self.reset_stream();
            }
        }
        outcome
    }

    /// Decode `frame` into GPU `surfaces` without resetting on failure.
    fn texture_inner(
        &mut self,
        frame: &Av1Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let active = self.sequence(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.reconfigure_texture(active, surfaces)?;
        } else if live != Some(active) {
            self.active = Some(active);
        }
        let active = self.active.ok_or(D3d12Error::NoSession)?;
        let decoded = match self.decode_dpb(frame) {
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
        decoded: &Av1Decoded,
        active: &Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any surface handoff: outputs may name the submission slot.
        let ticket = match decoded.submission.as_ref() {
            Some(submission) => Some(session.submit_av1(submission)?),
            None => None,
        };
        if let Some(ticket) = ticket {
            session.wait_video(ticket)?;
        }
        for output in &decoded.outputs {
            let slot = output.slot();
            let resource = session.texture(slot)?;
            let shared = session_surface_export(session, slot);
            let layout = surface_layout(active, slot)?;
            let pin = self.pins.pin(slot);
            let wait = ticket.map(|ticket| session.video_wait(ticket));
            surfaces.push(GpuSurface::new(
                resource,
                layout,
                self.generation,
                Some(pin),
                wait,
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
        let (Some(_live), Some(session)) = (self.dpb.as_ref(), self.session.as_ref()) else {
            return Ok(());
        };
        for output in Av1Dpb::drain() {
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
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns `Err` without an AV1 profile or when the backend
    /// cannot decode `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - compressed frame data.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d12Error> {
        if !self.device.supports(Codec::Av1) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::Av1 }.into());
        }
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut surfaces = Vec::new();
        for frame in &frames_in {
            self.texture_frame(frame, &mut surfaces)?;
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
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            self.texture_frame(&frame, &mut surfaces)?;
        }
        if let Some(active) = self.active {
            self.drain_live_texture(&active, &mut surfaces)?;
        }
        Ok(surfaces)
    }
}

impl Decoder for Av1Decoder<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
        if !self.device.supports(Codec::Av1) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::Av1 }.into());
        }
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for frame in &frames_in {
            if let Err(error) = self.submit_frame(frame, &mut pending, &mut frames) {
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
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame(&frame, &mut pending, &mut frames) {
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

/// H.264 decoder over a D3D12 device.
pub struct H264Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d12Device,
    /// Annex B ingest: sets plus picture assembly.
    ingest: Ingest,
    /// Live picture buffer, created from the first sequence set.
    dpb: Option<Dpb>,
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

impl<'a> H264Decoder<'a> {
    /// Decoder over `device`; the session starts on the first picture.
    #[must_use]
    pub fn open(device: &'a D3d12Device) -> Self {
        Self {
            device,
            ingest: Ingest::new(),
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
        picture: &Picture,
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
            match session.submit(&decoded.submission) {
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
            open_sequence(self.device, &self.ingest, Arc::clone(&pins), queues, active)?;
        self.session = Some(session);
        self.pins = pins;
        self.dpb = Some(dpb);
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `picture`'s parameter sets.
    fn sequence(&self, picture: &Picture) -> Result<Active, D3d12Error> {
        sequence_of(&self.ingest, picture)
    }

    /// Submission plus display-order outputs for `picture`.
    fn decode_dpb(&mut self, picture: &Picture) -> Result<Decoded, D3d12Error> {
        let dpb = self.dpb.as_mut().ok_or(D3d12Error::NoSession)?;
        submit_dpb(&self.ingest, dpb, picture)
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
        picture: &Picture,
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
        picture: &Picture,
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
        decoded: &Decoded,
        active: &Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any surface handoff: outputs may name the submission slot.
        let ticket = session.submit(&decoded.submission)?;
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
    /// Returns `Err` without an H.264 profile or when the backend
    /// cannot decode `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - compressed picture data.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d12Error> {
        if !self.device.supports(Codec::H264) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
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

impl Decoder for H264Decoder<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
        if !self.device.supports(Codec::H264) {
            return Err(SkipReason::ProfileUnsupported { codec: Codec::H264 }.into());
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

/// Surface layout for pool `slot` under `active`.
fn surface_layout(active: &Active, slot: u8) -> Result<SurfaceLayout, D3d12Error> {
    let coded = PictureSize::new(active.width, active.height)
        .ok_or(D3d12Error::Parse(ParseReason::EmptyCodedSize))?;
    let origin = active.crop.map_or((0, 0), |crop| (crop.left, crop.top));
    Ok(SurfaceLayout::new(
        active.visible,
        coded,
        origin,
        u32::from(slot),
    ))
}

/// NT-handle export for pool `slot`, honoring the session crop.
fn session_surface_export(session: &DecodeSession, slot: u8) -> Option<SharedExport> {
    if session.cropped() {
        session.shared_handle_crop(slot)
    } else {
        session.shared_handle(slot)
    }
}

/// Visible NV12 bytes of coded `nv12` under `active`.
///
/// AV1 sessions decode at the sequence maximum; frames below it crop
/// the right and bottom pad.
///
/// # Errors
///
/// Returns `Err` past the picture cap, or when `nv12` falls short of
/// the coded extent.
pub(crate) fn visible_nv12(nv12: Vec<u8>, active: &Active) -> Result<Vec<u8>, D3d12Error> {
    let too_large = || D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge);
    let short = || D3d12Error::Unsupported(UnsupportedReason::ReadbackShortOfExtent);
    let visible_width = usize::try_from(active.visible.width()).map_err(|_| too_large())?;
    let visible_height = usize::try_from(active.visible.height()).map_err(|_| too_large())?;
    let coded_width = usize::try_from(active.width).map_err(|_| too_large())?;
    let coded_height = usize::try_from(active.height).map_err(|_| too_large())?;
    if visible_width == coded_width && visible_height == coded_height {
        return Ok(nv12);
    }
    let luma = coded_width.checked_mul(coded_height).ok_or_else(short)?;
    if nv12.len() != luma + luma / 2 {
        return Err(short());
    }
    let mut visible = Vec::with_capacity(visible_width * visible_height * 3 / 2);
    for row in 0..visible_height {
        let start = row.checked_mul(coded_width).ok_or_else(short)?;
        visible.extend_from_slice(nv12.get(start..start + visible_width).ok_or_else(short)?);
    }
    for row in 0..visible_height / 2 {
        let start = luma + row.checked_mul(coded_width).ok_or_else(short)?;
        visible.extend_from_slice(nv12.get(start..start + visible_width).ok_or_else(short)?);
    }
    Ok(visible)
}

/// Frame for NV12 `nv12` at `generation`.
pub(crate) fn emit_frame(
    budget: &SharedDevice,
    nv12: Vec<u8>,
    export: Option<SharedExport>,
    active: &Active,
    generation: u64,
) -> Result<D3d12Frame, D3d12Error> {
    let luma = u64::from(active.visible.width()) * u64::from(active.visible.height());
    let y_len = usize::try_from(luma)
        .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
    if nv12.len() != y_len + y_len / 2 {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ReadbackShortOfExtent,
        ));
    }
    let identity = GpuFrame::new(active.visible, PixelFormat::Nv12, generation);
    let bytes = Bytes::from(nv12);
    let lease = budget.try_acquire(bytes.len() as u64)?;
    Ok(D3d12Frame::new(
        identity,
        bytes,
        lease,
        FrameOs::wrap(export),
    ))
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
pub(crate) fn open_sequence(
    device: &D3d12Device,
    ingest: &Ingest,
    pins: Arc<SlotPins>,
    queues: Option<DecodeQueues>,
    next: Active,
) -> Result<(DecodeSession, Dpb), D3d12Error> {
    let session = DecodeSession::open(
        device,
        Codec::H264,
        next.width,
        next.height,
        next.crop,
        queues,
    )?;
    let sps = ingest.sps(next.sps_id).ok_or(D3d12Error::NoSession)?;
    Ok((session, Dpb::new(sps, pins)))
}

/// Active sequence behind `picture`'s parameter sets.
///
/// # Errors
///
/// Returns `Err` without the picture sets, or when the crop empties
/// the picture.
pub(crate) fn sequence_of(ingest: &Ingest, picture: &Picture) -> Result<Active, D3d12Error> {
    let pps = ingest.pps(picture.pps_id()).ok_or(D3d12Error::NoSession)?;
    let sps = ingest
        .sps(pps.seq_parameter_set_id)
        .ok_or(D3d12Error::NoSession)?;
    let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
        .ok_or(D3d12Error::Parse(ParseReason::CropEmptiesPicture))?;
    Ok(Active {
        sps_id: sps.seq_parameter_set_id,
        width: sps.width(),
        height: sps.height(),
        crop: sps.crop,
        visible,
    })
}

/// Submission plus display-order outputs for `picture`.
///
/// # Errors
///
/// Returns `Err` without the picture sets, or when the picture
/// buffer rejects `picture`.
pub(crate) fn submit_dpb(
    ingest: &Ingest,
    dpb: &mut Dpb,
    picture: &Picture,
) -> Result<Decoded, D3d12Error> {
    let pps = ingest.pps(picture.pps_id()).ok_or(D3d12Error::NoSession)?;
    let sps = ingest
        .sps(pps.seq_parameter_set_id)
        .ok_or(D3d12Error::NoSession)?;
    dpb.decode(picture, sps, pps)
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
    let sps = ingest
        .sps(pps.seq_parameter_set_id)
        .ok_or(D3d12Error::NoSession)?;
    let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
        .ok_or(D3d12Error::Parse(ParseReason::CropEmptiesPicture))?;
    Ok(Active {
        sps_id: sps.seq_parameter_set_id,
        width: sps.width(),
        height: sps.height(),
        crop: sps.crop,
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
    let sps = ingest
        .sps(pps.seq_parameter_set_id)
        .ok_or(D3d12Error::NoSession)?;
    dpb.decode(picture, sps, pps)
}

/// Session plus reference slots for `next`, reusing submit `queues`.
///
/// # Errors
///
/// Returns `Err` without a decode profile, past the adapter caps, when
/// the driver refuses the session, or without the sequence header.
///
/// # Arguments
///
/// * `device` - D3D12 device behind the session.
/// * `ingest` - sequence header behind the session.
/// * `pins` - live-surface pin counts over the decoder pool.
/// * `queues` - submit queues from the dropped session, absent on first open.
/// * `next` - sequence behind the session; the session opens uncropped at its
///   maximum, frames below it crop per output.
pub(crate) fn open_sequence_av1(
    device: &D3d12Device,
    ingest: &Av1Ingest,
    pins: Arc<SlotPins>,
    queues: Option<DecodeQueues>,
    next: Active,
) -> Result<(DecodeSession, Av1Dpb), D3d12Error> {
    if ingest.seq().is_none() {
        return Err(D3d12Error::NoSession);
    }
    let session = DecodeSession::open(device, Codec::Av1, next.width, next.height, None, queues)?;
    Ok((session, Av1Dpb::new(pins)))
}

/// Active sequence behind `frame`'s headers.
///
/// # Errors
///
/// Returns `Err` without the sequence header, past the sequence
/// maximum, on odd extents, or when the frame empties the picture.
pub(crate) fn frame_active_av1(
    ingest: &Av1Ingest,
    frame: &Av1Frame,
    current: Option<Active>,
) -> Result<Active, D3d12Error> {
    let seq = ingest.seq().ok_or(D3d12Error::NoSession)?;
    let header = &frame.header;
    if header.show_existing_frame() {
        return current.ok_or(D3d12Error::NoSession);
    }
    let width = seq.max_frame_width();
    let height = seq.max_frame_height();
    if header.frame_width > width || header.frame_height > height {
        return Err(D3d12Error::Parse(
            ParseReason::FrameSizeAboveSequenceMaximum,
        ));
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(D3d12Error::Unsupported(UnsupportedReason::OddNv12Extent));
    }
    if !header.frame_width.is_multiple_of(2) || !header.frame_height.is_multiple_of(2) {
        return Err(D3d12Error::Unsupported(UnsupportedReason::OddVisibleExtent));
    }
    let crop = if header.frame_width == width && header.frame_height == height {
        None
    } else {
        Some(Crop {
            left: 0,
            right: width - header.frame_width,
            top: 0,
            bottom: height - header.frame_height,
        })
    };
    let visible = PictureSize::new(header.frame_width, header.frame_height)
        .ok_or(D3d12Error::Parse(ParseReason::CropEmptiesPicture))?;
    Ok(Active {
        sps_id: 0,
        width,
        height,
        crop,
        visible,
    })
}

/// Submission plus display-order outputs for `frame`.
///
/// # Errors
///
/// Returns `Err` without the sequence header, or when the
/// reference slots reject `frame`.
pub(crate) fn submit_av1_dpb(
    ingest: &Av1Ingest,
    dpb: &mut Av1Dpb,
    frame: &Av1Frame,
) -> Result<Av1Decoded, D3d12Error> {
    let seq = ingest.seq().ok_or(D3d12Error::NoSession)?;
    dpb.decode(frame, seq)
}
