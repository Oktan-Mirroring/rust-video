//! Multi-inflight overlapped decode behind the frozen async traits.
//!
//! Sends submit without waiting on completion; receives poll per-frame
//! fences and hand completed frames off oldest-first.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::decoder::emit_frame;
use crate::decoder::Active;
use crate::error::D3d11Error;
use crate::frame::D3d11Frame;
use crate::session::Session;

use rust_video_caps::delivery::{
    PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::SharedDevice;

/// Sends admitted while fewer outputs are in flight; [`FrameQueue::admits`]
/// enforces it, [`OverlappedPump::DEPTH`] publishes it.
pub(crate) const INFLIGHT_DEPTH: usize = 4;

/// Pending outputs before oldest-first blocking drains; decoders enforce
/// it in their submit path next to the lane-collision check.
pub(crate) const INFLIGHT_CEILING: usize = 8;

/// Split-phase decode: submit without waiting, poll completions in order.
///
/// Sync [`Decoder`](rust_video_caps::traits::Decoder) calls and split calls
/// share the output queue; drive one decoder from one side only.
pub trait SplitDecoder {
    /// Completed frame type.
    type Frame;

    /// `packet` submitted without waiting on completion.
    ///
    /// # Errors
    ///
    /// Returns [`TrySendError::Full`] with `packet` back past the in-flight
    /// bound or after input finished, [`TrySendError::Failed`] when the
    /// backend rejects `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>>;

    /// Oldest completed frame, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its ready frames.
    ///
    /// # Returns
    ///
    /// `None` when no frame completed yet; poll again or finish input.
    fn poll_frame(&mut self) -> Result<Option<Self::Frame>, D3d11Error>;

    /// Oldest frame, waiting for one or for end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its next frame.
    ///
    /// # Returns
    ///
    /// `None` once input finished and every queued frame drained.
    fn wait_frame(&mut self) -> Result<Option<Self::Frame>, D3d11Error>;

    /// Trailing frames, then end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    ///
    /// # Usage Notes
    ///
    /// Queues output held back for reordering or buffering, then ends input;
    /// receives drain the queue and end with `None`. Repeated calls succeed
    /// without new output.
    fn finish_input(&mut self) -> Result<(), D3d11Error>;

    /// Admitted outputs not yet completed.
    fn depth(&self) -> usize;

    /// Peak admitted outputs not yet completed.
    fn max_depth(&self) -> usize;

    /// True once input ended.
    fn is_finished(&self) -> bool;
}

/// Phase-split timing capture over decoded frames.
pub trait PhaseCapture {
    /// Record per-frame phases from now on when `on`.
    ///
    /// # Arguments
    ///
    /// * `on` - capture switch.
    fn capture_phases(&mut self, on: bool);

    /// Recorded per-frame phases, oldest first.
    fn phases(&self) -> &[FramePhases];
}

/// Submit, engine-wait, and readback spans of one decoded frame.
#[derive(Clone, Copy, Debug)]
pub struct FramePhases {
    /// Frame generation number.
    generation: u64,
    /// Decode slot behind the frame.
    slot: u8,
    /// Submit plus copy issue start.
    submit_start: Instant,
    /// Submit plus copy issue end.
    submit_end: Instant,
    /// First observed completion.
    signaled_at: Instant,
    /// Readback end.
    read_end: Instant,
}

impl FramePhases {
    /// Phases of `generation` from `slot` across the four stamps.
    pub(crate) fn new(
        generation: u64,
        slot: u8,
        submit_start: Instant,
        submit_end: Instant,
        signaled_at: Instant,
        read_end: Instant,
    ) -> Self {
        Self {
            generation,
            slot,
            submit_start,
            submit_end,
            signaled_at,
            read_end,
        }
    }

    /// Frame generation number.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Decode slot behind the frame.
    #[must_use]
    pub fn slot(&self) -> u8 {
        self.slot
    }

    /// Submit plus copy issue start.
    #[must_use]
    pub fn submit_start(&self) -> Instant {
        self.submit_start
    }

    /// Submit plus copy issue end.
    #[must_use]
    pub fn submit_end(&self) -> Instant {
        self.submit_end
    }

    /// First observed completion.
    #[must_use]
    pub fn signaled_at(&self) -> Instant {
        self.signaled_at
    }

    /// Readback end.
    #[must_use]
    pub fn read_end(&self) -> Instant {
        self.read_end
    }

    /// Submit plus copy issue span.
    #[must_use]
    pub fn submit(&self) -> Duration {
        self.submit_end.saturating_duration_since(self.submit_start)
    }

    /// Engine plus copy execution span as the CPU saw it.
    #[must_use]
    pub fn engine_wait(&self) -> Duration {
        self.signaled_at.saturating_duration_since(self.submit_end)
    }

    /// Map plus NV12 pack span.
    #[must_use]
    pub fn readback(&self) -> Duration {
        self.read_end.saturating_duration_since(self.signaled_at)
    }

    /// Submit start to readback end.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.read_end.saturating_duration_since(self.submit_start)
    }
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
}

