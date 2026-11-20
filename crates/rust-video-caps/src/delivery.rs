//! Split-phase decode: send, poll, blocking receive.
//!
//! ffmpeg send/receive over a bounded channel with a drop policy.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, WaitTimeoutResult};
use std::time::{Duration, Instant};

use crate::packet::Packet;
use crate::traits::Decoder;

/// Non-blocking frame poll behind split-phase decode.
pub trait PollFrames {
    /// Polled frame type.
    type Frame;
    /// Poll failure.
    type Error: std::error::Error;

    /// Oldest ready frame, if any.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its ready frames.
    ///
    /// # Returns
    ///
    /// `None` when no frame is ready; send more or finish input.
    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error>;
}

/// Blocking frame receive behind split-phase decode.
pub trait ReceiveFrames {
    /// Received frame type.
    type Frame;
    /// Receive failure.
    type Error: std::error::Error;

    /// Next frame, waiting for one or for end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its next frame.
    ///
    /// # Returns
    ///
    /// `None` once input finished and every queued frame drained.
    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error>;

    /// Next frame, waiting up to `timeout` for one or for end of stream.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiveError::Timeout`] when `timeout` lapses with no frame
    /// ready and input unfinished, [`ReceiveError::Failed`] when the backend
    /// cannot release its next frame.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when no frame is ready.
    ///
    /// # Returns
    ///
    /// `None` once input finished and every queued frame drained.
    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>>;
}

/// Non-blocking packet send behind split-phase decode.
pub trait SendPackets {
    /// Send failure.
    type Error: std::error::Error;

    /// Queue `packet` for decode without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TrySendError::Full`] with `packet` back when the output queue
    /// is full, [`TrySendError::Failed`] when the backend rejects `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>>;

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
    fn finish(&mut self) -> Result<(), Self::Error>;
}

/// Submit/poll decode behind one split-phase engine.
pub trait SplitDecode {
    /// Decoded frame type.
    type Frame;
    /// Submit, poll, or flush failure.
    type Error: std::error::Error;

    /// `packet` queued without waiting for its pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend rejects `packet`.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    ///
    /// # Returns
    ///
    /// False when inflight work saturates the depth; the packet stays
    /// unconsumed, poll then resend.
    fn try_submit(&mut self, packet: &Packet) -> Result<bool, Self::Error>;

    /// Pictures completed since the last poll, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its completed frames.
    fn poll(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Pictures completed within `timeout`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its completed frames.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when nothing completed.
    ///
    /// # Returns
    ///
    /// Empty when the budget lapses with nothing completed.
    fn poll_wait(&mut self, timeout: Duration) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Trailing pictures, then end of stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its trailing frames.
    fn flush_split(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Submit gauges since backend open.
    fn stats(&self) -> SubmitStats;
}

/// Full-queue behavior behind a bounded channel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DropPolicy {
    /// Evict the oldest queued frame for the incoming one.
    DropOldest,
    /// Refuse the incoming frame, keeping the queued ones.
    DropIncoming,
}

/// Blocking receive failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiveError<E> {
    /// Wait budget lapsed with no frame ready.
    Timeout,
    /// Backend failure.
    Failed {
        /// Backend failure.
        error: E,
    },
}

impl<E: fmt::Display> fmt::Display for ReceiveError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "receive timed out"),
            Self::Failed { error } => write!(f, "{error}"),
        }
    }
}

impl<E: std::error::Error> std::error::Error for ReceiveError<E> {}

/// Non-blocking send refusal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrySendError<E> {
    /// Output queue full; receive, then resend the packet.
    Full {
        /// Refused packet.
        packet: Packet,
    },
    /// Backend failure.
    Failed {
        /// Backend failure.
        error: E,
    },
}

impl<E: fmt::Display> fmt::Display for TrySendError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { .. } => write!(f, "output queue full"),
            Self::Failed { error } => write!(f, "{error}"),
        }
    }
}

impl<E: std::error::Error> std::error::Error for TrySendError<E> {}

/// Bound plus drop policy behind a frame channel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChannelConfig {
    /// Maximum queued frame count.
    capacity: NonZeroUsize,
    /// Full-queue behavior.
    policy: DropPolicy,
}

