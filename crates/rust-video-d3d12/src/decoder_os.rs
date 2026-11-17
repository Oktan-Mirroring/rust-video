//! H.264 and HEVC decode over live video sessions.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::decode::{
    DecodeQueues, DecodeSession, H264Submission, HevcSubmission, PictureSubmission, SharedExport,
};
use crate::decoder::D3d12Frame;
use crate::device::D3d12Device;
use crate::dpb::{Decoded, Dpb};
use crate::error::{D3d12Error, ParseReason, SkipReason, UnsupportedReason};
use crate::guid::Codec;
use crate::h264::{Crop, Ingest, Picture};
use crate::os::FrameOs;
use crate::pins::{SlotPin, SlotPins};
use crate::submit::Ticket;
use crate::texture::{GpuSurface, SurfaceLayout};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::delivery::{
    PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::frame::{GpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, SharedDevice};
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

/// Active sequence behind the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Active {
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

/// Real decoder over ingest plus a video decode session.
macro_rules! real_decoder {
    ($decoder:ident, $codec:ident, $ingest:ty, $picture:ty, $submission:ty, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device owning the decode session.
            device: &'a D3d12Device,
            /// Annex B ingest: sets plus picture assembly.
            ingest: $ingest,
            /// Live decode session, created from the first sequence set.
            session: Option<DecodeSession>,
            /// VRAM budget shared with the device.
            shared: SharedDevice,
            /// Sequence behind the live session.
            active: Option<Active>,
            /// Driver-echoed picture serial.
            feedback: u32,
            /// Next frame generation number.
            generation: u64,
            /// Live-surface pin counts over the decoder pool.
            pins: Arc<SlotPins>,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; the session starts on the first picture.
            #[must_use]
            pub fn open(device: &'a D3d12Device) -> Self {
                Self {
                    device,
                    ingest: <$ingest>::new(),
                    session: None,
                    shared: device.shared().clone(),
                    active: None,
                    feedback: 1,
                    generation: 0,
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

            /// Decode `picture` into `frames`.
            fn decode_picture(
                &mut self,
                picture: &$picture,
                frames: &mut Vec<D3d12Frame>,
            ) -> Result<(), D3d12Error> {
                let outcome = self.decode_picture_inner(picture, frames);
                if let Err(error) = &outcome {
                    if !matches!(error, D3d12Error::NoFreeSlot) {
                        self.reset_stream();
                    }
                }
                outcome
            }

            /// Decode `picture` into `frames` without resetting on failure.
            fn decode_picture_inner(
                &mut self,
                picture: &$picture,
                frames: &mut Vec<D3d12Frame>,
            ) -> Result<(), D3d12Error> {
                let (active, submission) = self.prepare_picture(picture)?;
                if self.pins.pinned(0) {
                    return Err(D3d12Error::NoFreeSlot);
                }
                let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
                let nv12 = submission.submit(session)?;
                let luma = u64::from(active.visible.width()) * u64::from(active.visible.height());
                let y_len = usize::try_from(luma)
                    .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
                if nv12.len() != y_len + y_len / 2 {
                    return Err(D3d12Error::Unsupported(
                        UnsupportedReason::ReadbackShortOfExtent,
                    ));
                }
                let identity = GpuFrame::new(active.visible, PixelFormat::Nv12, self.generation);
                self.generation = self.generation.wrapping_add(1);
                let bytes = Bytes::from(nv12);
                let lease = self.shared.try_acquire(bytes.len() as u64)?;
                frames.push(D3d12Frame::new(
                    identity,
                    bytes,
                    lease,
                    FrameOs::wrap(session.shared_handle(0)),
                ));
                Ok(())
            }

            /// Session plus arguments for `picture`.
            fn prepare_picture(
                &mut self,
                picture: &$picture,
            ) -> Result<(Active, $submission), D3d12Error> {
                let active = {
                    let pps = self
                        .ingest
                        .pps(picture.pps_id())
                        .ok_or(D3d12Error::NoSession)?;
                    let sps = self
                        .ingest
                        .sps(pps.seq_parameter_set_id)
                        .ok_or(D3d12Error::NoSession)?;
                    let visible = PictureSize::new(sps.visible_width(), sps.visible_height())
                        .ok_or(D3d12Error::Parse(ParseReason::CropEmptiesPicture))?;
                    Active {
                        sps_id: sps.seq_parameter_set_id,
                        width: sps.width(),
                        height: sps.height(),
                        crop: sps.crop,
                        visible,
                    }
                };
                if self.active != Some(active) {
                    let queues = self.session.take().map(DecodeSession::into_queues);
                    let session = DecodeSession::open(
                        self.device,
                        Codec::$codec,
                        active.width,
                        active.height,
                        active.crop,
                        queues,
                    )?;
                    self.session = Some(session);
                    self.pins = Arc::new(SlotPins::new());
                    self.active = Some(active);
                }
                let active = self.active.ok_or(D3d12Error::NoSession)?;
                let submission = {
                    let pps = self
                        .ingest
                        .pps(picture.pps_id())
                        .ok_or(D3d12Error::NoSession)?;
                    let sps = self
                        .ingest
                        .sps(pps.seq_parameter_set_id)
                        .ok_or(D3d12Error::NoSession)?;
                    <$submission>::build(sps, pps, picture, self.feedback)?
                };
                self.feedback = self.feedback.wrapping_add(1);
                Ok((active, submission))
            }

            /// `picture` into GPU `surfaces` without resetting on failure.
            fn texture_inner(
                &mut self,
                picture: &$picture,
                surfaces: &mut Vec<GpuSurface>,
            ) -> Result<(), D3d12Error> {
                let (active, submission) = self.prepare_picture(picture)?;
                if self.pins.pinned(0) {
                    return Err(D3d12Error::NoFreeSlot);
                }
                let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
                let (resource, wait) = submission.submit_texture(session)?;
                let shared = session_surface_export(session, 0);
                let layout = surface_layout(&active, 0)?;
                let pin = self.pins.pin(0);
                let surface = GpuSurface::new(
                    resource,
                    layout,
                    self.generation,
                    Some(pin),
                    Some(wait),
                    shared,
                );
                self.generation = self.generation.wrapping_add(1);
                surfaces.push(surface);
                Ok(())
            }

            /// `picture` into GPU `surfaces`.
            fn texture_picture(
                &mut self,
                picture: &$picture,
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

            /// Drop the live session and sequence after a mid-picture failure.
            fn reset_stream(&mut self) {
                self.session = None;
                self.active = None;
            }

            /// GPU surfaces decoded from `packet`, possibly none.
            ///
            /// # Errors
            ///
            /// Returns `Err` without a `codec` profile or when the backend
            /// cannot decode `packet`.
            ///
            /// # Arguments
            ///
            /// * `packet` - compressed picture data.
            pub fn decode_surface(
                &mut self,
                packet: &Packet,
            ) -> Result<Vec<GpuSurface>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                let pictures = self.ingest.ingest_packet(packet.data())?;
                if pictures.len() > 1 {
                    return Err(D3d12Error::Unsupported(
                        UnsupportedReason::MultiPicturePacketNeedsPerPictureEncode,
                    ));
                }
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
                Ok(surfaces)
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d12Frame;
            type Error = D3d12Error;

            fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                let pictures = self.ingest.ingest_packet(packet.data())?;
                let mut frames = Vec::new();
                for picture in &pictures {
                    self.decode_picture(picture, &mut frames)?;
                }
                Ok(frames)
            }

            fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
                let mut frames = Vec::new();
                if let Some(picture) = self.ingest.finish()? {
                    self.decode_picture(&picture, &mut frames)?;
                }
                Ok(frames)
            }
        }
    };
}

real_decoder!(
    HevcDecoder,
    Hevc,
    crate::hevc::Ingest,
    crate::hevc::Picture,
    HevcSubmission,
    "HEVC decoder over a D3D12 device. Decodes IDR pictures only; inter pictures and non-IDR intra pictures miss loud."
);

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

/// In-flight picture: submitted video plus queued downloads.
struct InflightPicture {
    /// Decode arguments behind the submit, dropped past the video ticket.
    submission: Option<Box<H264Submission>>,
    /// Pool pin behind the submission slot, steering the buffer past it.
    pin: SlotPin,
    /// Video queue ticket behind the decode.
    video: Ticket,
    /// Video signal time, stamped on poll.
    video_signaled: Option<Instant>,
    /// Queued downloads in output order.
    outputs: VecDeque<PendingOutput>,
}

/// H.264 decode with pipelined multi-inflight submits behind the async traits.
///
/// Submits video work without blocking on completion. Downloads ride
/// the copy queue behind GPU waits; copy N runs beside decode N+1.
/// Completions surface in order through poll or blocking receive.
pub struct OverlappedH264Pump<'a> {
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
    /// Pipelined picture ceiling behind sends.
    bound: usize,
    /// Submitted pictures with work outstanding, oldest first.
    pictures: VecDeque<InflightPicture>,
    /// Completed frames awaiting receive, oldest first.
    ready: VecDeque<D3d12Frame>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
    /// Peak pipelined pictures with incomplete GPU work.
    max_depth: usize,
    /// Phase-split timing per harvested frame, oldest first.
    timings: Vec<PhaseTiming>,
    /// Progress signal sender behind blocking receive.
    wake_tx: Sender<()>,
    /// Progress signal receiver behind blocking receive.
    wake_rx: Receiver<()>,
    /// Live-surface pin counts over the decoder pool.
    pins: Arc<SlotPins>,
}

