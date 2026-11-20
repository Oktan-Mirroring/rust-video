//! Multi-inflight overlapped decode behind the frozen async traits.
//!
//! Sends submit without waiting on completion; receives poll per-frame
//! fences and hand completed frames off oldest-first.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::decoder::emit_frame;
use crate::decoder::emit_frame_bytes;
use crate::decoder::Active;
use crate::error::D3d11Error;
use crate::frame::D3d11Frame;
use crate::readback::Readback;
use crate::session::Session;

use rust_video_caps::delivery::{
    PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets, SplitDecode, TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::SharedDevice;

/// Sends admitted while fewer outputs are in flight; [`FrameQueue::admits`]
/// enforces it, [`OverlappedPump::MAX_INFLIGHT`] publishes it.
pub(crate) const INFLIGHT_DEPTH: usize = 4;

/// Pending outputs before oldest-first blocking drains; decoders enforce
/// it in their submit path next to the lane-collision check.
pub(crate) const INFLIGHT_CEILING: usize = 8;

/// Split-phase gauges behind the overlapped pump.
pub(crate) trait PumpGauges {
    /// Admitted outputs not yet completed.
    fn depth(&self) -> usize;

    /// Peak admitted outputs not yet completed.
    fn max_depth(&self) -> usize;

    /// True once input ended.
    fn is_finished(&self) -> bool;

    /// Phase-split timing per collected frame, oldest first.
    fn pump_timings(&self) -> &[PhaseTiming];
}

/// Submitted outputs plus completed frames awaiting poll.
#[derive(Debug)]
pub(crate) struct FrameQueue<F> {
    /// Submitted outputs not yet completed, oldest first.
    pending: VecDeque<PendingOutput>,
    /// Completed frames not yet polled, oldest first.
    ready: VecDeque<F>,
    /// Peak pending count.
    max_observed: usize,
    /// Admitted outputs before sends refuse.
    depth: usize,
}

impl<F> FrameQueue<F> {
    /// Empty queue.
    pub(crate) fn new() -> Self {
        Self::with_depth(INFLIGHT_DEPTH)
    }

    /// Empty queue admitting at most `depth` outputs.
    ///
    /// # Arguments
    ///
    /// * `depth` - admitted outputs before sends refuse; clamped to `1..=INFLIGHT_CEILING`.
    pub(crate) fn with_depth(depth: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            max_observed: 0,
            depth: depth.clamp(1, INFLIGHT_CEILING),
        }
    }

    /// Submitted outputs not yet completed.
    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    /// True when no output is pending and no frame is ready.
    pub(crate) fn is_drained(&self) -> bool {
        self.pending.is_empty() && self.ready.is_empty()
    }

    /// Peak pending count.
    pub(crate) fn max_observed(&self) -> usize {
        self.max_observed
    }

    /// Peak pending count against `depth`.
    pub(crate) fn observe(&mut self, depth: usize) {
        self.max_observed = self.max_observed.max(depth);
    }

    /// True while another send fits under the in-flight bound.
    pub(crate) fn admits(&self) -> bool {
        self.pending.len() < self.depth
    }

    /// Oldest pending output, if any.
    pub(crate) fn front(&self) -> Option<&PendingOutput> {
        self.pending.front()
    }

    /// Oldest pending output, mutably.
    pub(crate) fn front_mut(&mut self) -> Option<&mut PendingOutput> {
        self.pending.front_mut()
    }

    /// Output submitted under `active` across the submit span, read via `lane`.
    pub(crate) fn push(
        &mut self,
        lane: u8,
        active: Active,
        submit_start: Instant,
        submit_end: Instant,
    ) {
        self.pending
            .push_back(PendingOutput::new(lane, active, submit_start, submit_end));
        self.observe(self.pending.len());
    }

    /// Completed `frame` awaiting poll.
    pub(crate) fn push_ready(&mut self, frame: F) {
        self.ready.push_back(frame);
    }

    /// Completed `frames` awaiting poll, oldest first.
    pub(crate) fn extend_ready(&mut self, frames: Vec<F>) {
        for frame in frames {
            self.push_ready(frame);
        }
    }

    /// Oldest pending output, if any.
    pub(crate) fn pop_pending(&mut self) -> Option<PendingOutput> {
        self.pending.pop_front()
    }

    /// Oldest completed frame, if any.
    pub(crate) fn pop_ready(&mut self) -> Option<F> {
        self.ready.pop_front()
    }

    /// Pending outputs dropped; ready frames kept.
    pub(crate) fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Pending outputs and ready frames dropped.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.ready.clear();
    }
}

