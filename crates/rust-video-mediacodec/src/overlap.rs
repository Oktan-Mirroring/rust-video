//! Bounded multi-inflight decode accounting.
//!
//! One ledger per decoder session; every queued input notes its submit,
//! every drained output notes its completion, matched FIFO.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rust_video_caps::delivery::PhaseTiming;
use rust_video_core::micros;

/// Single non-blocking queue attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitOutcome {
    /// Payload reached an input buffer.
    Queued,
    /// No input buffer free, or the in-flight bound holds.
    Starved,
}

/// Queued input awaiting its output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingSubmit {
    /// Submission order.
    seq: u64,
    /// Input stamp in microseconds.
    pts_us: u64,
    /// Queue call return.
    submitted_at: Instant,
    /// Queue call duration in microseconds.
    submit_us: u64,
}

/// Bounded in-flight submissions plus per-frame phase timings.
#[derive(Debug)]
pub struct PipelineLedger {
    /// In-flight ceiling.
    depth: usize,
    /// Queued inputs awaiting outputs, oldest first.
    pending: VecDeque<PendingSubmit>,
    /// Completed phase timings, oldest first, newest 1024.
    timings: VecDeque<PhaseTiming>,
    /// Largest queue seen.
    max_in_flight: usize,
    /// Next submission order.
    next_seq: u64,
}

impl PipelineLedger {
    /// In-flight ceiling: four queued inputs.
    ///
    /// The device codec pipelines internally across queue and dequeue;
    /// four outstanding inputs keep it fed while the drain frees outputs.
    /// Input buffer counts below four surface as `Starved`, never as waits.
    pub const DEFAULT_DEPTH: usize = 4;

    /// Smallest bound that still overlaps copy with decode.
    const MIN_DEPTH: usize = 2;

    /// Largest bound; deeper queues only age outputs.
    const MAX_DEPTH: usize = 4;

    /// Retained timings; older completions evict first.
    const MAX_TIMINGS: usize = 1024;

