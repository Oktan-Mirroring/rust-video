//! Overlapped decode: pipelined multi-inflight submits behind the async traits.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::av1::decoder::{frame_active_av1, open_sequence_av1, submit_av1_dpb};
use crate::av1::dpb::Av1Dpb;
use crate::av1::frontend::{Frame as Av1Frame, Ingest as Av1Ingest};
use crate::av1::picture::Av1Submission;
use crate::decode::{DecodeQueues, DecodeSession, Output};
use crate::decoder::D3d12Frame;
use crate::decoder_os::{emit_frame, visible_nv12, Active};
use crate::device::D3d12Device;
use crate::error::{D3d12Error, SkipReason};
use crate::guid::Codec;
use crate::h264::decoder::{open_sequence, sequence_of, submit_dpb};
use crate::h264::dpb::Dpb;
use crate::h264::frontend::{Ingest, Picture};
use crate::h264::picture::H264Submission;
use crate::h265::decoder::{open_sequence_hevc, sequence_of_hevc, submit_hevc_dpb};
use crate::h265::dpb::HevcDpb;
use crate::h265::frontend::{Ingest as HevcIngest, Picture as HevcPicture};
use crate::h265::picture::HevcSubmission;
use crate::pins::{SlotPin, SlotPins};
use crate::readback::{Done, Readback};
use crate::submit::Ticket;