impl<F> Default for FrameQueue<F> {
    fn default() -> Self {
        Self::new()
    }
}

/// Split-phase decode over one multi-inflight decoder.
///
/// Sends submit without waiting; receives hand completed frames off
/// oldest-first. Never drops: sends past [`MAX_INFLIGHT`](Self::MAX_INFLIGHT)
/// in flight refuse with the packet back.
///
/// Not a channel pump or a memcpy pump:
/// [`DecoderPump`](rust_video_caps::delivery::DecoderPump) queues frames
/// across threads, [`CopyPump`](rust_video_core::pump::CopyPump) copies
/// bytes; this pump pipelines submits across one multi-inflight decoder.
pub struct OverlappedPump<D: SplitDecode> {
    /// Split-phase decode behind sends.
    decoder: D,
    /// Completed frames awaiting receive, oldest first.
    ready: VecDeque<D::Frame>,
}

impl<D: SplitDecode> OverlappedPump<D> {
    /// Sends admitted while fewer outputs are in flight.
    pub const MAX_INFLIGHT: usize = INFLIGHT_DEPTH;

    /// Pump decoding through `decoder`.
    ///
    /// # Arguments
    ///
    /// * `decoder` - open multi-inflight decoder behind sends.
    #[must_use]
    pub fn open(decoder: D) -> Self {
        Self {
            decoder,
            ready: VecDeque::new(),
        }
    }

    /// Decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }
}

// Sealed per decoder: only the four tier decoders implement the gauges.
#[allow(private_bounds)]
impl<D: SplitDecode<Error = D3d11Error> + PumpGauges> OverlappedPump<D> {
    /// Admitted outputs not yet completed.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.decoder.depth()
    }

    /// Peak admitted outputs not yet completed.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.decoder.max_depth()
    }

    /// True once input ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.decoder.is_finished()
    }

    /// Phase-split timing per collected frame, oldest first.
    #[must_use]
    pub fn timings(&self) -> &[PhaseTiming] {
        self.decoder.pump_timings()
    }

    /// True once input ended and every frame drained.
    fn drained(&self) -> bool {
        self.ready.is_empty() && self.decoder.is_finished() && self.decoder.depth() == 0
    }
}

#[allow(private_bounds)]
impl<D: SplitDecode<Error = D3d11Error> + PumpGauges> SendPackets for OverlappedPump<D> {
    type Error = D3d11Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        match self.decoder.try_submit(&packet) {
            Ok(true) => Ok(()),
            Ok(false) => Err(TrySendError::Full { packet }),
            Err(error) => Err(TrySendError::Failed { error }),
        }
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        let trailing = self.decoder.flush_split()?;
        self.ready.extend(trailing);
        Ok(())
    }
}

#[allow(private_bounds)]
impl<D: SplitDecode<Error = D3d11Error> + PumpGauges> PollFrames for OverlappedPump<D> {
    type Frame = D::Frame;
    type Error = D3d11Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        if let Some(frame) = self.ready.pop_front() {
            return Ok(Some(frame));
        }
        let frames = self.decoder.poll()?;
        self.ready.extend(frames);
        Ok(self.ready.pop_front())
    }
}

#[allow(private_bounds)]
impl<D: SplitDecode<Error = D3d11Error> + PumpGauges> ReceiveFrames for OverlappedPump<D> {
    type Frame = D::Frame;
    type Error = D3d11Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            if self.drained() {
                return Ok(None);
            }
            let frames = self.decoder.poll_wait(Duration::from_millis(1))?;
            self.ready.extend(frames);
        }
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        if let Some(frame) = self.ready.pop_front() {
            return Ok(Some(frame));
        }
        if self.drained() {
            return Ok(None);
        }
        let frames = self
            .decoder
            .poll_wait(timeout)
            .map_err(|error| ReceiveError::Failed { error })?;
        self.ready.extend(frames);
        match self.ready.pop_front() {
            Some(frame) => Ok(Some(frame)),
            None if self.drained() => Ok(None),
            None => Err(ReceiveError::Timeout),
        }
    }
}