impl<'a> OverlappedH264Pump<'a> {
    /// Pipelined picture ceiling, sized to the session bitstream ring.
    pub const MAX_INFLIGHT: usize = 4;

    /// Pump over `device` with `max_inflight` pipelined pictures.
    ///
    /// # Arguments
    ///
    /// * `device` - D3D12 device behind decode sessions.
    /// * `max_inflight` - pipelined picture ceiling, clamped to 1..=`MAX_INFLIGHT`.
    #[must_use]
    pub fn open(device: &'a D3d12Device, max_inflight: usize) -> Self {
        let (wake_tx, wake_rx) = channel();
        Self {
            device,
            ingest: Ingest::new(),
            dpb: None,
            session: None,
            shared: device.shared().clone(),
            active: None,
            generation: 0,
            resets: 0,
            bound: max_inflight.clamp(1, Self::MAX_INFLIGHT),
            pictures: VecDeque::new(),
            ready: VecDeque::new(),
            finished: false,
            max_depth: 0,
            timings: Vec::new(),
            wake_tx,
            wake_rx,
            pins: Arc::new(SlotPins::new()),
        }
    }

    /// Pipelined picture ceiling behind sends.
    #[must_use]
    pub fn bound(&self) -> usize {
        self.bound
    }

    /// Peak pipelined pictures with incomplete GPU work.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Completed frames awaiting receive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ready.len()
    }

    /// True when no frame awaits receive.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// True once input ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Phase-split timing per harvested frame, oldest first.
    #[must_use]
    pub fn timings(&self) -> &[PhaseTiming] {
        &self.timings
    }

    /// Decode `picture` through a pipelined submit plus queued downloads.
    fn submit_picture(&mut self, picture: &Picture) -> Result<(), D3d12Error> {
        let active = sequence_of(&self.ingest, picture)?;
        if self.active != Some(active) {
            self.drain_pipeline()?;
            self.reconfigure(active)?;
        }
        let dpb = self.dpb.as_mut().ok_or(D3d12Error::NoSession)?;
        let decoded = match submit_dpb(&self.ingest, dpb, picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                if !matches!(error, D3d12Error::NoFreeSlot) {
                    self.reset_stream();
                }
                return Err(error);
            }
        };
        if let Err(error) = self.quiesce_slot(decoded.submission.slot) {
            self.reset_stream();
            return Err(error);
        }
        if let Err(error) = self.submit_tracked(active, decoded) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `decoded` submission through the video queue plus queued downloads, tracked in flight.
    fn submit_tracked(&mut self, active: Active, decoded: Decoded) -> Result<(), D3d12Error> {
        // Boxed before submit: the recorded pointers name stable heap addresses.
        let submission = Box::new(decoded.submission);
        let pin = self.pins.pin(submission.slot);
        let Some(session) = self.session.as_mut() else {
            return Err(D3d12Error::NoSession);
        };
        let video_submit = Instant::now();
        let ticket = session.submit(&submission)?;
        let submit_cpu = video_submit.elapsed();
        let mut pending = VecDeque::with_capacity(decoded.outputs.len());
        for output in &decoded.outputs {
            let copy_submit = Instant::now();
            let copy = session.start_download_after(output.slot(), ticket)?;
            let generation = self.generation;
            self.generation = self.generation.wrapping_add(1);
            pending.push_back(PendingOutput {
                pin: self.pins.pin(output.slot()),
                copy,
                copy_submit,
                active,
                generation,
                video_submit,
                submit_cpu,
            });
        }
        self.pictures.push_back(InflightPicture {
            submission: Some(submission),
            pin,
            video: ticket,
            video_signaled: None,
            outputs: pending,
        });
        self.note_depth();
        Ok(())
    }

    /// In-flight tickets behind `slot` completion.
    fn quiesce_slot(&mut self, slot: u8) -> Result<(), D3d12Error> {
        let mut videos = Vec::new();
        let mut copies = Vec::new();
        for picture in &self.pictures {
            if picture.pin.slot() == slot {
                videos.push(picture.video);
            }
            for output in &picture.outputs {
                if output.pin.slot() == slot {
                    copies.push(output.copy);
                }
            }
        }
        if videos.is_empty() && copies.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        for ticket in videos {
            session.wait_video(ticket)?;
        }
        for ticket in copies {
            session.wait_copy(ticket)?;
        }
        self.harvest_poll()
    }

    /// Completed front outputs into the ready queue, in order.
    fn harvest_poll(&mut self) -> Result<(), D3d12Error> {
        if self.pictures.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        for picture in &mut self.pictures {
            if picture.video_signaled.is_none() && session.video_signaled(picture.video) {
                picture.video_signaled = Some(Instant::now());
                picture.submission = None;
            }
        }
        while self.harvest_front()? {}
        Ok(())
    }

    /// Oldest output into the ready queue when its copy signaled.
    ///
    /// # Returns
    ///
    /// True on progress, false while the front output runs.
    fn harvest_front(&mut self) -> Result<bool, D3d12Error> {
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        if let Some(front) = self.pictures.front() {
            if front.outputs.is_empty() {
                if front.video_signaled.is_none() {
                    return Ok(false);
                }
                self.pictures.pop_front();
                return Ok(true);
            }
        } else {
            return Ok(false);
        }
        let output = self
            .pictures
            .front_mut()
            .and_then(|front| front.outputs.pop_front());
        let Some(output) = output else {
            return Ok(false);
        };
        match session.poll_download(output.pin.slot(), output.copy)? {
            None => {
                if let Some(front) = self.pictures.front_mut() {
                    front.outputs.push_front(output);
                }
                Ok(false)
            }
            Some(nv12) => {
                let signaled = Instant::now();
                let video = self
                    .pictures
                    .front()
                    .and_then(|front| front.video_signaled)
                    .unwrap_or(signaled);
                self.timings
                    .push(PhaseTiming::new(&output, video, signaled, signaled));
                let export = session.shared_handle(output.pin.slot());
                let frame = emit_frame(
                    &self.shared,
                    nv12,
                    export,
                    &output.active,
                    output.generation,
                )?;
                self.ready.push_back(frame);
                Ok(true)
            }
        }
    }

    /// Oldest completed frame, harvesting first.
    ///
    /// # Returns
    ///
    /// `None` while no submitted output completed.
    fn poll_ready(&mut self) -> Result<Option<D3d12Frame>, D3d12Error> {
        self.harvest_poll()?;
        Ok(self.ready.pop_front())
    }

    /// Oldest pending ticket completion, then harvest.
    fn pump_progress(&mut self) -> Result<(), D3d12Error> {
        self.harvest_poll()?;
        if !self.ready.is_empty() || self.pictures.is_empty() {
            return Ok(());
        }
        self.wait_front()?;
        self.harvest_poll()
    }

    /// Front ticket completion, blocking.
    fn wait_front(&self) -> Result<(), D3d12Error> {
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        if let Some(output) = self
            .pictures
            .front()
            .and_then(|front| front.outputs.front())
        {
            session.wait_copy(output.copy)
        } else if let Some(front) = self.pictures.front() {
            session.wait_video(front.video)
        } else {
            Ok(())
        }
    }

    /// Front ticket completion before `deadline`, through the fence event.
    ///
    /// # Errors
    ///
    /// Returns `Err` without a live session, or when the event call fails.
    ///
    /// # Arguments
    ///
    /// * `deadline` - signal budget, absent for none.
    ///
    /// # Returns
    ///
    /// True when the ticket signaled, false past the deadline.
    fn wait_front_before(&self, deadline: Option<Instant>) -> Result<bool, D3d12Error> {
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        let Some(front) = self.pictures.front() else {
            return Ok(true);
        };
        let video = front.video;
        let copy = front.outputs.front().map(|output| output.copy);
        let Some(timeout) = deadline.map(|at| at.saturating_duration_since(Instant::now())) else {
            return match copy {
                Some(ticket) => session.wait_copy(ticket),
                None => session.wait_video(video),
            }
            .map(|()| true);
        };
        match copy {
            Some(ticket) => session.wait_copy_timeout(ticket, timeout),
            None => session.wait_video_timeout(video, timeout),
        }
    }

    /// Every in-flight picture into the ready queue, waiting.
    fn drain_pipeline(&mut self) -> Result<(), D3d12Error> {
        while !self.pictures.is_empty() {
            self.pump_progress()?;
        }
        Ok(())
    }

    /// Live DPB outputs for `previous` into the ready queue.
    fn drain_previous(&mut self, previous: &Active) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        let outputs = dpb.drain();
        let mut pending = Vec::with_capacity(outputs.len());
        for output in &outputs {
            pending.push((output.slot(), session.start_download(output.slot())?));
        }
        for (slot, ticket) in pending {
            let nv12 = session.finish_download(slot, ticket)?;
            let export = session.shared_handle(slot);
            eprintln!("TRACE drain-push slot={slot}");
            let generation = self.generation;
            self.generation = self.generation.wrapping_add(1);
            self.ready.push_back(emit_frame(
                &self.shared,
                nv12,
                export,
                previous,
                generation,
            )?);
        }
        Ok(())
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(&mut self, active: Active) -> Result<(), D3d12Error> {
        if let Some(previous) = self.active {
            self.drain_previous(&previous)?;
        }
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

    /// Drop the live session, buffer, sequence, and in-flight pictures after a failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.active = None;
        self.pictures.clear();
        self.resets = self.resets.wrapping_add(1);
    }

    /// `error` after dropping the failed stream.
    fn fail(&mut self, error: D3d12Error) -> D3d12Error {
        self.reset_stream();
        error
    }

    /// Peak pipelined pictures outstanding against the current depth.
    fn note_depth(&mut self) {
        self.max_depth = self.max_depth.max(self.outstanding());
    }

    /// Pipelined pictures with incomplete GPU work, freshly polled.
    fn outstanding(&self) -> usize {
        let Some(session) = self.session.as_ref() else {
            return self.pictures.len();
        };
        self.pictures
            .iter()
            .filter(|picture| {
                !session.video_signaled(picture.video)
                    || picture
                        .outputs
                        .iter()
                        .any(|output| !session.copy_signaled(output.copy))
            })
            .count()
    }

    /// Wake blocking receivers past new progress.
    fn signal(&self) {
        let _dropped = self.wake_tx.send(());
    }
}