use rust_video_caps::delivery::{
    PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::SharedDevice;

/// Codec front behind the overlapped pump: ingest, slots, submit.
pub(crate) trait OverlapFront {
    /// Compressed picture or frame.
    type Picture;
    /// Stream ingest: sets plus picture assembly.
    type Ingest;
    /// Live picture buffer or reference slots.
    type Slots;
    /// Driver submission behind one decode call.
    type Submission;
    /// Codec behind profile probes.
    const CODEC: Codec;
    /// Fresh ingest.
    fn ingest() -> Self::Ingest;
    /// Pictures behind `data`.
    fn ingest_packet(
        ingest: &mut Self::Ingest,
        data: &[u8],
    ) -> Result<Vec<Self::Picture>, D3d12Error>;
    /// Trailing picture past input end.
    fn ingest_finish(ingest: &mut Self::Ingest) -> Result<Option<Self::Picture>, D3d12Error>;
    /// Ingest state past a stream reset.
    fn ingest_reset(_ingest: &mut Self::Ingest) {}
    /// Active sequence behind `picture`.
    fn sequence(
        ingest: &Self::Ingest,
        picture: &Self::Picture,
        current: Option<Active>,
    ) -> Result<Active, D3d12Error>;
    /// True when `next` needs a fresh session past `current`.
    fn needs_reconfigure(current: Option<Active>, next: Active) -> bool {
        current != Some(next)
    }
    /// Session plus slots for `next`, reusing submit `queues`.
    fn open(
        device: &D3d12Device,
        ingest: &Self::Ingest,
        pins: Arc<SlotPins>,
        queues: Option<DecodeQueues>,
        next: Active,
    ) -> Result<(DecodeSession, Self::Slots), D3d12Error>;
    /// Submission plus display-order outputs for `picture`.
    fn prepare(
        ingest: &Self::Ingest,
        slots: &mut Self::Slots,
        picture: &Self::Picture,
    ) -> Result<Prepared<Self::Submission>, D3d12Error>;
    /// Live slot outputs past input end.
    fn drain(slots: &mut Self::Slots) -> Vec<Output>;
    /// `submission` through the video queue into its pool slot.
    fn submit_video(
        session: &mut DecodeSession,
        submission: &Self::Submission,
    ) -> Result<Ticket, D3d12Error>;
    /// Readback bytes behind `active`, cropped per output when the session runs uncropped.
    fn finish_nv12(nv12: Vec<u8>, active: &Active) -> Result<Vec<u8>, D3d12Error> {
        let _ = active;
        Ok(nv12)
    }
}

/// AV1 front behind the overlapped pump.
pub struct Av1Front;

impl OverlapFront for Av1Front {
    type Picture = Av1Frame;
    type Ingest = Av1Ingest;
    type Slots = Av1Dpb;
    type Submission = Av1Submission;
    const CODEC: Codec = Codec::Av1;

    fn ingest() -> Av1Ingest {
        Av1Ingest::new()
    }

    fn ingest_packet(ingest: &mut Av1Ingest, data: &[u8]) -> Result<Vec<Av1Frame>, D3d12Error> {
        ingest.ingest_packet(data)
    }

    fn ingest_finish(ingest: &mut Av1Ingest) -> Result<Option<Av1Frame>, D3d12Error> {
        ingest.finish()
    }

    fn ingest_reset(ingest: &mut Av1Ingest) {
        ingest.reset_refs();
    }

    fn sequence(
        ingest: &Av1Ingest,
        frame: &Av1Frame,
        current: Option<Active>,
    ) -> Result<Active, D3d12Error> {
        frame_active_av1(ingest, frame, current)
    }

    fn needs_reconfigure(current: Option<Active>, next: Active) -> bool {
        current.is_none_or(|live| !live.same_extent(next))
    }

    fn open(
        device: &D3d12Device,
        ingest: &Av1Ingest,
        pins: Arc<SlotPins>,
        queues: Option<DecodeQueues>,
        next: Active,
    ) -> Result<(DecodeSession, Av1Dpb), D3d12Error> {
        open_sequence_av1(device, ingest, pins, queues, next)
    }

    fn prepare(
        ingest: &Av1Ingest,
        slots: &mut Av1Dpb,
        frame: &Av1Frame,
    ) -> Result<Prepared<Av1Submission>, D3d12Error> {
        let decoded = submit_av1_dpb(ingest, slots, frame)?;
        let submission = decoded.submission.map(|submission| {
            let slot = submission.slot;
            (submission, slot)
        });
        Ok(Prepared {
            submission,
            outputs: decoded.outputs,
        })
    }

    fn drain(_slots: &mut Av1Dpb) -> Vec<Output> {
        Av1Dpb::drain()
    }

    fn submit_video(
        session: &mut DecodeSession,
        submission: &Av1Submission,
    ) -> Result<Ticket, D3d12Error> {
        session.submit_av1(submission)
    }

    fn finish_nv12(nv12: Vec<u8>, active: &Active) -> Result<Vec<u8>, D3d12Error> {
        visible_nv12(nv12, active)
    }
}

/// H.264 front behind the overlapped pump.
pub struct H264Front;

impl OverlapFront for H264Front {
    type Picture = Picture;
    type Ingest = Ingest;
    type Slots = Dpb;
    type Submission = H264Submission;
    const CODEC: Codec = Codec::H264;

    fn ingest() -> Ingest {
        Ingest::new()
    }

    fn ingest_packet(ingest: &mut Ingest, data: &[u8]) -> Result<Vec<Picture>, D3d12Error> {
        ingest.ingest_packet(data)
    }

    fn ingest_finish(ingest: &mut Ingest) -> Result<Option<Picture>, D3d12Error> {
        ingest.finish()
    }

    fn sequence(
        ingest: &Ingest,
        picture: &Picture,
        _current: Option<Active>,
    ) -> Result<Active, D3d12Error> {
        sequence_of(ingest, picture)
    }

    fn open(
        device: &D3d12Device,
        ingest: &Ingest,
        pins: Arc<SlotPins>,
        queues: Option<DecodeQueues>,
        next: Active,
    ) -> Result<(DecodeSession, Dpb), D3d12Error> {
        open_sequence(device, ingest, pins, queues, next)
    }

    fn prepare(
        ingest: &Ingest,
        slots: &mut Dpb,
        picture: &Picture,
    ) -> Result<Prepared<H264Submission>, D3d12Error> {
        let decoded = submit_dpb(ingest, slots, picture)?;
        let slot = decoded.submission.slot;
        Ok(Prepared {
            submission: Some((decoded.submission, slot)),
            outputs: decoded.outputs,
        })
    }

    fn drain(slots: &mut Dpb) -> Vec<Output> {
        slots.drain()
    }

    fn submit_video(
        session: &mut DecodeSession,
        submission: &H264Submission,
    ) -> Result<Ticket, D3d12Error> {
        session.submit(submission)
    }
}

/// HEVC front behind the overlapped pump.
pub struct HevcFront;

impl OverlapFront for HevcFront {
    type Picture = HevcPicture;
    type Ingest = HevcIngest;
    type Slots = HevcDpb;
    type Submission = HevcSubmission;
    const CODEC: Codec = Codec::Hevc;

    fn ingest() -> HevcIngest {
        HevcIngest::new()
    }

    fn ingest_packet(ingest: &mut HevcIngest, data: &[u8]) -> Result<Vec<HevcPicture>, D3d12Error> {
        ingest.ingest_packet(data)
    }

    fn ingest_finish(ingest: &mut HevcIngest) -> Result<Option<HevcPicture>, D3d12Error> {
        ingest.finish()
    }

    fn sequence(
        ingest: &HevcIngest,
        picture: &HevcPicture,
        _current: Option<Active>,
    ) -> Result<Active, D3d12Error> {
        sequence_of_hevc(ingest, picture)
    }

    fn open(
        device: &D3d12Device,
        ingest: &HevcIngest,
        pins: Arc<SlotPins>,
        queues: Option<DecodeQueues>,
        next: Active,
    ) -> Result<(DecodeSession, HevcDpb), D3d12Error> {
        open_sequence_hevc(device, ingest, pins, queues, next)
    }

    fn prepare(
        ingest: &HevcIngest,
        slots: &mut HevcDpb,
        picture: &HevcPicture,
    ) -> Result<Prepared<HevcSubmission>, D3d12Error> {
        let decoded = submit_hevc_dpb(ingest, slots, picture)?;
        let slot = decoded.submission.slot;
        Ok(Prepared {
            submission: Some((decoded.submission, slot)),
            outputs: decoded.outputs,
        })
    }

    fn drain(slots: &mut HevcDpb) -> Vec<Output> {
        slots.drain()
    }

    fn submit_video(
        session: &mut DecodeSession,
        submission: &HevcSubmission,
    ) -> Result<Ticket, D3d12Error> {
        session.submit_hevc(submission)
    }
}

/// In-flight picture: submitted video plus queued downloads.
struct InflightPicture<S> {
    /// Decode arguments behind the submit, dropped past the video ticket.
    submission: Option<Box<S>>,
    /// Pool pin behind the submission slot, steering the buffer past it.
    pin: SlotPin,
    /// Video queue ticket behind the decode.
    video: Ticket,
    /// Video signal time, stamped on poll.
    video_signaled: Option<Instant>,
    /// Queued downloads in output order.
    outputs: VecDeque<PendingOutput>,
}

/// Decode with pipelined multi-inflight submits behind the async traits.
///
/// Submits video work without blocking on completion. Downloads ride
/// the copy queue behind GPU waits; copy N runs beside decode N+1.
/// Completions surface in order through poll or blocking receive.
// Sealed per codec: only the three concrete pumps name a front.
#[allow(private_bounds)]
pub struct OverlapPump<'a, F: OverlapFront> {
    /// Device owning the decode session.
    device: &'a D3d12Device,
    /// Stream ingest behind picture assembly.
    ingest: F::Ingest,
    /// Live picture buffer, created from the first sequence.
    dpb: Option<F::Slots>,
    /// Live decode session, created from the first sequence.
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
    pictures: VecDeque<InflightPicture<F::Submission>>,
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
    /// Copy worker behind launches.
    readback: Readback,
}