impl ChannelConfig {
    /// Bound of `capacity` frames under `policy`.
    ///
    /// # Arguments
    ///
    /// * `capacity` - maximum queued frame count.
    /// * `policy` - full-queue behavior.
    #[must_use]
    pub const fn new(capacity: NonZeroUsize, policy: DropPolicy) -> Self {
        Self { capacity, policy }
    }

    /// Maximum queued frame count.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity.get()
    }

    /// Full-queue behavior.
    #[must_use]
    pub const fn policy(&self) -> DropPolicy {
        self.policy
    }
}

/// Shared queue, signal, bound, policy, counters.
#[derive(Debug)]
struct ChannelState<F> {
    /// Queued frames, oldest first.
    queue: Mutex<VecDeque<F>>,
    /// Queued-frame or end-of-input signal.
    ready: Condvar,
    /// Bound plus drop policy.
    config: ChannelConfig,
    /// Frames dropped under the policy, plus post-close sends.
    dropped: AtomicU64,
    /// Input ended.
    closed: AtomicBool,
}

impl<F> ChannelState<F> {
    /// Empty state under `config`.
    fn new(config: ChannelConfig) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(config.capacity())),
            ready: Condvar::new(),
            config,
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }
}

/// Bounded frame channel with an explicit drop policy.
///
/// Producers send without blocking; consumers poll, block, or drain through a
/// callback. Bound plus policy cap memory under a slow consumer.
#[derive(Debug)]
pub struct FrameChannel<F> {
    /// Shared queue, signal, bound, policy, counters.
    state: Arc<ChannelState<F>>,
}

impl<F> Clone for FrameChannel<F> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<F> FrameChannel<F> {
    /// Channel holding at most `config` frames under its drop policy.
    ///
    /// # Arguments
    ///
    /// * `config` - bound plus drop policy.
    #[must_use]
    pub fn new(config: ChannelConfig) -> Self {
        Self {
            state: Arc::new(ChannelState::new(config)),
        }
    }

    /// Bound plus drop policy.
    #[must_use]
    pub fn config(&self) -> ChannelConfig {
        self.state.config
    }