    /// Ledger holding `depth` inputs, clamped to 2..=4.
    ///
    /// # Arguments
    ///
    /// * `depth` - in-flight ceiling.
    #[must_use]
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.clamp(Self::MIN_DEPTH, Self::MAX_DEPTH),
            pending: VecDeque::new(),
            timings: VecDeque::new(),
            max_in_flight: 0,
            next_seq: 0,
        }
    }

    /// Microseconds since `since`, saturating.
    ///
    /// # Arguments
    ///
    /// * `since` - phase start.
    #[must_use]
    pub fn elapsed_us(since: Instant) -> u64 {
        micros(since.elapsed())
    }

    /// In-flight ceiling.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Inputs awaiting outputs.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Largest queue seen.
    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    /// Completed phase timings, oldest first, newest 1024.
    #[must_use]
    pub fn timings(&self) -> &VecDeque<PhaseTiming> {
        &self.timings
    }

    /// True when another input fits under the bound.
    #[must_use]
    pub fn can_submit(&self) -> bool {
        self.pending.len() < self.depth
    }

    /// True once two inputs shared the queue.
    #[must_use]
    pub fn overlap_observed(&self) -> bool {
        self.max_in_flight > 1
    }

    /// Input stamped `pts_us` queued after `submit_us`.
    ///
    /// # Arguments
    ///
    /// * `pts_us` - input stamp in microseconds.
    /// * `submit_us` - queue call duration in microseconds.
    pub fn note_submitted(&mut self, pts_us: u64, submit_us: u64) {
        self.pending.push_back(PendingSubmit {
            seq: self.next_seq,
            pts_us,
            submitted_at: Instant::now(),
            submit_us,
        });
        self.next_seq += 1;
        self.max_in_flight = self.max_in_flight.max(self.pending.len());
    }

    /// Oldest input completed, stamped `completed_pts_us`, after `copy_us`.
    ///
    /// Engines execute in order, so completions match submissions FIFO.
    ///
    /// # Arguments
    ///
    /// * `completed_pts_us` - output stamp in microseconds.
    /// * `copy_us` - output-to-surface conversion in microseconds.
    pub fn note_completed(&mut self, completed_pts_us: i64, copy_us: u64) {
        let Some(submit) = self.pending.pop_front() else {
            debug_assert!(false, "output without submission");
            return;
        };
        if self.timings.len() >= Self::MAX_TIMINGS {
            self.timings.pop_front();
        }
        self.timings.push_back(
            PhaseTiming::new(
                submit.seq,
                Duration::from_micros(submit.submit_us),
                submit.submitted_at.elapsed(),
                Duration::from_micros(copy_us),
                submit.submitted_at,
                Instant::now(),
            )
            .with_pts(submit.pts_us, completed_pts_us),
        );
    }

    /// Queued inputs dropped; codec flush voids them.
    pub fn discard_pending(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PipelineLedger;

    #[test]
    fn depth_clamps_to_two_through_four() {
        assert_eq!(PipelineLedger::new(0).depth(), 2);
        assert_eq!(PipelineLedger::new(1).depth(), 2);
        assert_eq!(PipelineLedger::new(3).depth(), 3);
        assert_eq!(PipelineLedger::new(4).depth(), 4);
        assert_eq!(PipelineLedger::new(99).depth(), 4);
        assert_eq!(PipelineLedger::DEFAULT_DEPTH, 4);
    }

    #[test]
    fn submit_complete_pairs_match_fifo() {
        let mut ledger = PipelineLedger::new(4);
        assert!(ledger.can_submit());
        for pts in [10_u64, 20, 30] {
            ledger.note_submitted(pts, 5);
        }
        assert_eq!(ledger.in_flight(), 3);
        assert_eq!(ledger.max_in_flight(), 3);
        assert!(ledger.overlap_observed());
        for (seq, pts) in [(0_u64, 10_i64), (1, 20), (2, 30)] {
            ledger.note_completed(pts, 7);
            let timing = ledger.timings()[usize::try_from(seq).expect("small seq")];
            assert_eq!(timing.seq(), seq);
            assert_eq!(
                timing.submitted_pts_us().expect("stamped"),
                u64::try_from(pts).expect("small pts")
            );
            assert_eq!(timing.completed_pts_us().expect("stamped"), pts);
            assert_eq!(timing.submit_us(), 5);
            assert_eq!(timing.copy_us(), 7);
            assert_eq!(
                timing.total_us(),
                12_u64.saturating_add(timing.engine_wait_us())
            );
        }
        assert_eq!(ledger.in_flight(), 0);
        assert!(ledger.can_submit());
    }

    #[test]
    fn single_frame_shows_no_overlap() {
        let mut ledger = PipelineLedger::new(4);
        for pts in [1_u64, 2] {
            ledger.note_submitted(pts, 1);
            ledger.note_completed(i64::try_from(pts).expect("small pts"), 1);
        }
        assert_eq!(ledger.max_in_flight(), 1);
        assert!(!ledger.overlap_observed());
        assert_eq!(ledger.timings().len(), 2);
    }

    #[test]
    fn engine_wait_spans_submit_to_completion() {
        let mut ledger = PipelineLedger::new(4);
        ledger.note_submitted(42, 1);
        std::thread::sleep(Duration::from_millis(5));
        ledger.note_completed(42, 1);
        assert!(ledger.timings()[0].engine_wait_us() >= 1_000);
    }

    #[test]
    fn window_evicts_oldest_timing() {
        let mut ledger = PipelineLedger::new(4);
        for pts in 0..1100_u64 {
            ledger.note_submitted(pts, 1);
            ledger.note_completed(i64::try_from(pts).expect("small pts"), 1);
        }
        assert_eq!(ledger.timings().len(), 1024);
        assert_eq!(ledger.timings()[0].seq(), 76);
        assert_eq!(ledger.timings()[1023].seq(), 1099);
    }

    #[test]
    fn discard_clears_without_timings() {
        let mut ledger = PipelineLedger::new(4);
        ledger.note_submitted(1, 1);
        ledger.note_submitted(2, 1);
        ledger.discard_pending();
        assert_eq!(ledger.in_flight(), 0);
        assert!(ledger.timings().is_empty());
        assert!(ledger.can_submit());
    }
}