#[allow(private_bounds)]
impl<F: OverlapFront> Drop for OverlapPump<'_, F> {
    fn drop(&mut self) {
        self.readback.shutdown();
    }
}

#[allow(private_bounds)]
impl<'a, F: OverlapFront> OverlapPump<'a, F> {
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
            ingest: F::ingest(),
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
            readback: Readback::spawn(),
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
    fn submit_picture(&mut self, picture: &F::Picture) -> Result<(), D3d12Error> {
        let active = F::sequence(&self.ingest, picture, self.active)?;
        if F::needs_reconfigure(self.active, active) {
            self.drain_pipeline()?;
            self.reconfigure(active)?;
        } else if self.active != Some(active) {
            self.active = Some(active);
        }
        let dpb = self.dpb.as_mut().ok_or(D3d12Error::NoSession)?;
        let prepared = match F::prepare(&self.ingest, dpb, picture) {
            Ok(prepared) => prepared,
            Err(error) => {
                if !matches!(error, D3d12Error::NoFreeSlot) {
                    self.reset_stream();
                }
                return Err(error);
            }
        };
        if let Some((_, slot)) = prepared.submission.as_ref() {
            if let Err(error) = self.quiesce_slot(*slot) {
                self.reset_stream();
                return Err(error);
            }
        }
        if let Err(error) = self.submit_tracked(active, prepared) {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// `prepared` submission through the video queue plus queued downloads, tracked in flight.
    fn submit_tracked(
        &mut self,
        active: Active,
        prepared: Prepared<F::Submission>,
    ) -> Result<(), D3d12Error> {
        let Some((submission, slot)) = prepared.submission else {
            return self.submit_synced(active, &prepared.outputs);
        };
        // Boxed before submit: the recorded pointers name stable heap addresses.
        let submission = Box::new(submission);
        let pin = self.pins.pin(slot);
        let Some(session) = self.session.as_mut() else {
            return Err(D3d12Error::NoSession);
        };
        let video_submit = Instant::now();
        let ticket = F::submit_video(session, &submission)?;
        let submit_cpu = video_submit.elapsed();
        let mut pending = VecDeque::with_capacity(prepared.outputs.len());
        for output in &prepared.outputs {
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
                launched: false,
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

    /// Display-only `outputs` over settled slots into the ready queue.
    fn submit_synced(&mut self, active: Active, outputs: &[Output]) -> Result<(), D3d12Error> {
        self.drain_pipeline()?;
        let Some(session) = self.session.as_mut() else {
            return Err(D3d12Error::NoSession);
        };
        let mut pending = Vec::with_capacity(outputs.len());
        for output in outputs {
            pending.push((output.slot(), session.start_download(output.slot())?));
        }
        for (slot, ticket) in pending {
            let nv12 = session.finish_download(slot, ticket)?;
            let nv12 = F::finish_nv12(nv12, &active)?;
            let export = session.shared_handle(slot);
            let generation = self.generation;
            self.generation = self.generation.wrapping_add(1);
            self.ready
                .push_back(emit_frame(&self.shared, nv12, export, &active, generation)?);
        }
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

    /// Oldest output into the ready queue when its copy landed.
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
        if let Some(done) = self.readback.harvest() {
            self.land_front(done)?;
            return Ok(true);
        }
        let launch = match self
            .pictures
            .front()
            .and_then(|front| front.outputs.front())
        {
            Some(output) if !output.launched => Some((output.pin.slot(), output.copy)),
            _ => None,
        };
        let Some((slot, copy)) = launch else {
            return Ok(false);
        };
        if !session.copy_signaled(copy) {
            return Ok(false);
        }
        let mapped = session.map_slot(slot)?;
        if let Some(output) = self
            .pictures
            .front_mut()
            .and_then(|front| front.outputs.front_mut())
        {
            output.launched = true;
        }
        if let Some(done) = self.readback.launch(mapped) {
            self.land_front(done)?;
        }
        Ok(true)
    }

    /// Front output plus `done` into the ready queue.
    fn land_front(&mut self, done: Done) -> Result<(), D3d12Error> {
        let (slot, bytes, release, _copy) = done.into_parts();
        let output = self
            .pictures
            .front_mut()
            .and_then(|front| front.outputs.pop_front());
        let Some(output) = output else {
            debug_assert!(false, "landed copy without pending output");
            return Ok(());
        };
        debug_assert_eq!(slot, output.pin.slot());
        release.unmap();
        let signaled = Instant::now();
        let video = self
            .pictures
            .front()
            .and_then(|front| front.video_signaled)
            .unwrap_or(signaled);
        self.timings.push(PhaseTiming::new(
            output.generation,
            output.submit_cpu,
            video.saturating_duration_since(output.video_submit),
            signaled.saturating_duration_since(output.copy_submit),
            output.video_submit,
            signaled,
        ));
        let export = self
            .session
            .as_ref()
            .and_then(|session| session.shared_handle(output.pin.slot()));
        let frame = emit_frame(
            &self.shared,
            F::finish_nv12(bytes?, &output.active)?,
            export,
            &output.active,
            output.generation,
        )?;
        self.ready.push_back(frame);
        Ok(())
    }

    /// Oldest launched copy into ready, waiting for it.
    fn collect_front(&mut self) -> Result<(), D3d12Error> {
        let launched = matches!(
            self.pictures
                .front()
                .and_then(|front| front.outputs.front()),
            Some(output) if output.launched
        );
        if !launched {
            return Ok(());
        }
        match self.readback.collect() {
            Some(done) => self.land_front(done),
            None => self.rescue_front(),
        }
    }

    /// Front output read synchronously, rescuing a lost worker.
    fn rescue_front(&mut self) -> Result<(), D3d12Error> {
        let Some(session) = self.session.as_ref() else {
            return Err(D3d12Error::NoSession);
        };
        let output = self
            .pictures
            .front_mut()
            .and_then(|front| front.outputs.pop_front());
        let Some(output) = output else {
            return Ok(());
        };
        let nv12 = session.finish_download(output.pin.slot(), output.copy)?;
        let nv12 = F::finish_nv12(nv12, &output.active)?;
        let export = session.shared_handle(output.pin.slot());
        let signaled = Instant::now();
        let video = self
            .pictures
            .front()
            .and_then(|front| front.video_signaled)
            .unwrap_or(signaled);
        self.timings.push(PhaseTiming::new(
            output.generation,
            output.submit_cpu,
            video.saturating_duration_since(output.video_submit),
            signaled.saturating_duration_since(output.copy_submit),
            output.video_submit,
            signaled,
        ));
        let frame = emit_frame(
            &self.shared,
            nv12,
            export,
            &output.active,
            output.generation,
        )?;
        self.ready.push_back(frame);
        Ok(())
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
        let launched = matches!(
            self.pictures
                .front()
                .and_then(|front| front.outputs.front()),
            Some(output) if output.launched
        );
        if launched {
            self.collect_front()?;
        } else {
            self.wait_front()?;
        }
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

    /// Live slot outputs for `previous` into the ready queue.
    fn drain_previous(&mut self, previous: &Active) -> Result<(), D3d12Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_mut()) else {
            return Ok(());
        };
        let outputs = F::drain(dpb);
        let mut pending = Vec::with_capacity(outputs.len());
        for output in &outputs {
            pending.push((output.slot(), session.start_download(output.slot())?));
        }
        for (slot, ticket) in pending {
            let nv12 = session.finish_download(slot, ticket)?;
            let nv12 = F::finish_nv12(nv12, previous)?;
            let export = session.shared_handle(slot);
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
        let (session, dpb) = F::open(self.device, &self.ingest, Arc::clone(&pins), queues, active)?;
        self.session = Some(session);
        self.pins = pins;
        self.dpb = Some(dpb);
        self.active = Some(active);
        Ok(())
    }

    /// Drop the live session, buffer, sequence, and in-flight pictures after a failure.
    fn reset_stream(&mut self) {
        let mut launched = self
            .pictures
            .iter()
            .flat_map(|picture| picture.outputs.iter())
            .filter(|output| output.launched)
            .count();
        while launched > 0 {
            launched -= 1;
            match self.readback.collect() {
                Some(done) => {
                    let (_, _, release, _) = done.into_parts();
                    release.unmap();
                }
                None => break,
            }
        }
        self.dpb = None;
        self.session = None;
        self.active = None;
        F::ingest_reset(&mut self.ingest);
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

#[allow(private_bounds)]
impl<F: OverlapFront> SendPackets for OverlapPump<'_, F> {
    type Error = D3d12Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        if !self.device.supports(F::CODEC) {
            return Err(TrySendError::Failed {
                error: SkipReason::ProfileUnsupported { codec: F::CODEC }.into(),
            });
        }
        if self.pictures.len() >= self.bound {
            return Err(TrySendError::Full { packet });
        }
        let pictures = F::ingest_packet(&mut self.ingest, packet.data())
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
        if let Some(picture) = F::ingest_finish(&mut self.ingest)? {
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

#[allow(private_bounds)]
impl<F: OverlapFront> PollFrames for OverlapPump<'_, F> {
    type Frame = D3d12Frame;
    type Error = D3d12Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.poll_ready()
    }
}

#[allow(private_bounds)]
impl<F: OverlapFront> ReceiveFrames for OverlapPump<'_, F> {
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

/// AV1 decode with pipelined multi-inflight submits.
pub type OverlappedAv1Pump<'a> = OverlapPump<'a, Av1Front>;

/// H.264 decode with pipelined multi-inflight submits.
pub type OverlappedH264Pump<'a> = OverlapPump<'a, H264Front>;

/// HEVC decode with pipelined multi-inflight submits.
pub type OverlappedHevcPump<'a> = OverlapPump<'a, HevcFront>;

/// Queued download behind one display-order output.
struct PendingOutput {
    /// Pool pin behind the output slot, steering the buffer past it.
    pin: SlotPin,
    /// Copy queue ticket behind the download.
    copy: Ticket,
    /// Copy launched on the worker.
    launched: bool,
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

/// Submission plus display-order outputs behind one picture.
pub(crate) struct Prepared<S> {
    /// Driver submission plus its pool slot, absent for existing-frame display.
    submission: Option<(S, u8)>,
    /// Display-order outputs.
    outputs: Vec<Output>,
}