impl<F> FrameQueue<F> {
    /// Empty queue.
    pub(crate) fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            max_observed: 0,
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
        self.pending.len() < INFLIGHT_DEPTH
    }

    /// True when `slot` names a pending output lane.
    pub(crate) fn collides(&self, slot: u8) -> bool {
        self.pending.iter().any(|output| output.slot() == slot)
    }

    /// Oldest pending output, if any.
    pub(crate) fn front(&self) -> Option<&PendingOutput> {
        self.pending.front()
    }

    /// `slot` submitted under `active` across the submit span.
    pub(crate) fn push(
        &mut self,
        slot: u8,
        active: Active,
        submit_start: Instant,
        submit_end: Instant,
    ) {
        self.pending
            .push_back(PendingOutput::new(slot, active, submit_start, submit_end));
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
/// oldest-first. Never drops: sends past [`DEPTH`](Self::DEPTH) in flight
/// refuse with the packet back.
pub struct OverlappedPump<D: SplitDecoder> {
    /// Split-phase decode behind sends.
    decoder: D,
}

impl<D: SplitDecoder> OverlappedPump<D> {
    /// Sends admitted while fewer outputs are in flight.
    pub const DEPTH: usize = INFLIGHT_DEPTH;

    /// Pump decoding through `decoder`.
    ///
    /// # Arguments
    ///
    /// * `decoder` - open multi-inflight decoder behind sends.
    #[must_use]
    pub fn open(decoder: D) -> Self {
        Self { decoder }
    }

    /// Decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }

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
}

impl<D: SplitDecoder + PhaseCapture> OverlappedPump<D> {
    /// Record per-frame phases from now on when `on`.
    ///
    /// # Arguments
    ///
    /// * `on` - capture switch.
    pub fn capture_phases(&mut self, on: bool) {
        self.decoder.capture_phases(on);
    }

    /// Recorded per-frame phases, oldest first.
    #[must_use]
    pub fn phases(&self) -> &[FramePhases] {
        self.decoder.phases()
    }
}

impl<D: SplitDecoder> SendPackets for OverlappedPump<D> {
    type Error = D3d11Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        self.decoder.submit_packet(packet)
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.decoder.finish_input()
    }
}

impl<D: SplitDecoder> PollFrames for OverlappedPump<D> {
    type Frame = D::Frame;
    type Error = D3d11Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.decoder.poll_frame()
    }
}