    /// Maximum queued frame count.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.state.config.capacity()
    }

    /// Full-queue behavior.
    #[must_use]
    pub fn policy(&self) -> DropPolicy {
        self.state.config.policy()
    }

    /// Queued frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True when no frame is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Frames dropped under the policy, plus post-close sends.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.state.dropped.load(Ordering::Relaxed)
    }

    /// True once the producer ended input.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }

    /// End of input; queued frames still drain, later sends drop.
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
        self.state.ready.notify_all();
    }

    /// Queue `frame`, dropping one frame when full.
    ///
    /// # Arguments
    ///
    /// * `frame` - frame to enqueue.
    ///
    /// # Returns
    ///
    /// Dropped frame: evicted oldest under [`DropPolicy::DropOldest`], `frame`
    /// itself under [`DropPolicy::DropIncoming`] or after close; `None` when
    /// `frame` queued.
    #[must_use]
    pub fn send(&self, frame: F) -> Option<F> {
        if self.is_closed() {
            self.count_drop();
            return Some(frame);
        }
        let mut queue = self.lock();
        if queue.len() < self.capacity() {
            queue.push_back(frame);
            drop(queue);
            self.state.ready.notify_one();
            return None;
        }
        match self.policy() {
            DropPolicy::DropOldest => {
                let evicted = queue.pop_front();
                queue.push_back(frame);
                drop(queue);
                self.state.ready.notify_one();
                self.count_drop();
                evicted
            }
            DropPolicy::DropIncoming => {
                drop(queue);
                self.count_drop();
                Some(frame)
            }
        }
    }

    /// Oldest queued frame, if any.
    #[must_use]
    pub fn try_receive(&self) -> Option<F> {
        self.lock().pop_front()
    }

    /// Oldest queued frame, waiting for one or for end of input.
    ///
    /// # Returns
    ///
    /// `None` once input ended and every queued frame drained.
    #[must_use]
    pub fn receive(&self) -> Option<F> {
        let mut queue = self.lock();
        loop {
            if let Some(frame) = queue.pop_front() {
                return Some(frame);
            }
            if self.is_closed() {
                return None;
            }
            queue = self.wait(queue);
        }
    }

    /// Oldest queued frame, waiting up to `timeout` for one or for end of input.
    ///
    /// # Errors
    ///
    /// Returns [`ReceiveTimeout`] when `timeout` lapses with no frame queued
    /// and input unfinished.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when no frame is queued.
    ///
    /// # Returns
    ///
    /// `None` once input ended and every queued frame drained.
    pub fn receive_timeout(&self, timeout: Duration) -> Result<Option<F>, ReceiveTimeout> {
        let mut queue = self.lock();
        loop {
            if let Some(frame) = queue.pop_front() {
                return Ok(Some(frame));
            }
            if self.is_closed() {
                return Ok(None);
            }
            let (guard, lapsed) = self.wait_timeout(queue, timeout);
            queue = guard;
            if lapsed.timed_out() && queue.is_empty() && !self.is_closed() {
                return Err(ReceiveTimeout);
            }
        }
    }

    /// Every queued frame through `on_frame`, oldest first.
    ///
    /// # Arguments
    ///
    /// * `on_frame` - callback per queued frame.
    ///
    /// # Returns
    ///
    /// Drained frame count.
    #[must_use]
    pub fn drain_with(&self, mut on_frame: impl FnMut(F)) -> usize {
        let mut drained = 0;
        while let Some(frame) = self.try_receive() {
            on_frame(frame);
            drained += 1;
        }
        drained
    }

    /// Queue guard, poison-tolerant.
    fn lock(&self) -> MutexGuard<'_, VecDeque<F>> {
        self.state
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait for a queued frame or end of input.
    fn wait<'a>(&self, queue: MutexGuard<'a, VecDeque<F>>) -> MutexGuard<'a, VecDeque<F>> {
        self.state
            .ready
            .wait(queue)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait up to `timeout` for a queued frame or end of input.
    fn wait_timeout<'a>(
        &self,
        queue: MutexGuard<'a, VecDeque<F>>,
        timeout: Duration,
    ) -> (MutexGuard<'a, VecDeque<F>>, WaitTimeoutResult) {
        self.state
            .ready
            .wait_timeout(queue, timeout)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// One counted drop.
    fn count_drop(&self) {
        self.state.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

// Not core::CopyPump (threaded memcpy) nor an overlap pump (multi-inflight submit/collect).
/// Split-phase decode over a sync decoder plus a bounded channel.
///
/// `try_send` decodes now and queues output through the bound plus policy;
/// consumers poll, block, or drain through a callback. Slow consumers drop
/// frames per policy; memory stays bounded.
pub struct DecoderPump<D: Decoder> {
    /// Sync decode behind sends.
    decoder: D,
    /// Bounded output with drop policy.
    channel: FrameChannel<D::Frame>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
}

impl<D: Decoder> DecoderPump<D> {
    /// Pump decoding through `decoder` into `config` frames of queue.
    ///
    /// # Arguments
    ///
    /// * `decoder` - sync decode behind sends.
    /// * `config` - bound plus drop policy.
    #[must_use]
    pub fn open(decoder: D, config: ChannelConfig) -> Self {
        Self {
            decoder,
            channel: FrameChannel::new(config),
            finished: false,
        }
    }

    /// Sync decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }

    /// Bound plus drop policy.
    #[must_use]
    pub fn config(&self) -> ChannelConfig {
        self.channel.config()
    }

    /// Queued frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// True when no frame is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }

    /// True once input ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Frames dropped under the policy, plus post-close sends.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.channel.dropped()
    }

    /// Consumer handle over the queue; later sends arrive here.
    #[must_use]
    pub fn channel(&self) -> FrameChannel<D::Frame> {
        self.channel.clone()
    }
}

impl<D: Decoder> SendPackets for DecoderPump<D> {
    type Error = D::Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        let frames = self
            .decoder
            .decode(&packet)
            .map_err(|error| TrySendError::Failed { error })?;
        for frame in frames {
            let _dropped = self.channel.send(frame);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        for frame in self.decoder.flush()? {
            let _dropped = self.channel.send(frame);
        }
        self.channel.close();
        Ok(())
    }
}

impl<D: Decoder> PollFrames for DecoderPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        Ok(self.channel.try_receive())
    }
}