/// Submitted output awaiting completion read.
#[derive(Clone, Debug)]
pub(crate) struct PendingOutput {
    /// Readback lane behind the output.
    lane: u8,
    /// Sequence behind the submit.
    active: Active,
    /// Submit plus copy issue start.
    submit_start: Instant,
    /// Submit plus copy issue end.
    submit_end: Instant,
    /// Copy launched on the worker.
    launched: bool,
    /// First observed completion.
    signaled_at: Option<Instant>,
    /// Frame generation number.
    generation: u64,
}

impl PendingOutput {
    /// Output under `active` across the submit span, read via `lane`.
    pub(crate) fn new(
        lane: u8,
        active: Active,
        submit_start: Instant,
        submit_end: Instant,
    ) -> Self {
        Self {
            lane,
            active,
            submit_start,
            submit_end,
            launched: false,
            signaled_at: None,
            generation: 0,
        }
    }

    /// Readback lane behind the output.
    pub(crate) fn lane(&self) -> u8 {
        self.lane
    }
}

/// Shared split-phase queue mechanics over decoder state.
pub(crate) struct SplitParts<'s> {
    /// Live decode session, absent before the first sequence.
    session: Option<&'s Session>,
    /// VRAM budget, absent before the first sequence.
    shared: Option<&'s SharedDevice>,
    /// Submitted outputs plus completed frames awaiting poll.
    queue: &'s mut FrameQueue<D3d11Frame>,
    /// Next frame generation number.
    generation: &'s mut u64,
    /// Phase-split timing per collected frame, oldest first.
    timings: &'s mut Vec<PhaseTiming>,
    /// Copy worker behind launches.
    readback: &'s Readback,
}

impl<'s> SplitParts<'s> {
    /// Mechanics over `session` with `queue`, `generation`, and `timings`.
    pub(crate) fn new(
        session: Option<&'s Session>,
        shared: Option<&'s SharedDevice>,
        queue: &'s mut FrameQueue<D3d11Frame>,
        generation: &'s mut u64,
        timings: &'s mut Vec<PhaseTiming>,
        readback: &'s Readback,
    ) -> Self {
        Self {
            session,
            shared,
            queue,
            generation,
            timings,
            readback,
        }
    }

    /// Completed oldest outputs into ready, without waiting.
    pub(crate) fn sweep_ready(&mut self) -> Result<(), D3d11Error> {
        self.harvest()?;
        let (Some(session), _) = (self.session, self.shared) else {
            return Ok(());
        };
        loop {
            let launch = match self.queue.front() {
                Some(front) if !front.launched => Some((front.lane(), front.active)),
                _ => None,
            };
            let Some((lane, active)) = launch else {
                break;
            };
            if !session.poll_lane(lane)? {
                break;
            }
            self.launch_front(lane, active)?;
        }
        Ok(())
    }

    /// Oldest pending output into ready, waiting for it.
    ///
    /// No-op when nothing is pending.
    pub(crate) fn drain_oldest(&mut self) -> Result<(), D3d11Error> {
        let (Some(session), _) = (self.session, self.shared) else {
            return Ok(());
        };
        let launch = match self.queue.front() {
            Some(front) if !front.launched => Some((front.lane(), front.active)),
            Some(_) => None,
            None => return Ok(()),
        };
        if let Some((lane, active)) = launch {
            session.wait_lane(lane)?;
            self.launch_front(lane, active)?;
        }
        self.collect_oldest()
    }

    /// Front copy launched on the worker, signaled just now.
    fn launch_front(&mut self, lane: u8, active: Active) -> Result<(), D3d11Error> {
        let (Some(session), _) = (self.session, self.shared) else {
            return Ok(());
        };
        let signaled_at = Instant::now();
        let mapped = session.map_lane(lane, active.crop())?;
        let generation = *self.generation;
        *self.generation = self.generation.wrapping_add(1);
        if let Some(front) = self.queue.front_mut() {
            front.launched = true;
            front.signaled_at = Some(signaled_at);
            front.generation = generation;
        }
        if let Some(done) = self.readback.launch(mapped) {
            self.land(done)?;
        }
        Ok(())
    }

    /// Landed worker copies into ready, without waiting.
    fn harvest(&mut self) -> Result<(), D3d11Error> {
        while let Some(done) = self.readback.harvest() {
            self.land(done)?;
        }
        Ok(())
    }