impl SendPackets for OverlappedH264Pump<'_> {
    type Error = D3d12Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        if !self.device.supports(Codec::H264) {
            return Err(TrySendError::Failed {
                error: SkipReason::ProfileUnsupported { codec: Codec::H264 }.into(),
            });
        }
        if self.pictures.len() >= self.bound {
            return Err(TrySendError::Full { packet });
        }
        let pictures = self
            .ingest
            .ingest_packet(packet.data())
            .map_err(|error| TrySendError::Failed { error })?;
        let ready_len = self.ready.len();
        for picture in &pictures {
            while self.pictures.len() >= self.bound {
                if let Err(error) = self.pump_progress() {
                    self.ready.truncate(ready_len);
                    return Err(TrySendError::Failed {
                        error: self.fail(error),
                    });
                }
            }
            if let Err(error) = self.submit_picture(picture) {
                self.ready.truncate(ready_len);
                self.signal();
                return Err(TrySendError::Failed { error });
            }
        }
        if let Err(error) = self.harvest_poll() {
            self.ready.truncate(ready_len);
            self.signal();
            return Err(TrySendError::Failed {
                error: self.fail(error),
            });
        }
        self.signal();
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if let Some(picture) = self.ingest.finish()? {
            while self.pictures.len() >= self.bound {
                if let Err(error) = self.pump_progress() {
                    return Err(self.fail(error));
                }
            }
            self.submit_picture(&picture)?;
        }
        if let Err(error) = self.drain_pipeline() {
            return Err(self.fail(error));
        }
        if let Some(previous) = self.active {
            self.drain_previous(&previous)?;
        }
        self.signal();
        Ok(())
    }
}

