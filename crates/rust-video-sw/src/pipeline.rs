//! Pipelined multi-inflight decode over split-phase engines.
//!
//! Sends queue input without waiting for pictures; one worker converts
//! ready pictures while the caller submits more.

use std::collections::VecDeque;
use std::mem;
use std::num::NonZeroUsize;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rust_video_caps::delivery::{
    ChannelConfig, FrameChannel, PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::packet::Packet;

/// Packet sink plus raw-picture source behind one engine.
pub trait RawSource {
    /// Owned picture handed to the conversion worker.
    type Raw: Send + 'static;
    /// Converted frame handed to consumers.
    type Frame: Send + 'static;
    /// Submit, poll, or conversion failure.
    type Error: std::error::Error + Send + 'static;

    /// Input frames from `packet` queued; pictures freed only to make
    /// room surface in `freed`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the packet is rejected or a freed picture is unusable.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    /// * `freed` - pictures freed to make room.
    fn submit_raw(
        &mut self,
        packet: &Packet,
        freed: &mut Vec<Self::Raw>,
    ) -> Result<(), Self::Error>;

    /// Pictures ready now, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a ready picture breaks mid-decode.
    ///
    /// # Arguments
    ///
    /// * `out` - pictures ready now, oldest first.
    fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), Self::Error>;

    /// Parser tail plus held pictures; resets the stream.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the tail breaks mid-decode.
    fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, Self::Error>;

    /// Converted frame behind one raw picture.
    ///
    /// Runs on the conversion worker; one call at a time, in post order.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the picture cannot form a frame.
    fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error>;
}

/// Submit/poll decode behind one pipelined engine.
pub trait SplitDecoder {
    /// Decoded frame type.
    type Frame;
    /// Submit, poll, or conversion failure.
    type Error: std::error::Error;

    /// `packet` queued without waiting for its pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the packet is rejected or a freed picture is unusable.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    ///
    /// # Returns
    ///
    /// False when conversions saturate the depth; the packet stays
    /// unconsumed, receive then resend.
    fn try_submit(&mut self, packet: &Packet) -> Result<bool, Self::Error>;