    /// Oldest launched copy into ready.
    fn land(&mut self, done: crate::readback::Done) -> Result<(), D3d11Error> {
        let (Some(session), Some(shared)) = (self.session, self.shared) else {
            return Ok(());
        };
        let Some(output) = self.queue.pop_pending() else {
            debug_assert!(false, "landed copy without pending output");
            return Ok(());
        };
        let (bytes, release, copy) = done.into_parts();
        let unmap_start = Instant::now();
        release.unmap();
        session.give_lane(output.lane);
        let signaled_at = output.signaled_at.unwrap_or(unmap_start);
        let read_end = signaled_at + copy + unmap_start.elapsed();
        let frame = emit_frame_bytes(session, shared, &output.active, output.generation, bytes?)?;
        self.timings.push(PhaseTiming::new(
            output.generation,
            output
                .submit_end
                .saturating_duration_since(output.submit_start),
            signaled_at.saturating_duration_since(output.submit_end),
            read_end.saturating_duration_since(signaled_at),
            output.submit_start,
            read_end,
        ));
        self.queue.push_ready(frame);
        Ok(())
    }

    /// Oldest pending output read synchronously, rescuing a lost worker.
    fn rescue(&mut self) -> Result<(), D3d11Error> {
        let (Some(session), Some(shared)) = (self.session, self.shared) else {
            return Ok(());
        };
        let Some(output) = self.queue.pop_pending() else {
            return Ok(());
        };
        let frame = emit_frame(
            session,
            shared,
            output.lane,
            &output.active,
            output.generation,
        )?;
        session.give_lane(output.lane);
        self.queue.push_ready(frame);
        Ok(())
    }

    /// Every pending output into ready, oldest first.
    pub(crate) fn drain_all(&mut self) -> Result<(), D3d11Error> {
        while self.queue.front().is_some() {
            self.drain_oldest()?;
        }
        Ok(())
    }

    /// Completed frames oldest-first into `frames`.
    pub(crate) fn take_ready(&mut self, frames: &mut Vec<D3d11Frame>) {
        while let Some(frame) = self.queue.pop_ready() {
            frames.push(frame);
        }
    }

    /// Free readback lane, draining oldest-first under the ceiling until one frees.
    pub(crate) fn make_lane(&mut self) -> Result<u8, D3d11Error> {
        let Some(session) = self.session else {
            return Err(D3d11Error::NoSession);
        };
        loop {
            if self.queue.pending() < INFLIGHT_CEILING {
                if let Some(lane) = session.take_lane() {
                    return Ok(lane);
                }
            }
            if self.queue.front().is_none() {
                return Err(D3d11Error::OverCaps {
                    what: "readback lanes",
                });
            }
            self.drain_oldest()?;
        }
    }

    /// `slot` copied onto a free lane, returning it.
    pub(crate) fn copy_output(&mut self, slot: u8) -> Result<u8, D3d11Error> {
        let (Some(session), _) = (self.session, self.shared) else {
            return Err(D3d11Error::NoSession);
        };
        let lane = self.make_lane()?;
        if let Err(error) = session.copy_lane(slot, lane) {
            session.give_lane(lane);
            return Err(error);
        }
        Ok(lane)
    }

    /// Pending outputs abandoned, their lanes returned.
    pub(crate) fn abandon_pending(&mut self) {
        if let Some(session) = self.session {
            while let Some(output) = self.queue.pop_pending() {
                session.give_lane(output.lane);
            }
        }
        self.queue.clear_pending();
    }

    /// Pending outputs and ready frames abandoned, lanes returned.
    pub(crate) fn abandon_queue(&mut self) {
        self.abandon_pending();
        self.queue.clear();
    }