impl PollFrames for OverlappedH264Pump<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.poll_ready()
    }
}

impl ReceiveFrames for OverlappedH264Pump<'_> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        loop {
            let next = self.poll_ready().map_err(|error| self.fail(error))?;
            if let Some(frame) = next {
                return Ok(Some(frame));
            }
            if self.pictures.is_empty() {
                if self.finished {
                    return Ok(None);
                }
                let _woke = self.wake_rx.recv();
            } else {
                self.pump_progress().map_err(|error| self.fail(error))?;
            }
        }
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let next = self.poll_ready().map_err(|error| ReceiveError::Failed {
                error: self.fail(error),
            })?;
            if let Some(frame) = next {
                return Ok(Some(frame));
            }
            if self.pictures.is_empty() {
                if self.finished {
                    return Ok(None);
                }
                let remaining = deadline.map(|at| at.saturating_duration_since(Instant::now()));
                let arrived = match remaining {
                    None => self.wake_rx.recv().is_ok(),
                    Some(budget) => self.wake_rx.recv_timeout(budget).is_ok(),
                };
                if !arrived {
                    return Err(ReceiveError::Timeout);
                }
            } else if !self
                .wait_front_before(deadline)
                .map_err(|error| ReceiveError::Failed {
                    error: self.fail(error),
                })?
            {
                return Err(ReceiveError::Timeout);
            }
        }
    }
}

