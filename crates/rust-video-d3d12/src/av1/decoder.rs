//! AV1 decode over a live video session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::dpb::{Av1Decoded, Av1Dpb};
use super::frontend::{Frame as Av1Frame, Ingest as Av1Ingest};
use crate::decoder::{
    emit_frame, session_surface_export, surface_layout, visible_nv12, Active, SyncPending,
};
use crate::device::D3d12Device;
use crate::error::{D3d12Error, ParseReason, SkipReason, UnsupportedReason};
use crate::frame::D3d12Frame;
use crate::guid::Codec;
use crate::h264::frontend::Crop;
use crate::pins::SlotPins;
use crate::session::{DecodeQueues, DecodeSession};
use crate::texture::GpuSurface;

use rust_video_caps::common::PictureSize;
use rust_video_caps::delivery::{PhaseTiming, SubmitStats};
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::{ParseReason as SharedParse, UnsupportedReason as SharedUnsupported};
use rust_video_caps::traits::{Decoder, SharedDevice};
use rust_video_caps::transcode::GpuCaps;
use windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue;

/// Submitted surfaces held back across surface calls.
const SURFACE_HOLD: usize = 2;

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
    /// Phase-split timing per submitted surface picture, oldest first.
    surface_timings: Vec<PhaseTiming>,
    /// Phase-split timing per sync output, oldest first.
    sync_timings: Vec<PhaseTiming>,
    /// Staging map plus tighten per sync output, oldest first.
    sync_readback: Vec<Duration>,
    /// Export plus frame wrap per sync output, oldest first.
    sync_emit: Vec<Duration>,
    /// Submitted surfaces held back two packets for submit/wait overlap.
    held_surfaces: Vec<GpuSurface>,
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
            surface_timings: Vec::new(),
            sync_timings: Vec::new(),
            sync_readback: Vec::new(),
            sync_emit: Vec::new(),
            held_surfaces: Vec::new(),
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

    /// Submit gauges since decoder open.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        let mut stats = SubmitStats::new();
        stats.add_resets(self.resets());
        stats.fold_peak(self.max_inflight());
        stats
    }

    /// Phase-split timing per submitted surface picture, oldest first.
    #[must_use]
    pub fn surface_timings(&self) -> &[PhaseTiming] {
        &self.surface_timings
    }

    /// Phase-split timing per sync output, oldest first.
    #[must_use]
    pub fn sync_timings(&self) -> &[PhaseTiming] {
        &self.sync_timings
    }

    /// Staging map plus tighten per sync output, oldest first.
    #[must_use]
    pub fn sync_readback(&self) -> &[Duration] {
        &self.sync_readback
    }

    /// Export plus frame wrap per sync output, oldest first.
    #[must_use]
    pub fn sync_emit(&self) -> &[Duration] {
        &self.sync_emit
    }

    /// Decode `frame`, queueing its outputs for readback into `pending`.
    fn submit_frame(
        &mut self,
        frame: &Av1Frame,
        pending: &mut Vec<SyncPending>,
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        let submitted_at = Instant::now();
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
        if let Err(error) = self.submit_downloads(&decoded, active, submitted_at, pending) {
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
        submitted_at: Instant,
        pending: &mut Vec<SyncPending>,
    ) -> Result<(), D3d12Error> {
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Downloads queue behind the video ticket: the copy runs at
        // video-signal without a CPU wakeup gap, and the record hides
        // under the engine wait. The wait still serializes submits.
        if let Some(submission) = decoded.submission.as_ref() {
            let video = session.submit_av1(submission)?;
            let mut tickets = Vec::with_capacity(decoded.outputs.len());
            for output in &decoded.outputs {
                let ticket = session.start_download_after(output.slot(), video)?;
                tickets.push((output.slot(), ticket));
            }
            let recorded = Instant::now();
            session.wait_video(video)?;
            let signaled = Instant::now();
            pending.extend(tickets.into_iter().map(|(slot, ticket)| SyncPending {
                slot,
                ticket,
                active,
                submitted_at,
                submit: recorded.duration_since(submitted_at),
                engine: signaled.duration_since(recorded),
            }));
        } else {
            let mut tickets = Vec::with_capacity(decoded.outputs.len());
            for output in &decoded.outputs {
                let ticket = session.start_download(output.slot())?;
                tickets.push((output.slot(), ticket));
            }
            let recorded = Instant::now();
            pending.extend(tickets.into_iter().map(|(slot, ticket)| SyncPending {
                slot,
                ticket,
                active,
                submitted_at,
                submit: recorded.duration_since(submitted_at),
                engine: Duration::ZERO,
            }));
        }
        Ok(())
    }

    /// Readback for queued `pending` outputs into `frames`.
    fn consume_pending(
        &mut self,
        pending: &[SyncPending],
        frames: &mut Vec<D3d12Frame>,
    ) -> Result<(), D3d12Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        for output in pending {
            let (waited, read, nv12) = session.finish_download_split(output.slot, output.ticket)?;
            let shaped_at = Instant::now();
            let nv12 = visible_nv12(nv12, &output.active)?;
            let shaped = shaped_at.elapsed();
            let emit_at = Instant::now();
            let export = session.shared_handle(output.slot);
            let frame = emit_frame(&self.shared, nv12, export, &output.active, self.generation)?;
            let collected_at = Instant::now();
            self.sync_timings.push(PhaseTiming::new(
                self.generation,
                output.submit,
                output.engine,
                waited,
                output.submitted_at,
                collected_at,
            ));
            self.sync_readback.push(read + shaped);
            self.sync_emit.push(collected_at.duration_since(emit_at));
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
        self.held_surfaces.clear();
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
        let queues = self.session.take().map(|session| session.queues());
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
        let submitted_at = Instant::now();
        let session = self.session.as_mut().ok_or(D3d12Error::NoSession)?;
        // Submit before any surface handoff: outputs may name the submission slot.
        let ticket = match decoded.submission.as_ref() {
            Some(submission) => Some(session.submit_av1(submission)?),
            None => None,
        };
        let submitted = Instant::now();
        let submit = submitted.duration_since(submitted_at);
        // Handoffs carry the fence and the release waits instead.
        let engine_wait = Duration::ZERO;
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
        if ticket.is_some() {
            self.record_surface_timing(submitted_at, submit, engine_wait, Duration::ZERO);
        }
        Ok(())
    }

    /// Surface phases from `submitted_at` into the timing log.
    fn record_surface_timing(
        &mut self,
        submitted_at: Instant,
        submit: Duration,
        engine_wait: Duration,
        copy: Duration,
    ) {
        self.surface_timings.push(PhaseTiming::new(
            self.generation,
            submit,
            engine_wait,
            copy,
            submitted_at,
            Instant::now(),
        ));
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
        let mut fresh = Vec::new();
        for frame in &frames_in {
            self.texture_frame(frame, &mut fresh)?;
        }
        // Two packets held back: older calls' surfaces return
        // complete while the new submits' engine work runs. Held
        // pins steer the next submit past their slots, so the
        // release wait below is the only per-call engine stall.
        self.held_surfaces.append(&mut fresh);
        let mut surfaces = Vec::new();
        while self.held_surfaces.len() > SURFACE_HOLD {
            let oldest = self.held_surfaces.remove(0);
            oldest.try_wait()?;
            surfaces.push(oldest);
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d12Error> {
        let mut fresh = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            self.texture_frame(&frame, &mut fresh)?;
        }
        if let Some(active) = self.active {
            self.drain_live_texture(&active, &mut fresh)?;
        }
        self.held_surfaces.append(&mut fresh);
        Ok(std::mem::take(&mut self.held_surfaces))
    }
}

impl Decoder for Av1Decoder<'_> {
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
        return Err(D3d12Error::Parse(ParseReason::Parse(
            SharedParse::FrameSizeAboveSequenceMaximum,
        )));
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(D3d12Error::Unsupported(UnsupportedReason::Stream(
            SharedUnsupported::OddNv12Extent,
        )));
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
    let visible = PictureSize::new(header.frame_width, header.frame_height).ok_or(
        D3d12Error::Parse(ParseReason::Parse(SharedParse::CropEmptiesPicture)),
    )?;
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