    /// `slots` copied and read under `active` into `frames`, oldest first.
    pub(crate) fn read_live(
        &mut self,
        active: &Active,
        slots: &[u8],
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let (Some(session), Some(shared)) = (self.session, self.shared) else {
            return Ok(());
        };
        let Some(lane) = session.take_lane() else {
            return Err(D3d11Error::OverCaps {
                what: "readback lanes",
            });
        };
        let mut outcome = Ok(());
        for slot in slots {
            let submit_start = Instant::now();
            if let Err(error) = session.copy_lane(*slot, lane) {
                outcome = Err(error);
                break;
            }
            let submit_end = Instant::now();
            if let Err(error) = session.wait_lane(lane) {
                outcome = Err(error);
                break;
            }
            let signaled_at = Instant::now();
            let frame = match emit_frame(session, shared, lane, active, *self.generation) {
                Ok(frame) => frame,
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            };
            let read_end = Instant::now();
            self.timings.push(PhaseTiming::new(
                *self.generation,
                submit_end.saturating_duration_since(submit_start),
                signaled_at.saturating_duration_since(submit_end),
                read_end.saturating_duration_since(signaled_at),
                submit_start,
                read_end,
            ));
            *self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        session.give_lane(lane);
        outcome
    }

    /// Oldest launched copy into ready, waiting for it.
    ///
    /// No-op when nothing is launched.
    fn collect_oldest(&mut self) -> Result<(), D3d11Error> {
        let launched = matches!(self.queue.front(), Some(front) if front.launched);
        if !launched {
            return Ok(());
        }
        match self.readback.collect() {
            Some(done) => self.land(done),
            None => self.rescue(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use super::{FrameQueue, OverlappedPump, PumpGauges, INFLIGHT_CEILING, INFLIGHT_DEPTH};
    use crate::decoder::Active;
    use crate::error::D3d11Error;

    use bytes::Bytes;
    use rust_video_caps::delivery::{
        PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets, SplitDecode,
        SubmitStats, TrySendError,
    };
    use rust_video_caps::packet::Packet;

    /// Scripted split decoder: submits queue, releases complete oldest-first.
    struct Stub {
        /// Submitted packets not yet completed.
        pending: VecDeque<u64>,
        /// Completed frames not yet polled.
        ready: VecDeque<u64>,
        /// Next frame id.
        next: u64,
        /// Peak pending count.
        max: usize,
        /// Input ended.
        finished: bool,
        /// Timing per released frame, oldest first.
        timings: Vec<PhaseTiming>,
    }

    impl Stub {
        /// Empty stub.
        fn new() -> Self {
            Self {
                pending: VecDeque::new(),
                ready: VecDeque::new(),
                next: 0,
                max: 0,
                finished: false,
                timings: Vec::new(),
            }
        }

        /// Every pending packet completed oldest-first.
        fn release(&mut self) {
            while let Some(id) = self.pending.pop_front() {
                self.ready.push_back(id);
                let now = Instant::now();
                self.timings.push(PhaseTiming::new(
                    id,
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::ZERO,
                    now,
                    now,
                ));
            }
        }
    }

    impl SplitDecode for Stub {
        type Frame = u64;
        type Error = D3d11Error;

        fn try_submit(&mut self, packet: &Packet) -> Result<bool, D3d11Error> {
            let _ = packet;
            if self.finished || self.pending.len() >= INFLIGHT_DEPTH {
                return Ok(false);
            }
            self.pending.push_back(self.next);
            self.next += 1;
            self.max = self.max.max(self.pending.len());
            Ok(true)
        }

        fn poll(&mut self) -> Result<Vec<u64>, D3d11Error> {
            Ok(std::mem::take(&mut self.ready).into_iter().collect())
        }

        fn poll_wait(&mut self, _timeout: Duration) -> Result<Vec<u64>, D3d11Error> {
            self.poll()
        }

        fn flush_split(&mut self) -> Result<Vec<u64>, D3d11Error> {
            self.finished = true;
            self.release();
            self.poll()
        }

        fn stats(&self) -> SubmitStats {
            let mut stats = SubmitStats::new();
            stats.fold_peak(self.max);
            stats
        }
    }

    impl PumpGauges for Stub {
        fn depth(&self) -> usize {
            self.pending.len()
        }

        fn max_depth(&self) -> usize {
            self.max
        }

        fn is_finished(&self) -> bool {
            self.finished
        }

        fn pump_timings(&self) -> &[PhaseTiming] {
            &self.timings
        }
    }

    /// Coded packet carrying one zero byte.
    fn packet() -> Packet {
        Packet::new(Bytes::from(vec![0]))
    }

    /// Test sequence at 320x240.
    fn active() -> Active {
        Active::new(0, 320, 240, None, 320, 240).expect("valid test sequence")
    }

    #[test]
    fn queue_admits_under_bound() {
        let mut queue = FrameQueue::<u64>::new();
        let start = Instant::now();
        for lane in 0..u8::try_from(INFLIGHT_DEPTH).expect("bound fits") {
            assert!(queue.admits());
            queue.push(lane, active(), start, start);
        }
        assert!(!queue.admits());
        assert_eq!(queue.pending(), INFLIGHT_DEPTH);
        assert_eq!(queue.max_observed(), INFLIGHT_DEPTH);
        queue.pop_pending();
        assert!(queue.admits());
    }

    #[test]
    fn queue_depth_clamps_to_lanes() {
        let start = Instant::now();
        let mut one = FrameQueue::<u64>::with_depth(0);
        one.push(0, active(), start, start);
        assert!(!one.admits());
        let mut full = FrameQueue::<u64>::with_depth(usize::MAX);
        for lane in 0..u8::try_from(INFLIGHT_CEILING).expect("bound fits") {
            assert!(full.admits());
            full.push(lane, active(), start, start);
        }
        assert!(!full.admits());
    }

    #[test]
    fn queue_admits_under_configured_depth() {
        let mut queue = FrameQueue::<u64>::with_depth(2);
        let start = Instant::now();
        assert!(queue.admits());
        queue.push(0, active(), start, start);
        assert!(queue.admits());
        queue.push(1, active(), start, start);
        assert!(!queue.admits());
    }

    #[test]
    fn queue_pending_carries_lane() {
        let mut queue = FrameQueue::<u64>::new();
        let start = Instant::now();
        queue.push(5, active(), start, start);
        let front = queue.front().expect("pending output");
        assert_eq!(front.lane(), 5);
        queue.pop_pending();
        assert!(queue.front().is_none());
    }

    #[test]
    fn queue_ready_pops_oldest_first() {
        let mut queue = FrameQueue::new();
        queue.push_ready(1);
        queue.push_ready(2);
        assert!(!queue.is_drained());
        assert_eq!(queue.pop_ready(), Some(1));
        assert_eq!(queue.pop_ready(), Some(2));
        assert_eq!(queue.pop_ready(), None);
        assert!(queue.is_drained());
    }

    #[test]
    fn queue_clear_pending_keeps_ready() {
        let mut queue = FrameQueue::new();
        let start = Instant::now();
        queue.push(1, active(), start, start);
        queue.push_ready(7);
        queue.clear_pending();
        assert_eq!(queue.pending(), 0);
        assert_eq!(queue.pop_ready(), Some(7));
        queue.push_ready(8);
        queue.clear();
        assert!(queue.is_drained());
    }

    #[test]
    fn pump_refuses_past_bound_then_accepts_after_poll() {
        let mut pump = OverlappedPump::open(Stub::new());
        for _ in 0..INFLIGHT_DEPTH {
            pump.try_send(packet()).expect("queue room");
        }
        assert_eq!(pump.depth(), OverlappedPump::<Stub>::MAX_INFLIGHT);
        assert!(matches!(
            pump.try_send(packet()),
            Err(TrySendError::Full { .. })
        ));
        pump.decoder.release();
        let mut ids = Vec::new();
        while let Some(id) = pump.try_receive().expect("poll") {
            ids.push(id);
        }
        assert_eq!(ids, [0, 1, 2, 3]);
        assert_eq!(pump.max_depth(), INFLIGHT_DEPTH);
        pump.try_send(packet()).expect("room after poll");
    }

    #[test]
    fn pump_send_after_finish_returns_packet() {
        let mut pump = OverlappedPump::open(Stub::new());
        pump.finish().expect("finish");
        assert!(pump.is_finished());
        let err = pump.try_send(packet()).unwrap_err();
        let TrySendError::Full { packet } = err else {
            panic!("full after finish");
        };
        assert_eq!(packet.data(), &[0]);
    }

    #[test]
    fn pump_finish_twice_drains_empty() {
        let mut pump = OverlappedPump::open(Stub::new());
        pump.try_send(packet()).expect("send");
        pump.finish().expect("first finish");
        pump.finish().expect("second finish");
        assert_eq!(pump.receive().expect("trailing"), Some(0));
        assert_eq!(pump.receive().expect("drained"), None);
        assert_eq!(pump.try_receive().expect("polled"), None);
    }

    #[test]
    fn pump_receive_timeout_on_empty() {
        let mut pump = OverlappedPump::open(Stub::new());
        let err = pump.receive_timeout(Duration::from_millis(10)).unwrap_err();
        assert!(matches!(err, ReceiveError::Timeout));
    }

    #[test]
    fn pump_exposes_decoder_timings() {
        let mut pump = OverlappedPump::open(Stub::new());
        pump.try_send(packet()).expect("send");
        pump.decoder.release();
        assert_eq!(pump.try_receive().expect("poll"), Some(0));
        let timings = pump.timings();
        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].seq(), 0);
        assert_eq!(pump.decoder.stats().peak_inflight(), 1);
    }
}