/// Queued download behind one display-order output.
struct PendingOutput {
    /// Pool pin behind the output slot, steering the buffer past it.
    pin: SlotPin,
    /// Copy queue ticket behind the download.
    copy: Ticket,
    /// Copy submit time.
    copy_submit: Instant,
    /// Sequence geometry behind the frame.
    active: Active,
    /// Frame generation number.
    generation: u64,
    /// Video submit time behind the picture.
    video_submit: Instant,
    /// CPU time behind the video submit.
    submit_cpu: Duration,
}

/// Phase-split timing behind one overlapped frame.
#[derive(Clone, Copy, Debug)]
pub struct PhaseTiming {
    /// Frame generation number.
    generation: u64,
    /// CPU microseconds behind the video submit.
    submit_us: u64,
    /// Video submit time.
    video_submit: Instant,
    /// Video signal time, polled.
    video_signaled: Instant,
    /// Copy submit time.
    copy_submit: Instant,
    /// Copy signal time, polled.
    copy_signaled: Instant,
    /// Frame harvest time.
    harvested: Instant,
}

impl PhaseTiming {
    /// Timing behind `output` with observed signal times.
    fn new(
        output: &PendingOutput,
        video_signaled: Instant,
        copy_signaled: Instant,
        harvested: Instant,
    ) -> Self {
        Self {
            generation: output.generation,
            submit_us: Self::micros(output.submit_cpu),
            video_submit: output.video_submit,
            video_signaled,
            copy_submit: output.copy_submit,
            copy_signaled,
            harvested,
        }
    }