impl<D: Decoder> ReceiveFrames for DecoderPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        Ok(self.channel.receive())
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        self.channel
            .receive_timeout(timeout)
            .map_err(|ReceiveTimeout| ReceiveError::Timeout)
    }
}

/// Submit, engine-wait, and copy phases of one collected picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhaseTiming {
    /// Collection order.
    seq: u64,
    /// Queue-submit phase: record plus submit, no completion wait.
    submit: Duration,
    /// Engine-wait phase: submit to output-ready.
    engine_wait: Duration,
    /// Copy phase: output-to-surface conversion.
    copy: Duration,
    /// Submit instant of the picture.
    submitted_at: Instant,
    /// Collection instant of the picture.
    collected_at: Instant,
    /// Input stamp in microseconds, present on stamp-correlated backends.
    submitted_pts_us: Option<u64>,
    /// Output stamp in microseconds, present on stamp-correlated backends.
    completed_pts_us: Option<i64>,
}

impl PhaseTiming {
    /// Timing of sequence `seq` over its three phases and wall span.
    ///
    /// # Arguments
    ///
    /// * `seq` - collection order.
    /// * `submit` - queue-submit phase.
    /// * `engine_wait` - engine-wait phase.
    /// * `copy` - copy phase.
    /// * `submitted_at` - submit instant.
    /// * `collected_at` - collection instant.
    #[must_use]
    pub fn new(
        seq: u64,
        submit: Duration,
        engine_wait: Duration,
        copy: Duration,
        submitted_at: Instant,
        collected_at: Instant,
    ) -> Self {
        Self {
            seq,
            submit,
            engine_wait,
            copy,
            submitted_at,
            collected_at,
            submitted_pts_us: None,
            completed_pts_us: None,
        }
    }

    /// Timing stamped with its input and output presentation stamps.
    ///
    /// # Arguments
    ///
    /// * `submitted_pts_us` - input stamp in microseconds.
    /// * `completed_pts_us` - output stamp in microseconds.
    #[must_use]
    pub fn with_pts(mut self, submitted_pts_us: u64, completed_pts_us: i64) -> Self {
        self.submitted_pts_us = Some(submitted_pts_us);
        self.completed_pts_us = Some(completed_pts_us);
        self
    }

    /// Collection order.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Queue-submit phase: record plus submit, no completion wait.
    #[must_use]
    pub fn submit(&self) -> Duration {
        self.submit
    }

    /// Engine-wait phase: submit to output-ready.
    #[must_use]
    pub fn engine_wait(&self) -> Duration {
        self.engine_wait
    }

    /// Copy phase: output-to-surface conversion.
    #[must_use]
    pub fn copy(&self) -> Duration {
        self.copy
    }

    /// Submit instant of the picture.
    #[must_use]
    pub fn submitted_at(&self) -> Instant {
        self.submitted_at
    }

    /// Collection instant of the picture.
    #[must_use]
    pub fn collected_at(&self) -> Instant {
        self.collected_at
    }

    /// Input stamp in microseconds, present on stamp-correlated backends.
    #[must_use]
    pub fn submitted_pts_us(&self) -> Option<u64> {
        self.submitted_pts_us
    }

    /// Output stamp in microseconds, present on stamp-correlated backends.
    #[must_use]
    pub fn completed_pts_us(&self) -> Option<i64> {
        self.completed_pts_us
    }

    /// Stage-time sum: submit plus engine-wait plus copy.
    #[must_use]
    pub fn stage_sum(&self) -> Duration {
        self.submit + self.engine_wait + self.copy
    }

    /// Queue-submit phase in whole microseconds, saturating.
    #[must_use]
    pub fn submit_us(&self) -> u64 {
        Self::micros(self.submit)
    }

    /// Engine-wait phase in whole microseconds, saturating.
    #[must_use]
    pub fn engine_wait_us(&self) -> u64 {
        Self::micros(self.engine_wait)
    }

    /// Copy phase in whole microseconds, saturating.
    #[must_use]
    pub fn copy_us(&self) -> u64 {
        Self::micros(self.copy)
    }