impl<D: SplitDecoder> ReceiveFrames for OverlappedPump<D> {
    type Frame = D::Frame;
    type Error = D3d11Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.decoder.wait_frame()
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self
                .decoder
                .poll_frame()
                .map_err(|error| ReceiveError::Failed { error })?
            {
                return Ok(Some(frame));
            }
            if self.decoder.is_finished() && self.decoder.depth() == 0 {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Err(ReceiveError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Submitted output awaiting completion read.
#[derive(Clone, Debug)]
pub(crate) struct PendingOutput {
    /// Decode slot behind the output.
    slot: u8,
    /// Sequence behind the submit.
    active: Active,
    /// Submit plus copy issue start.
    submit_start: Instant,
    /// Submit plus copy issue end.
    submit_end: Instant,
}

impl PendingOutput {
    /// Output from `slot` under `active` across the submit span.
    pub(crate) fn new(
        slot: u8,
        active: Active,
        submit_start: Instant,
        submit_end: Instant,
    ) -> Self {
        Self {
            slot,
            active,
            submit_start,
            submit_end,
        }
    }

    /// Decode slot behind the output.
    pub(crate) fn slot(&self) -> u8 {
        self.slot
    }

    /// Sequence behind the submit.
    pub(crate) fn active(&self) -> Active {
        self.active
    }

    /// Submit plus copy issue start.
    pub(crate) fn submit_start(&self) -> Instant {
        self.submit_start
    }

    /// Submit plus copy issue end.
    pub(crate) fn submit_end(&self) -> Instant {
        self.submit_end
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
    /// Phase capture switch.
    capture: bool,
    /// Recorded per-frame phases.
    phases: &'s mut Vec<FramePhases>,
}

impl<'s> SplitParts<'s> {
    /// Mechanics over `session` with `queue`, `generation`, and phase capture.
    pub(crate) fn new(
        session: Option<&'s Session>,
        shared: Option<&'s SharedDevice>,
        queue: &'s mut FrameQueue<D3d11Frame>,
        generation: &'s mut u64,
        capture: bool,
        phases: &'s mut Vec<FramePhases>,
    ) -> Self {
        Self {
            session,
            shared,
            queue,
            generation,
            capture,
            phases,
        }
    }

    /// Completed oldest outputs into ready, without waiting.
    pub(crate) fn sweep_ready(&mut self) -> Result<(), D3d11Error> {
        let (Some(session), _) = (self.session, self.shared) else {
            return Ok(());
        };
        while let Some(output) = self.queue.front() {
            if !session.poll_slot(output.slot())? {
                return Ok(());
            }
            let signaled_at = Instant::now();
            self.complete(signaled_at)?;
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
        let Some(output) = self.queue.front() else {
            return Ok(());
        };
        session.wait_slot(output.slot())?;
        let signaled_at = Instant::now();
        self.complete(signaled_at)
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

    /// Oldest-first blocking drains until `slot` fits under the ceiling.
    ///
    /// # Arguments
    ///
    /// * `slot` - submission slot checked against pending lanes; `None`
    ///   skips the collision check.
    pub(crate) fn make_room(&mut self, slot: Option<u8>) -> Result<(), D3d11Error> {
        while self.queue.pending() >= INFLIGHT_CEILING
            || slot.is_some_and(|slot| self.queue.collides(slot))
        {
            self.drain_oldest()?;
        }
        Ok(())
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
        let mut spans = Vec::with_capacity(slots.len());
        for slot in slots {
            let submit_start = Instant::now();
            session.copy_slot(*slot)?;
            spans.push((*slot, submit_start, Instant::now()));
        }
        for (slot, submit_start, submit_end) in spans {
            session.wait_slot(slot)?;
            let signaled_at = Instant::now();
            let frame = emit_frame(session, shared, slot, active, *self.generation)?;
            let read_end = Instant::now();
            if self.capture {
                self.phases.push(FramePhases::new(
                    *self.generation,
                    slot,
                    submit_start,
                    submit_end,
                    signaled_at,
                    read_end,
                ));
            }
            *self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Oldest pending output read into ready, completed at `signaled_at`.
    fn complete(&mut self, signaled_at: Instant) -> Result<(), D3d11Error> {
        let (Some(session), Some(shared)) = (self.session, self.shared) else {
            return Ok(());
        };
        let Some(output) = self.queue.pop_pending() else {
            return Ok(());
        };
        let frame = emit_frame(
            session,
            shared,
            output.slot(),
            &output.active(),
            *self.generation,
        )?;
        let read_end = Instant::now();
        if self.capture {
            self.phases.push(FramePhases::new(
                *self.generation,
                output.slot(),
                output.submit_start(),
                output.submit_end(),
                signaled_at,
                read_end,
            ));
        }
        *self.generation = self.generation.wrapping_add(1);
        self.queue.push_ready(frame);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use super::{FramePhases, FrameQueue, OverlappedPump, SplitDecoder, INFLIGHT_DEPTH};
    use crate::decoder::Active;
    use crate::error::D3d11Error;

    use bytes::Bytes;
    use rust_video_caps::delivery::{
        PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
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
            }
        }

        /// Every pending packet completed oldest-first.
        fn release(&mut self) {
            while let Some(id) = self.pending.pop_front() {
                self.ready.push_back(id);
            }
        }
    }

    impl SplitDecoder for Stub {
        type Frame = u64;

        fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>> {
            if self.finished || self.pending.len() >= INFLIGHT_DEPTH {
                return Err(TrySendError::Full { packet });
            }
            self.pending.push_back(self.next);
            self.next += 1;
            self.max = self.max.max(self.pending.len());
            Ok(())
        }

        fn poll_frame(&mut self) -> Result<Option<u64>, D3d11Error> {
            Ok(self.ready.pop_front())
        }

        fn wait_frame(&mut self) -> Result<Option<u64>, D3d11Error> {
            loop {
                if let Some(id) = self.ready.pop_front() {
                    return Ok(Some(id));
                }
                if self.finished && self.pending.is_empty() {
                    return Ok(None);
                }
                if let Some(id) = self.pending.pop_front() {
                    return Ok(Some(id));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        fn finish_input(&mut self) -> Result<(), D3d11Error> {
            self.finished = true;
            self.release();
            Ok(())
        }

        fn depth(&self) -> usize {
            self.pending.len()
        }

        fn max_depth(&self) -> usize {
            self.max
        }

        fn is_finished(&self) -> bool {
            self.finished
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
        for slot in 0..u8::try_from(INFLIGHT_DEPTH).expect("bound fits") {
            assert!(queue.admits());
            queue.push(slot, active(), start, start);
        }
        assert!(!queue.admits());
        assert_eq!(queue.pending(), INFLIGHT_DEPTH);
        assert_eq!(queue.max_observed(), INFLIGHT_DEPTH);
        queue.pop_pending();
        assert!(queue.admits());
    }

    #[test]
    fn queue_collides_on_pending_slot() {
        let mut queue = FrameQueue::<u64>::new();
        let start = Instant::now();
        queue.push(3, active(), start, start);
        assert!(queue.collides(3));
        assert!(!queue.collides(4));
        queue.pop_pending();
        assert!(!queue.collides(3));
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
        queue.push(0, active(), start, start);
        queue.push_ready(7);
        queue.clear_pending();
        assert_eq!(queue.pending(), 0);
        assert_eq!(queue.pop_ready(), Some(7));
        queue.push_ready(8);
        queue.clear();
        assert!(queue.is_drained());
    }

    #[test]
    fn phases_split_submit_wait_read() {
        let start = Instant::now();
        let phases = FramePhases::new(
            5,
            2,
            start,
            start + Duration::from_millis(1),
            start + Duration::from_millis(3),
            start + Duration::from_millis(4),
        );
        assert_eq!(phases.generation(), 5);
        assert_eq!(phases.slot(), 2);
        assert_eq!(phases.submit(), Duration::from_millis(1));
        assert_eq!(phases.engine_wait(), Duration::from_millis(2));
        assert_eq!(phases.readback(), Duration::from_millis(1));
        assert_eq!(phases.latency(), Duration::from_millis(4));
        assert_eq!(phases.submit_start(), start);
        assert!(phases.signaled_at() < phases.read_end());
    }

    #[test]
    fn pump_refuses_past_bound_then_accepts_after_poll() {
        let mut pump = OverlappedPump::open(Stub::new());
        for _ in 0..INFLIGHT_DEPTH {
            pump.try_send(packet()).expect("queue room");
        }
        assert_eq!(pump.depth(), OverlappedPump::<Stub>::DEPTH);
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
}