    /// Whole microseconds behind `duration`, saturating.
    fn micros(duration: Duration) -> u64 {
        u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
    }

    /// Frame generation number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// CPU microseconds behind the video submit.
    #[must_use]
    pub fn submit_us(&self) -> u64 {
        self.submit_us
    }

    /// Video submit time.
    #[must_use]
    pub fn video_submit(&self) -> Instant {
        self.video_submit
    }

    /// Video signal time, polled.
    #[must_use]
    pub fn video_signaled(&self) -> Instant {
        self.video_signaled
    }

    /// Copy submit time.
    #[must_use]
    pub fn copy_submit(&self) -> Instant {
        self.copy_submit
    }

    /// Copy signal time, polled.
    #[must_use]
    pub fn copy_signaled(&self) -> Instant {
        self.copy_signaled
    }

    /// Frame harvest time.
    #[must_use]
    pub fn harvested(&self) -> Instant {
        self.harvested
    }

    /// Engine microseconds, video submit to signal.
    #[must_use]
    pub fn engine_us(&self) -> u64 {
        Self::micros(
            self.video_signaled
                .saturating_duration_since(self.video_submit),
        )
    }

    /// Copy microseconds, copy submit to signal.
    #[must_use]
    pub fn copy_us(&self) -> u64 {
        Self::micros(
            self.copy_signaled
                .saturating_duration_since(self.copy_submit),
        )
    }

    /// Frame latency microseconds, video submit to harvest.
    #[must_use]
    pub fn latency_us(&self) -> u64 {
        Self::micros(self.harvested.saturating_duration_since(self.video_submit))
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

/// Frame for NV12 `nv12` at `generation`.
fn emit_frame(
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
fn open_sequence(
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
fn sequence_of(ingest: &Ingest, picture: &Picture) -> Result<Active, D3d12Error> {
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
fn submit_dpb(ingest: &Ingest, dpb: &mut Dpb, picture: &Picture) -> Result<Decoded, D3d12Error> {
    let pps = ingest.pps(picture.pps_id()).ok_or(D3d12Error::NoSession)?;
    let sps = ingest
        .sps(pps.seq_parameter_set_id)
        .ok_or(D3d12Error::NoSession)?;
    dpb.decode(picture, sps, pps)
}