    /// Submit plus engine-wait plus copy in microseconds, saturating.
    #[must_use]
    pub fn total_us(&self) -> u64 {
        self.submit_us()
            .saturating_add(self.engine_wait_us())
            .saturating_add(self.copy_us())
    }

    /// Frame latency in microseconds, submit to collection, saturating.
    #[must_use]
    pub fn latency_us(&self) -> u64 {
        Self::micros(
            self.collected_at
                .saturating_duration_since(self.submitted_at),
        )
    }

    /// Whole microseconds behind `duration`, saturating.
    fn micros(duration: Duration) -> u64 {
        u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
    }
}

/// Lapsed blocking-receive budget.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReceiveTimeout;

impl fmt::Display for ReceiveTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receive timed out")
    }
}

impl std::error::Error for ReceiveTimeout {}

/// Submit gauges since backend open.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubmitStats {
    /// Pictures submitted.
    submitted: u64,
    /// Deepest submitted-but-uncollected count observed.
    peak_inflight: usize,
    /// Submit-core resets after failures.
    resets: u64,
    /// Driver objects created since open.
    buffer_creates: u64,
}

impl SubmitStats {
    /// Zeroed gauges.
    #[must_use]
    pub fn new() -> Self {
        Self {
            submitted: 0,
            peak_inflight: 0,
            resets: 0,
            buffer_creates: 0,
        }
    }

    /// Snapshot of `submitted` submits peaking at `peak_inflight` with
    /// `resets` resets and `buffer_creates` driver creates.
    ///
    /// # Arguments
    ///
    /// * `submitted` - pictures submitted.
    /// * `peak_inflight` - deepest submitted-but-uncollected count observed.
    /// * `resets` - submit-core resets after failures.
    /// * `buffer_creates` - driver objects created since open.
    #[must_use]
    pub fn snapshot(
        submitted: u64,
        peak_inflight: usize,
        resets: u64,
        buffer_creates: u64,
    ) -> Self {
        Self {
            submitted,
            peak_inflight,
            resets,
            buffer_creates,
        }
    }

    /// Pictures submitted.
    #[must_use]
    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Deepest submitted-but-uncollected count observed.
    #[must_use]
    pub fn peak_inflight(&self) -> usize {
        self.peak_inflight
    }

    /// Submit-core resets after failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Driver objects created since open.
    #[must_use]
    pub fn buffer_creates(&self) -> u64 {
        self.buffer_creates
    }

    /// Peak raised to `peak` when deeper.
    ///
    /// # Arguments
    ///
    /// * `peak` - observed submitted-but-uncollected count.
    pub fn fold_peak(&mut self, peak: usize) {
        self.peak_inflight = self.peak_inflight.max(peak);
    }

    /// `submitted` more pictures submitted, saturating.
    ///
    /// # Arguments
    ///
    /// * `submitted` - pictures submitted since the last fold.
    pub fn add_submitted(&mut self, submitted: u64) {
        self.submitted = self.submitted.saturating_add(submitted);
    }

    /// `resets` more submit-core resets, saturating.
    ///
    /// # Arguments
    ///
    /// * `resets` - resets since the last fold.
    pub fn add_resets(&mut self, resets: u64) {
        self.resets = self.resets.saturating_add(resets);
    }

    /// `creates` more driver creates, saturating.
    ///
    /// # Arguments
    ///
    /// * `creates` - driver objects created since the last fold.
    pub fn add_buffer_creates(&mut self, creates: u64) {
        self.buffer_creates = self.buffer_creates.saturating_add(creates);
    }
}