    /// Pictures completed since the last poll, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` on the first failed conversion, dropping staged frames.
    fn poll(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Pictures completed within `timeout`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` on the first failed conversion, dropping staged frames.
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
    /// Returns `Err` on the first failed conversion, dropping staged frames.
    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Phase timing plus depth behind the session so far.
    fn stats(&self) -> PhaseStats;
}

/// FIFO conversion worker mapping raw pictures to frames.
///
/// The worker exits when the converter drops.
struct Converter<J: Send + 'static, F: Send + 'static> {
    /// Job intake; closing it stops the worker.
    jobs: Sender<(u64, J)>,
    /// Finished outputs in submission order.
    done: Receiver<(u64, F, PhaseSpan)>,
    /// Finished outputs awaiting drain, oldest first.
    staged: VecDeque<F>,
    /// Next submission sequence.
    next: u64,
    /// Next expected completion sequence.
    expect: u64,
    /// Jobs submitted minus outputs collected.
    outstanding: usize,
    /// Peak outstanding observed.
    max_outstanding: usize,
    /// Worker span per collected output.
    spans: Vec<PhaseSpan>,
}

impl<J: Send + 'static, F: Send + 'static> Converter<J, F> {
    /// Worker mapping each job through `f` in submission order.
    ///
    /// # Panics
    ///
    /// Panics when the OS refuses the worker thread.
    ///
    /// # Arguments
    ///
    /// * `pace` - idle delay before each job; zero disables.
    /// * `f` - job mapping; runs on the worker.
    fn spawn(pace: Duration, f: impl FnMut(J) -> F + Send + 'static) -> Self {
        let (jobs, input) = mpsc::channel();
        let (output, done) = mpsc::channel();
        thread::Builder::new()
            .name(String::from("sw-convert"))
            .spawn(move || {
                let mut f = f;
                for (seq, job) in input {
                    let start = Instant::now();
                    if !pace.is_zero() {
                        thread::sleep(pace);
                    }
                    let frame = f(job);
                    let span = PhaseSpan {
                        start,
                        end: Instant::now(),
                    };
                    let _ = output.send((seq, frame, span));
                }
            })
            .expect("conversion worker spawns");
        Self {
            jobs,
            done,
            staged: VecDeque::new(),
            next: 0,
            expect: 0,
            outstanding: 0,
            max_outstanding: 0,
            spans: Vec::new(),
        }
    }

    /// Job queued for conversion.
    fn post(&mut self, job: J) {
        let seq = self.next;
        self.next += 1;
        self.outstanding += 1;
        self.max_outstanding = self.max_outstanding.max(self.outstanding);
        let sent = self.jobs.send((seq, job)).is_ok();
        debug_assert!(sent, "worker alive");
    }

    /// Finished outputs staged; count newly staged.
    fn harvest(&mut self) -> usize {
        let mut fresh = 0;
        while let Ok((seq, frame, span)) = self.done.try_recv() {
            self.stage(seq, frame, span);
            fresh += 1;
        }
        fresh
    }

    /// Finished outputs staged, waiting up to `timeout` for one.
    ///
    /// Sleeps the budget when nothing runs; the caller re-polls its engine.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when nothing staged.
    ///
    /// # Returns
    ///
    /// Count newly staged.
    fn harvest_wait(&mut self, timeout: Duration) -> usize {
        if self.has_staged() {
            return 0;
        }
        if self.outstanding == 0 {
            thread::sleep(timeout);
            return 0;
        }
        match self.done.recv_timeout(timeout) {
            Ok((seq, frame, span)) => {
                self.stage(seq, frame, span);
                self.harvest() + 1
            }
            Err(_) => self.harvest(),
        }
    }

    /// Every outstanding output awaited and staged; count newly staged.
    fn collect_all(&mut self) -> usize {
        let before = self.spans.len();
        while self.outstanding > 0 {
            match self.done.recv() {
                Ok((seq, frame, span)) => self.stage(seq, frame, span),
                Err(_) => break,
            }
        }
        self.spans.len() - before
    }

    /// One finished output staged.
    fn stage(&mut self, seq: u64, frame: F, span: PhaseSpan) {
        debug_assert_eq!(seq, self.expect, "worker answers in order");
        debug_assert!(self.outstanding > 0, "receipt matches a post");
        self.expect += 1;
        self.outstanding -= 1;
        self.spans.push(span);
        self.staged.push_back(frame);
    }

    /// Jobs submitted minus outputs collected.
    fn outstanding(&self) -> usize {
        self.outstanding
    }

    /// Peak outstanding observed.
    fn max_outstanding(&self) -> usize {
        self.max_outstanding
    }

    /// Worker span per collected output.
    fn spans(&self) -> &[PhaseSpan] {
        &self.spans
    }

    /// True when finished outputs await drain.
    fn has_staged(&self) -> bool {
        !self.staged.is_empty()
    }

    /// Every staged output, oldest first.
    fn drain(&mut self) -> Vec<F> {
        mem::take(&mut self.staged).into_iter().collect()
    }
}

/// One engine harvest behind the session.
#[derive(Clone, Copy, Debug)]
pub struct EnginePoll {
    /// Harvest interval.
    span: PhaseSpan,
    /// Raw pictures harvested.
    raws: u32,
}

impl EnginePoll {
    /// Harvest length.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.span.elapsed()
    }

    /// Raw pictures harvested.
    #[must_use]
    pub fn raws(&self) -> u32 {
        self.raws
    }

    /// True when the harvest shares interior time with `span`.
    #[must_use]
    pub fn hits(&self, span: &PhaseSpan) -> bool {
        self.span.hits(span)
    }
}

/// Measured interval behind one phase sample.
#[derive(Clone, Copy, Debug)]
pub struct PhaseSpan {
    /// Interval start.
    start: Instant,
    /// Interval end.
    end: Instant,
}

impl PhaseSpan {
    /// True when the intervals share interior time.
    #[must_use]
    pub fn hits(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// End minus start.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.end.saturating_duration_since(self.start)
    }
}

/// Phase timing plus depth behind one pipelined session.
#[derive(Clone, Debug, Default)]
pub struct PhaseStats {
    /// Packets accepted.
    submitted: u64,
    /// Frames yielded.
    completed: u64,
    /// Peak conversions in flight.
    max_inflight: usize,
    /// A conversion shared interior time with a submit or engine harvest.
    overlap: bool,
    /// Submit interval per accepted packet.
    submit: Vec<PhaseSpan>,
    /// Engine harvest per poll.
    engine: Vec<EnginePoll>,
    /// Conversion interval per collected frame.
    copy: Vec<PhaseSpan>,
    /// First submit to first yielded frame.
    first_frame: Option<Duration>,
    /// First submit to last yielded frame.
    wall: Option<Duration>,
}

impl PhaseStats {
    /// Packets accepted.
    #[must_use]
    pub fn submitted(&self) -> u64 {
        self.submitted
    }

    /// Frames yielded.
    #[must_use]
    pub fn completed(&self) -> u64 {
        self.completed
    }

    /// Peak conversions in flight.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.max_inflight
    }

    /// True when a conversion shared interior time with a submit or
    /// engine harvest.
    #[must_use]
    pub fn overlap_observed(&self) -> bool {
        self.overlap
    }

    /// Submit interval per accepted packet.
    #[must_use]
    pub fn submit_spans(&self) -> &[PhaseSpan] {
        &self.submit
    }

    /// Engine harvest per poll.
    #[must_use]
    pub fn engine_polls(&self) -> &[EnginePoll] {
        &self.engine
    }

    /// Conversion interval per collected frame.
    #[must_use]
    pub fn copy_spans(&self) -> &[PhaseSpan] {
        &self.copy
    }

    /// First submit to first yielded frame.
    #[must_use]
    pub fn first_frame(&self) -> Option<Duration> {
        self.first_frame
    }

    /// First submit to last yielded frame.
    #[must_use]
    pub fn wall(&self) -> Option<Duration> {
        self.wall
    }

    /// Submit intervals summed.
    #[must_use]
    pub fn submit_total(&self) -> Duration {
        self.submit.iter().map(PhaseSpan::elapsed).sum()
    }

    /// Engine harvests summed.
    #[must_use]
    pub fn engine_total(&self) -> Duration {
        self.engine.iter().map(EnginePoll::elapsed).sum()
    }

    /// Conversion intervals summed.
    #[must_use]
    pub fn copy_total(&self) -> Duration {
        self.copy.iter().map(PhaseSpan::elapsed).sum()
    }
}

/// Bounds behind a pipelined session.
#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    /// Conversions in flight before sends refuse.
    depth: NonZeroUsize,
    /// Idle delay before each conversion; zero disables.
    pace: Duration,
}

impl PipelineConfig {
    /// Default conversions in flight.
    pub const DEFAULT_DEPTH: usize = 4;

    /// Bounds holding at most `depth` conversions in flight.
    ///
    /// Sends refuse while outstanding conversions reach `depth`;
    /// completions already freed post regardless.
    ///
    /// # Arguments
    ///
    /// * `depth` - conversions in flight before sends refuse.
    #[must_use]
    pub fn new(depth: NonZeroUsize) -> Self {
        Self {
            depth,
            pace: Duration::ZERO,
        }
    }

    /// Bounds pacing each conversion behind `pace`.
    ///
    /// # Arguments
    ///
    /// * `pace` - idle delay before each conversion; zero disables.
    #[must_use]
    pub fn with_pace(self, pace: Duration) -> Self {
        Self { pace, ..self }
    }

    /// Conversions in flight before sends refuse.
    #[must_use]
    pub fn depth(&self) -> NonZeroUsize {
        self.depth
    }

    /// Idle delay before each conversion; zero disables.
    #[must_use]
    pub fn pace(&self) -> Duration {
        self.pace
    }
}

impl Default for PipelineConfig {
    /// Bounds at [`PipelineConfig::DEFAULT_DEPTH`] with pacing disabled.
    ///
    /// # Panics
    ///
    /// Panics when the default depth is zero.
    fn default() -> Self {
        Self {
            depth: NonZeroUsize::new(Self::DEFAULT_DEPTH).expect("nonzero default depth"),
            pace: Duration::ZERO,
        }
    }
}

/// Conversion function behind one raw source.
type ConvertFn<I> =
    fn(<I as RawSource>::Raw) -> Result<<I as RawSource>::Frame, <I as RawSource>::Error>;

/// Split-phase decode over one engine plus a conversion worker.
///
/// `Send`, never `Sync`: hand the decoder to another thread when idle,
/// but run every call on one thread.
pub struct PipelinedDecoder<I: RawSource> {
    /// Packet sink plus raw-picture source.
    inner: I,
    /// FIFO conversion worker.
    convert: Converter<I::Raw, Result<I::Frame, I::Error>>,
    /// Bounds behind the session.
    config: PipelineConfig,
    /// Packets accepted.
    submitted: u64,
    /// Frames yielded.
    completed: u64,
    /// A conversion shared interior time with a submit or engine harvest.
    overlap: bool,
    /// Submit interval per accepted packet.
    submit: Vec<PhaseSpan>,
    /// Engine harvest per poll.
    engine: Vec<EnginePoll>,
    /// First submit instant.
    first_submit: Option<Instant>,
    /// First yielded frame instant.
    first_yield: Option<Instant>,
    /// Last yielded frame instant.
    last_yield: Option<Instant>,
}

impl<I: RawSource> PipelinedDecoder<I> {
    /// Engine re-poll quantum behind bounded waits.
    const POLL_QUANTUM: Duration = Duration::from_millis(1);

    /// Split-phase decode over `inner` under `config`.
    ///
    /// # Panics
    ///
    /// Panics when the OS refuses the conversion worker thread.
    ///
    /// # Arguments
    ///
    /// * `inner` - packet sink plus raw-picture source.
    /// * `config` - bounds behind the session.
    pub fn open(inner: I, config: PipelineConfig) -> Self {
        let convert_fn: ConvertFn<I> = I::convert;
        let convert = Converter::spawn(config.pace(), convert_fn);
        Self {
            inner,
            convert,
            config,
            submitted: 0,
            completed: 0,
            overlap: false,
            submit: Vec::new(),
            engine: Vec::new(),
            first_submit: None,
            first_yield: None,
            last_yield: None,
        }
    }

    /// Bounds behind the session.
    #[must_use]
    pub fn config(&self) -> PipelineConfig {
        self.config
    }

    /// Engine harvest posted for conversion.
    fn engine_poll(&mut self) -> Result<(), I::Error> {
        let start = Instant::now();
        let mut raws = Vec::new();
        self.inner.poll_raw(&mut raws)?;
        let poll = EnginePoll {
            span: PhaseSpan {
                start,
                end: Instant::now(),
            },
            raws: u32::try_from(raws.len()).unwrap_or(u32::MAX),
        };
        for raw in raws {
            self.convert.post(raw);
        }
        self.note_engine(poll);
        Ok(())
    }

    /// Engine harvest recorded plus checked against conversions.
    fn note_engine(&mut self, poll: EnginePoll) {
        if !self.overlap && self.convert.spans().iter().any(|span| poll.hits(span)) {
            self.overlap = true;
        }
        self.engine.push(poll);
    }

    /// Submit interval recorded plus checked against conversions.
    fn note_submit(&mut self, span: PhaseSpan) {
        if !self.overlap && self.convert.spans().iter().any(|copy| span.hits(copy)) {
            self.overlap = true;
        }
        self.submit.push(span);
    }

    /// Fresh conversion spans checked against submits and harvests.
    fn note_copies(&mut self, before: usize, fresh: usize) {
        if self.overlap || fresh == 0 {
            return;
        }
        let hit = self.convert.spans().iter().skip(before).any(|copy| {
            self.submit.iter().any(|span| span.hits(copy))
                || self.engine.iter().any(|poll| poll.hits(copy))
        });
        self.overlap = hit;
    }

    /// Finished outputs staged plus checked against submits and harvests.
    fn harvest(&mut self) {
        let before = self.convert.spans().len();
        let fresh = self.convert.harvest();
        self.note_copies(before, fresh);
    }

    /// Staged frames yielded, oldest first.
    ///
    /// # Errors
    ///
    /// Returns `Err` on the first failed conversion, dropping staged frames.
    fn drain(&mut self) -> Result<Vec<I::Frame>, I::Error> {
        let mut frames = Vec::new();
        for output in self.convert.drain() {
            frames.push(output?);
        }
        if !frames.is_empty() {
            let now = Instant::now();
            self.first_yield.get_or_insert(now);
            self.last_yield = Some(now);
            self.completed += u64::try_from(frames.len()).unwrap_or(u64::MAX);
        }
        Ok(frames)
    }
}

impl<I: RawSource> SplitDecoder for PipelinedDecoder<I> {
    type Frame = I::Frame;
    type Error = I::Error;

    fn try_submit(&mut self, packet: &Packet) -> Result<bool, Self::Error> {
        self.harvest();
        self.engine_poll()?;
        if self.convert.outstanding() >= self.config.depth().get() {
            return Ok(false);
        }
        self.first_submit.get_or_insert_with(Instant::now);
        let start = Instant::now();
        let mut freed = Vec::new();
        self.inner.submit_raw(packet, &mut freed)?;
        self.note_submit(PhaseSpan {
            start,
            end: Instant::now(),
        });
        self.submitted += 1;
        for raw in freed {
            self.convert.post(raw);
        }
        Ok(true)
    }

    fn poll(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.engine_poll()?;
        self.harvest();
        self.drain()
    }

    fn poll_wait(&mut self, timeout: Duration) -> Result<Vec<Self::Frame>, Self::Error> {
        let deadline = Instant::now() + timeout;
        loop {
            self.engine_poll()?;
            self.harvest();
            if self.convert.has_staged() {
                return self.drain();
            }
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let before = self.convert.spans().len();
            let fresh = self.convert.harvest_wait(remaining.min(Self::POLL_QUANTUM));
            self.note_copies(before, fresh);
        }
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        for raw in self.inner.flush_raw()? {
            self.convert.post(raw);
        }
        let before = self.convert.spans().len();
        let fresh = self.convert.collect_all();
        self.note_copies(before, fresh);
        self.drain()
    }

    fn stats(&self) -> PhaseStats {
        let first_frame = self
            .first_submit
            .zip(self.first_yield)
            .map(|(start, end)| end.saturating_duration_since(start));
        let wall = self
            .first_submit
            .zip(self.last_yield)
            .map(|(start, end)| end.saturating_duration_since(start));
        PhaseStats {
            submitted: self.submitted,
            completed: self.completed,
            max_inflight: self.convert.max_outstanding(),
            overlap: self.overlap,
            submit: self.submit.clone(),
            engine: self.engine.clone(),
            copy: self.convert.spans().to_vec(),
            first_frame,
            wall,
        }
    }
}

/// Bounded multi-inflight decode session over a split decoder.
pub struct PipelinedPump<D: SplitDecoder> {
    /// Split-phase decode behind sends.
    decoder: D,
    /// Bounded output with drop policy.
    channel: FrameChannel<D::Frame>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
}

impl<D: SplitDecoder> PipelinedPump<D> {
    /// Bounded wait behind blocking receive.
    const RECEIVE_QUANTUM: Duration = Duration::from_secs(1);

    /// Pump decoding through `decoder` into `config` frames of queue.
    ///
    /// # Arguments
    ///
    /// * `decoder` - split-phase decode behind sends.
    /// * `config` - bound plus drop policy.
    #[must_use]
    pub fn open(decoder: D, config: ChannelConfig) -> Self {
        Self {
            decoder,
            channel: FrameChannel::new(config),
            finished: false,
        }
    }

    /// Split-phase decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }

    /// Phase timing plus depth behind the session so far.
    #[must_use]
    pub fn stats(&self) -> PhaseStats {
        self.decoder.stats()
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

    /// Completions queued for consumers.
    fn harvest(&mut self) -> Result<(), D::Error> {
        for frame in self.decoder.poll()? {
            let _dropped = self.channel.send(frame);
        }
        Ok(())
    }

    /// Completions awaited up to `timeout`, then queued for consumers.
    fn harvest_wait(&mut self, timeout: Duration) -> Result<(), D::Error> {
        for frame in self.decoder.poll_wait(timeout)? {
            let _dropped = self.channel.send(frame);
        }
        Ok(())
    }
}

impl<D: SplitDecoder> SendPackets for PipelinedPump<D> {
    type Error = D::Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        let accepted = self
            .decoder
            .try_submit(&packet)
            .map_err(|error| TrySendError::Failed { error })?;
        if !accepted {
            return Err(TrySendError::Full { packet });
        }
        self.harvest()
            .map_err(|error| TrySendError::Failed { error })?;
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

impl<D: SplitDecoder> PollFrames for PipelinedPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.harvest()?;
        Ok(self.channel.try_receive())
    }
}

impl<D: SplitDecoder> ReceiveFrames for PipelinedPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        loop {
            self.harvest()?;
            if let Some(frame) = self.channel.try_receive() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            self.harvest_wait(Self::RECEIVE_QUANTUM)?;
        }
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        self.harvest()
            .map_err(|error| ReceiveError::Failed { error })?;
        if let Some(frame) = self.channel.try_receive() {
            return Ok(Some(frame));
        }
        if self.finished {
            return Ok(None);
        }
        self.harvest_wait(timeout)
            .map_err(|error| ReceiveError::Failed { error })?;
        if let Some(frame) = self.channel.try_receive() {
            return Ok(Some(frame));
        }
        Err(ReceiveError::Timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::num::NonZeroUsize;
    use std::thread;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::delivery::{
        ChannelConfig, DropPolicy, PollFrames, Pump, ReceiveError, ReceiveFrames, SendPackets,
        TrySendError,
    };
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;

    use super::{
        Converter, PipelineConfig, PipelinedDecoder, PipelinedPump, RawSource, SplitDecoder,
    };
    use crate::error::SwError;

    /// Scripted raw source stamping each job id into its frame.
    struct Scripted {
        /// Next job id.
        next: u64,
        /// Jobs submitted minus jobs released.
        pending: Vec<Result<u64, SwError>>,
        /// Job failing conversion, if any.
        fail: Option<u64>,
    }

    impl Scripted {
        /// Engine holding two jobs back, releasing the rest per submit.
        fn new() -> Self {
            Self {
                next: 0,
                pending: Vec::new(),
                fail: None,
            }
        }

        /// Engine failing conversion of `job`.
        fn failing(job: u64) -> Self {
            Self {
                next: 0,
                pending: Vec::new(),
                fail: Some(job),
            }
        }

        /// 4x2 frame stamped with `id` in its first luma byte.
        fn stamped(id: u64) -> CpuFrame {
            let size = PictureSize::new(4, 2).expect("extent");
            let mut y = vec![0u8; 8];
            y[0] = u8::try_from(id % 256).expect("modulo byte");
            let planes = vec![
                Plane::new(Bytes::from(y), 4).expect("luma"),
                Plane::new(Bytes::from(vec![0u8; 2]), 2).expect("chroma u"),
                Plane::new(Bytes::from(vec![0u8; 2]), 2).expect("chroma v"),
            ];
            CpuFrame::new(size, PixelFormat::Yuv420P, planes).expect("frame")
        }
    }

    impl RawSource for Scripted {
        type Raw = Result<u64, SwError>;
        type Frame = CpuFrame;
        type Error = SwError;

        fn submit_raw(
            &mut self,
            _packet: &Packet,
            freed: &mut Vec<Self::Raw>,
        ) -> Result<(), SwError> {
            let id = self.next;
            self.next += 1;
            if Some(id) == self.fail {
                self.pending.push(Err(SwError::Rejected));
            } else {
                self.pending.push(Ok(id));
            }
            while self.pending.len() > 2 {
                freed.push(self.pending.remove(0));
            }
            Ok(())
        }

        fn poll_raw(&mut self, out: &mut Vec<Self::Raw>) -> Result<(), SwError> {
            out.append(&mut self.pending);
            Ok(())
        }

        fn flush_raw(&mut self) -> Result<Vec<Self::Raw>, SwError> {
            Ok(mem::take(&mut self.pending))
        }

        fn convert(raw: Self::Raw) -> Result<Self::Frame, Self::Error> {
            raw.map(Self::stamped)
        }
    }

    impl Decoder for Scripted {
        type Frame = CpuFrame;
        type Error = SwError;

        fn decode(&mut self, packet: &Packet) -> Result<Vec<CpuFrame>, SwError> {
            let mut raws = Vec::new();
            self.submit_raw(packet, &mut raws)?;
            self.poll_raw(&mut raws)?;
            raws.into_iter().map(Self::convert).collect()
        }

        fn flush(&mut self) -> Result<Vec<CpuFrame>, SwError> {
            self.flush_raw()?.into_iter().map(Self::convert).collect()
        }
    }

    /// Bound of `capacity` frames under `policy`.
    fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
        ChannelConfig::new(
            NonZeroUsize::new(capacity).expect("nonzero capacity"),
            policy,
        )
    }

    /// Pipeline bounds at `depth` with `pace` per conversion.
    fn pipeline(depth: usize, pace: Duration) -> PipelineConfig {
        PipelineConfig::new(NonZeroUsize::new(depth).expect("nonzero depth")).with_pace(pace)
    }

    /// Coded packet carrying `byte`.
    fn packet(byte: u8) -> Packet {
        Packet::new(Bytes::from(vec![byte]))
    }

    /// Job id stamped in `frame`.
    fn stamped(frame: &CpuFrame) -> u8 {
        frame.planes()[0].data()[0]
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn decoder_and_pump_move_between_threads() {
        assert_send::<PipelinedDecoder<Scripted>>();
        assert_send::<PipelinedPump<PipelinedDecoder<Scripted>>>();
    }

    #[test]
    fn pump_preserves_bytes_and_order_vs_sync() {
        let packets: Vec<Packet> = (0..6u8).map(packet).collect();
        let mut sync = Pump::new(Scripted::new(), config(16, DropPolicy::DropOldest));
        for packet in &packets {
            sync.try_send(packet.clone()).expect("queue room");
        }
        sync.finish().expect("finish drains");
        let mut expected = Vec::new();
        while let Some(frame) = sync.receive().expect("drain") {
            expected.push(frame);
        }
        let decoder = PipelinedDecoder::open(Scripted::new(), pipeline(64, Duration::ZERO));
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        assert_eq!(pump.config(), config(16, DropPolicy::DropOldest));
        assert!(pump.is_empty());
        assert!(!pump.is_finished());
        for packet in &packets {
            pump.try_send(packet.clone()).expect("depth room");
        }
        let mut frames = Vec::new();
        while let Some(frame) = pump.try_receive().expect("poll") {
            frames.push(frame);
        }
        pump.finish().expect("finish drains");
        assert!(pump.is_finished());
        while let Some(frame) = pump.receive().expect("drain") {
            frames.push(frame);
        }
        assert_eq!(pump.dropped(), 0);
        assert_eq!(frames.len(), expected.len());
        assert_eq!(frames.len(), packets.len());
        for (frame, want) in frames.iter().zip(expected.iter()) {
            assert_eq!(stamped(frame), stamped(want));
            for (plane, goal) in frame.planes().iter().zip(want.planes().iter()) {
                assert_eq!(plane.data(), goal.data());
            }
        }
        let stats = pump.stats();
        assert_eq!(stats.submitted(), 6);
        assert_eq!(stats.completed(), 6);
        assert_eq!(stats.submit_spans().len(), 6);
        assert_eq!(stats.copy_spans().len(), 6);
        assert!(stats.first_frame().is_some());
        assert!(stats.wall().is_some());
    }

    #[test]
    fn sends_refuse_past_depth_then_recover() {
        let pace = Duration::from_millis(100);
        let decoder = PipelinedDecoder::open(Scripted::new(), pipeline(2, pace));
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        pump.try_send(packet(0)).expect("depth room");
        pump.try_send(packet(1)).expect("depth room");
        let err = pump.try_send(packet(2)).expect_err("depth refused");
        let TrySendError::Full { packet: refused } = err else {
            panic!("full past depth");
        };
        assert_eq!(refused.data(), &[2]);
        assert_eq!(pump.stats().max_inflight(), 2);
        let mut frames = Vec::new();
        while let Some(frame) = pump.receive_timeout(Duration::from_secs(5)).expect("wait") {
            frames.push(frame);
            if frames.len() == 2 {
                break;
            }
        }
        assert_eq!(frames.len(), 2);
        pump.try_send(packet(2)).expect("depth freed");
        pump.finish().expect("finish drains");
        while let Some(frame) = pump.receive().expect("drain") {
            frames.push(frame);
        }
        assert_eq!(frames.len(), 3);
        for (id, frame) in frames.iter().enumerate() {
            assert_eq!(stamped(frame), u8::try_from(id).expect("small id"));
        }
    }

    #[test]
    fn paced_conversions_share_time_with_submits() {
        let pace = Duration::from_millis(20);
        let mut decoder = PipelinedDecoder::open(Scripted::new(), pipeline(8, pace));
        for byte in 0..4u8 {
            assert!(
                decoder.try_submit(&packet(byte)).expect("submit"),
                "depth room"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let frames = decoder.flush().expect("flush drains");
        assert_eq!(frames.len(), 4);
        let stats = decoder.stats();
        assert!(stats.max_inflight() >= 2, "backlog observed");
        assert!(stats.overlap_observed(), "submit shared conversion time");
    }

    #[test]
    fn failed_conversion_fails_poll_and_drops_staged() {
        let pace = Duration::from_millis(50);
        let mut decoder = PipelinedDecoder::open(Scripted::failing(1), pipeline(8, pace));
        for byte in 0..3u8 {
            assert!(
                decoder.try_submit(&packet(byte)).expect("submit"),
                "depth room"
            );
        }
        let first = decoder
            .poll_wait(Duration::from_secs(5))
            .expect("first poll");
        assert_eq!(first.len(), 1);
        assert_eq!(stamped(&first[0]), 0);
        let err = decoder
            .poll_wait(Duration::from_secs(5))
            .expect_err("poisoned poll");
        assert_eq!(err, SwError::Rejected);
        let rest = decoder
            .poll_wait(Duration::from_secs(5))
            .expect("last poll");
        assert_eq!(rest.len(), 1);
        assert_eq!(stamped(&rest[0]), 2);
        assert_eq!(decoder.stats().completed(), 2);
    }

    #[test]
    fn poll_wait_times_out_on_idle_open_decoder() {
        let mut decoder = PipelinedDecoder::open(Scripted::new(), PipelineConfig::default());
        let start = Instant::now();
        let frames = decoder.poll_wait(Duration::from_millis(10)).expect("poll");
        assert_eq!(frames.len(), 0);
        assert!(start.elapsed() >= Duration::from_millis(10), "budget slept");
    }

    #[test]
    fn converter_preserves_fifo_order_under_pace() {
        let mut convert = Converter::spawn(Duration::from_millis(5), |job: u64| job * 2);
        for job in 0..5u64 {
            convert.post(job);
        }
        assert_eq!(convert.max_outstanding(), 5);
        assert_eq!(convert.collect_all(), 5);
        assert_eq!(convert.drain(), vec![0, 2, 4, 6, 8]);
        assert_eq!(convert.spans().len(), 5);
    }

    #[test]
    fn blocking_receive_gets_frames_then_end() {
        let decoder = PipelinedDecoder::open(Scripted::new(), PipelineConfig::default());
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        for byte in 0..3u8 {
            pump.try_send(packet(byte)).expect("depth room");
        }
        pump.finish().expect("finish drains");
        let mut ids = Vec::new();
        while let Some(frame) = pump.receive().expect("drain") {
            ids.push(stamped(&frame));
        }
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[test]
    fn receive_timeout_maps_lapsed_budget_and_close() {
        let decoder = PipelinedDecoder::open(Scripted::new(), PipelineConfig::default());
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        let err = pump
            .receive_timeout(Duration::from_millis(10))
            .expect_err("lapsed budget");
        assert_eq!(err, ReceiveError::Timeout);
        pump.finish().expect("finish drains");
        assert_eq!(
            pump.receive_timeout(Duration::from_millis(1))
                .expect("drained"),
            None
        );
    }

    #[test]
    fn finish_is_idempotent_and_refuses_sends() {
        let decoder = PipelinedDecoder::open(Scripted::new(), PipelineConfig::default());
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        pump.finish().expect("first finish");
        pump.finish().expect("second finish");
        let err = pump.try_send(packet(9)).expect_err("full after finish");
        let TrySendError::Full { packet } = err else {
            panic!("full after finish");
        };
        assert_eq!(packet.data(), &[9]);
    }

    #[test]
    fn consumer_handle_shares_pump_queue() {
        let decoder = PipelinedDecoder::open(Scripted::new(), PipelineConfig::default());
        let mut pump = PipelinedPump::open(decoder, config(16, DropPolicy::DropOldest));
        let consumer = pump.channel();
        for byte in 0..3u8 {
            pump.try_send(packet(byte)).expect("depth room");
        }
        pump.finish().expect("finish drains");
        assert_eq!(stamped(&consumer.try_receive().expect("shared queue")), 0);
        assert_eq!(
            stamped(&pump.try_receive().expect("poll").expect("frame queued")),
            1
        );
        assert_eq!(pump.len(), 1);
        assert_eq!(pump.dropped(), 0);
    }
}