impl Default for SubmitStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use super::{
        ChannelConfig, DecoderPump, DropPolicy, FrameChannel, PhaseTiming, PollFrames,
        ReceiveError, ReceiveFrames, ReceiveTimeout, SendPackets, SubmitStats, TrySendError,
    };
    use crate::packet::Packet;
    use crate::traits::Decoder;
    use crate::transcode::GpuCaps;

    use bytes::Bytes;

    /// Decode holding one frame back for `finish`, like reorder.
    struct HoldbackDecoder {
        /// Next frame id.
        next: u64,
        /// Frame held for the next call or `finish`.
        held: Option<u64>,
    }

    impl HoldbackDecoder {
        /// Decoder holding its first frame back.
        fn new() -> Self {
            Self {
                next: 0,
                held: None,
            }
        }
    }

    impl Decoder for HoldbackDecoder {
        type Frame = u64;
        type Error = Reject;

        fn caps(&self) -> GpuCaps {
            GpuCaps::new(None, false, false)
        }

        fn decode(&mut self, _packet: &Packet) -> Result<Vec<u64>, Reject> {
            let id = self.next;
            self.next += 1;
            Ok(self.held.replace(id).into_iter().collect())
        }

        fn flush(&mut self) -> Result<Vec<u64>, Reject> {
            Ok(self.held.take().into_iter().collect())
        }
    }

    /// Scripted-decode failure.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Reject;

    impl fmt::Display for Reject {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "rejected")
        }
    }

    impl std::error::Error for Reject {}

    /// Send queue refusing past its bound, unlike policy drops.
    struct TinySender {
        /// Accepted packet count.
        accepted: usize,
        /// Refusal bound.
        bound: usize,
    }

    impl TinySender {
        /// Sender refusing past `bound` packets.
        fn new(bound: usize) -> Self {
            Self { accepted: 0, bound }
        }
    }

    impl SendPackets for TinySender {
        type Error = Reject;

        fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Reject>> {
            if self.accepted >= self.bound {
                return Err(TrySendError::Full { packet });
            }
            self.accepted += 1;
            Ok(())
        }

        fn finish(&mut self) -> Result<(), Reject> {
            Ok(())
        }
    }

    /// Config of `capacity` frames under `policy`.
    fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
        ChannelConfig::new(
            NonZeroUsize::new(capacity).expect("nonzero capacity"),
            policy,
        )
    }

    /// Coded packet carrying `byte`.
    fn packet(byte: u8) -> Packet {
        Packet::new(Bytes::from(vec![byte]))
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn sends_receive_own_frames_then_drain() {
        let mut pump = DecoderPump::open(HoldbackDecoder::new(), config(8, DropPolicy::DropOldest));
        for byte in 0..4u8 {
            pump.try_send(packet(byte)).expect("queue room");
        }
        let mut ids = Vec::new();
        while let Some(id) = pump.try_receive().expect("poll") {
            ids.push(id);
        }
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(pump.dropped(), 0);
        pump.finish().expect("finish");
        assert!(pump.is_finished());
        assert_eq!(pump.receive().expect("trailing"), Some(3));
        assert_eq!(pump.receive().expect("drained"), None);
    }

    #[test]
    fn drop_oldest_keeps_latest_under_slow_consumer() {
        let mut pump = DecoderPump::open(HoldbackDecoder::new(), config(2, DropPolicy::DropOldest));
        for byte in 0..5u8 {
            pump.try_send(packet(byte)).expect("policy absorbs");
        }
        assert_eq!(pump.len(), 2);
        assert!(!pump.is_empty());
        assert_eq!(pump.dropped(), 2);
        pump.finish().expect("finish");
        assert_eq!(pump.dropped(), 3);
        let mut ids = Vec::new();
        while let Some(id) = pump.receive().expect("drain") {
            ids.push(id);
        }
        assert_eq!(ids, [3, 4]);
    }

    #[test]
    fn drop_incoming_keeps_first_under_slow_consumer() {
        let mut pump =
            DecoderPump::open(HoldbackDecoder::new(), config(2, DropPolicy::DropIncoming));
        for byte in 0..5u8 {
            pump.try_send(packet(byte)).expect("policy absorbs");
        }
        assert_eq!(pump.dropped(), 2);
        pump.finish().expect("finish");
        assert_eq!(pump.dropped(), 3);
        let mut ids = Vec::new();
        while let Some(id) = pump.receive().expect("drain") {
            ids.push(id);
        }
        assert_eq!(ids, [0, 1]);
    }

    #[test]
    fn send_after_finish_returns_packet() {
        let mut pump = DecoderPump::open(HoldbackDecoder::new(), config(4, DropPolicy::DropOldest));
        pump.finish().expect("finish");
        let err = pump.try_send(packet(9)).unwrap_err();
        let TrySendError::Full { packet } = err else {
            panic!("full after finish");
        };
        assert_eq!(packet.data(), &[9]);
    }

    #[test]
    fn finish_twice_drains_empty() {
        let mut pump =
            DecoderPump::open(HoldbackDecoder::new(), config(4, DropPolicy::DropIncoming));
        pump.finish().expect("first finish");
        pump.finish().expect("second finish");
        assert_eq!(pump.try_receive().expect("poll"), None);
        assert_eq!(pump.receive().expect("drained"), None);
    }

    #[test]
    fn backpressure_sender_returns_full_packet() {
        let mut sender = TinySender::new(1);
        sender.try_send(packet(1)).expect("room");
        let err = sender.try_send(packet(2)).unwrap_err();
        let TrySendError::Full { packet } = err else {
            panic!("full past bound");
        };
        assert_eq!(packet.data(), &[2]);
        sender.finish().expect("finish");
    }

    #[test]
    fn send_and_receive_errors_name_cause() {
        let full: TrySendError<Reject> = TrySendError::Full { packet: packet(1) };
        assert_eq!(full.to_string(), "output queue full");
        let failed: TrySendError<Reject> = TrySendError::Failed { error: Reject };
        assert_eq!(failed.to_string(), "rejected");
        let timeout: ReceiveError<Reject> = ReceiveError::Timeout;
        assert_eq!(timeout.to_string(), "receive timed out");
        let failed: ReceiveError<Reject> = ReceiveError::Failed { error: Reject };
        assert_eq!(failed.to_string(), "rejected");
        assert_eq!(ReceiveTimeout.to_string(), "receive timed out");
    }

    #[test]
    fn channel_queues_fifo_and_counts_drops() {
        let channel = FrameChannel::new(config(2, DropPolicy::DropOldest));
        assert_eq!(channel.capacity(), 2);
        assert_eq!(channel.policy(), DropPolicy::DropOldest);
        assert_eq!(channel.config().capacity(), 2);
        assert!(channel.is_empty());
        assert_eq!(channel.send(10u64), None);
        assert_eq!(channel.send(11u64), None);
        assert!(!channel.is_empty());
        assert_eq!(channel.len(), 2);
        assert_eq!(channel.send(12u64), Some(10u64));
        assert_eq!(channel.dropped(), 1);
        assert_eq!(channel.try_receive(), Some(11u64));
        assert_eq!(channel.try_receive(), Some(12u64));
        assert_eq!(channel.try_receive(), None);
        channel.close();
        assert!(channel.is_closed());
        assert_eq!(channel.send(13u64), Some(13u64));
        assert_eq!(channel.dropped(), 2);
        assert_eq!(channel.receive(), None);
    }

    #[test]
    fn blocking_receive_waits_for_producer_then_close() {
        assert_send_sync::<FrameChannel<u64>>();
        let channel = FrameChannel::new(config(4, DropPolicy::DropOldest));
        let producer = channel.clone();
        std::thread::spawn(move || {
            let _dropped = producer.send(1u64);
            let _dropped = producer.send(2u64);
            producer.close();
        });
        assert_eq!(channel.receive(), Some(1u64));
        assert_eq!(
            channel
                .receive_timeout(Duration::from_secs(5))
                .expect("ready"),
            Some(2u64)
        );
        assert_eq!(channel.receive(), None);
    }

    #[test]
    fn receive_timeout_expires_on_empty_open_channel() {
        let channel = FrameChannel::<u64>::new(config(4, DropPolicy::DropIncoming));
        let err = channel
            .receive_timeout(Duration::from_millis(1))
            .unwrap_err();
        assert_eq!(err, ReceiveTimeout);
    }

    #[test]
    fn drain_with_invokes_callback_in_order() {
        let channel = FrameChannel::new(config(4, DropPolicy::DropOldest));
        let _dropped = channel.send(1u64);
        let _dropped = channel.send(2u64);
        let _dropped = channel.send(3u64);
        let mut seen = Vec::new();
        let drained = channel.drain_with(|frame| seen.push(frame));
        assert_eq!(drained, 3);
        assert_eq!(seen, [1, 2, 3]);
        assert!(channel.is_empty());
    }

    #[test]
    fn pump_receive_timeout_maps_lapsed_budget() {
        let mut pump = DecoderPump::open(HoldbackDecoder::new(), config(4, DropPolicy::DropOldest));
        let err = pump.receive_timeout(Duration::from_millis(1)).unwrap_err();
        assert_eq!(err, ReceiveError::Timeout);
    }

    #[test]
    fn consumer_handle_shares_pump_queue() {
        let mut pump = DecoderPump::open(HoldbackDecoder::new(), config(8, DropPolicy::DropOldest));
        assert_eq!(pump.config(), config(8, DropPolicy::DropOldest));
        let consumer = pump.channel();
        for byte in 0..3u8 {
            pump.try_send(packet(byte)).expect("queue room");
        }
        assert_eq!(consumer.try_receive(), Some(0u64));
        assert_eq!(pump.try_receive().expect("poll"), Some(1u64));
        assert_eq!(pump.try_receive().expect("poll"), None);
    }

    #[test]
    fn phase_timing_reports_phases_and_derivations() {
        let submitted_at = std::time::Instant::now();
        let timing = PhaseTiming::new(
            7,
            Duration::from_micros(5),
            Duration::from_micros(11),
            Duration::from_micros(3),
            submitted_at,
            submitted_at + Duration::from_micros(40),
        );
        assert_eq!(timing.seq(), 7);
        assert_eq!(timing.submit(), Duration::from_micros(5));
        assert_eq!(timing.engine_wait(), Duration::from_micros(11));
        assert_eq!(timing.copy(), Duration::from_micros(3));
        assert_eq!(timing.submitted_at(), submitted_at);
        assert_eq!(timing.stage_sum(), Duration::from_micros(19));
        assert_eq!(timing.submit_us(), 5);
        assert_eq!(timing.engine_wait_us(), 11);
        assert_eq!(timing.copy_us(), 3);
        assert_eq!(timing.total_us(), 19);
        assert_eq!(timing.latency_us(), 40);
        assert_eq!(timing.submitted_pts_us(), None);
        assert_eq!(timing.completed_pts_us(), None);
    }

    #[test]
    fn phase_timing_stamps_correlate() {
        let submitted_at = std::time::Instant::now();
        let timing = PhaseTiming::new(
            0,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            submitted_at,
            submitted_at,
        )
        .with_pts(1_000, 1_000);
        assert_eq!(timing.submitted_pts_us(), Some(1_000));
        assert_eq!(timing.completed_pts_us(), Some(1_000));
    }

    #[test]
    fn submit_stats_fold_accumulates() {
        let mut stats = SubmitStats::new();
        assert_eq!(stats, SubmitStats::default());
        assert_eq!(stats.submitted(), 0);
        assert_eq!(stats.peak_inflight(), 0);
        assert_eq!(stats.resets(), 0);
        assert_eq!(stats.buffer_creates(), 0);
        stats.add_submitted(3);
        stats.fold_peak(2);
        stats.fold_peak(1);
        stats.add_resets(1);
        stats.add_buffer_creates(7);
        assert_eq!(
            stats,
            SubmitStats::snapshot(3, 2, 1, 7),
            "folds match the snapshot"
        );
    }

    #[test]
    fn split_decode_shape_compiles() {
        use super::{SplitDecode, SubmitStats};
        use crate::frame::CpuFrame;
        use crate::packet::Packet;
        struct Dummy;
        impl SplitDecode for Dummy {
            type Frame = CpuFrame;
            type Error = std::io::Error;
            fn try_submit(&mut self, _packet: &Packet) -> Result<bool, Self::Error> {
                Ok(true)
            }
            fn poll(&mut self) -> Result<Vec<CpuFrame>, Self::Error> {
                Ok(Vec::new())
            }
            fn poll_wait(
                &mut self,
                _timeout: std::time::Duration,
            ) -> Result<Vec<CpuFrame>, Self::Error> {
                Ok(Vec::new())
            }
            fn flush_split(&mut self) -> Result<Vec<CpuFrame>, Self::Error> {
                Ok(Vec::new())
            }
            fn stats(&self) -> SubmitStats {
                SubmitStats::new()
            }
        }
        fn assert_split_decode<T: SplitDecode>() {}
        assert_split_decode::<Dummy>();
    }
}
