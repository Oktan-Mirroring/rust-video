//! Worker copy pump behind split-phase readback.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Queued copies before a launch waits.
const DEPTH: usize = 8;

/// Copies below this size run inline on the calling thread when idle.
const INLINE_COPY_BYTES: usize = 256 * 1024;

/// Inline-copy policy for one launch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InlineCopy {
    /// Copies below the threshold complete on the calling thread when idle.
    Allowed,
    /// Every copy queues behind the worker.
    Denied,
}

impl From<bool> for InlineCopy {
    /// `true` maps to [`Allowed`](Self::Allowed).
    fn from(allow: bool) -> Self {
        if allow {
            Self::Allowed
        } else {
            Self::Denied
        }
    }
}

/// Copy worker behind split-phase readback maps.
pub struct CopyPump<M, D> {
    /// Copy jobs, absent after shutdown or a spawn failure.
    tx: Option<SyncSender<Job<M>>>,
    /// Landed copies in send order.
    rx: Option<std::sync::mpsc::Receiver<D>>,
    /// Worker thread, absent after shutdown or a spawn failure.
    worker: Option<JoinHandle<()>>,
    /// Queued copies not yet received.
    inflight: AtomicUsize,
    /// Map-to-done conversion; runs on the worker and inline.
    copy: Arc<dyn Fn(M, Instant) -> D + Send + Sync>,
}

/// One queued copy.
struct Job<M> {
    /// Mapped source to copy.
    transfer: M,
}

impl<M: Send + 'static, D: Send + 'static> CopyPump<M, D> {
    /// Worker over nothing; maps arrive per copy.
    ///
    /// # Arguments
    ///
    /// * `name` - worker thread name.
    /// * `copy` - map-to-done conversion over a launch stamp; runs on the
    ///   worker and inline.
    #[must_use]
    pub fn spawn(name: &str, copy: impl Fn(M, Instant) -> D + Send + Sync + 'static) -> Self {
        let copy = Arc::new(copy);
        let (tx, rx) = sync_channel::<Job<M>>(DEPTH);
        let (done_tx, done_rx) = sync_channel::<D>(DEPTH);
        let worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn({
                let copy = Arc::clone(&copy);
                move || {
                    for job in rx {
                        let done = copy(job.transfer, Instant::now());
                        if done_tx.send(done).is_err() {
                            return;
                        }
                    }
                }
            })
            .ok();
        Self {
            tx: worker.is_some().then_some(tx),
            rx: worker.is_some().then_some(done_rx),
            worker,
            inflight: AtomicUsize::new(0),
            copy,
        }
    }

    /// Queue the copy of `mapped`, oldest-first behind earlier jobs.
    ///
    /// # Arguments
    ///
    /// * `mapped` - mapped source awaiting its copy.
    /// * `len` - copy length in bytes; below the threshold completes inline
    ///   when idle.
    /// * `inline` - inline-copy policy for this launch.
    ///
    /// # Returns
    ///
    /// The landed copy when completed inline, else `None` once queued.
    pub fn launch(&self, mapped: M, len: usize, inline: InlineCopy) -> Option<D> {
        let started = Instant::now();
        if inline == InlineCopy::Allowed && len < INLINE_COPY_BYTES && self.inflight() == 0 {
            return Some((self.copy)(mapped, started));
        }
        let Some(tx) = self.tx.as_ref() else {
            return Some((self.copy)(mapped, started));
        };
        match tx.send(Job { transfer: mapped }) {
            Ok(()) => {
                self.inflight.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(returned) => Some((self.copy)(returned.0.transfer, started)),
        }
    }

    /// Queued copies not yet received.
    fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Relaxed)
    }

    /// Oldest landed copy, if any arrived.
    pub fn harvest(&self) -> Option<D> {
        let rx = self.rx.as_ref()?;
        let done = rx.try_recv().ok()?;
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }

    /// Oldest landed copy, waiting for it.
    pub fn collect(&self) -> Option<D> {
        let rx = self.rx.as_ref()?;
        let done = rx.recv().ok()?;
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }

    /// Oldest landed copy, waiting up to `timeout` for it.
    ///
    /// # Arguments
    ///
    /// * `timeout` - wait budget when nothing landed.
    pub fn collect_timeout(&self, timeout: Duration) -> Option<D> {
        let rx = self.rx.as_ref()?;
        let done = rx.recv_timeout(timeout).ok()?;
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }

    /// Queued copies drain, the worker joins, orphaned maps release.
    ///
    /// # Arguments
    ///
    /// * `dispose` - orphan release; runs per uncollected copy.
    pub fn shutdown_with(&mut self, dispose: impl FnMut(D)) {
        let mut dispose = dispose;
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(rx) = self.rx.take() {
            while let Ok(done) = rx.try_recv() {
                dispose(done);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{CopyPump, InlineCopy, DEPTH, INLINE_COPY_BYTES};

    /// Pump copying its input plus one.
    fn pump() -> CopyPump<u64, u64> {
        CopyPump::spawn("core-test", |mapped, _started| mapped + 1)
    }

    #[test]
    fn small_copy_completes_inline_when_idle() {
        let pump = pump();
        assert_eq!(
            pump.launch(41, INLINE_COPY_BYTES - 1, InlineCopy::Allowed),
            Some(42)
        );
        assert!(pump.harvest().is_none());
    }

    #[test]
    fn threshold_copy_queues_behind_worker() {
        let pump = pump();
        assert_eq!(
            pump.launch(41, INLINE_COPY_BYTES, InlineCopy::Allowed),
            None
        );
        assert_eq!(pump.collect(), Some(42));
    }

    #[test]
    fn denied_inline_queues_small_copies() {
        let pump = pump();
        assert_eq!(pump.launch(7, 1, InlineCopy::Denied), None);
        assert_eq!(pump.collect(), Some(8));
    }

    #[test]
    fn bool_maps_to_policy() {
        assert_eq!(InlineCopy::from(true), InlineCopy::Allowed);
        assert_eq!(InlineCopy::from(false), InlineCopy::Denied);
    }

    #[test]
    fn queued_copies_land_in_order() {
        let pump = pump();
        let depth = u64::try_from(DEPTH).expect("small depth");
        for input in 0..depth {
            assert_eq!(
                pump.launch(input, INLINE_COPY_BYTES, InlineCopy::Allowed),
                None
            );
        }
        for input in 0..depth {
            assert_eq!(pump.collect(), Some(input + 1));
        }
    }

    #[test]
    fn harvest_takes_only_landed() {
        let pump = pump();
        assert_eq!(pump.launch(1, INLINE_COPY_BYTES, InlineCopy::Denied), None);
        assert_eq!(pump.collect(), Some(2));
        assert!(pump.harvest().is_none());
    }

    #[test]
    fn collect_timeout_gives_up_waiting() {
        let pump = pump();
        assert!(pump.collect_timeout(Duration::from_millis(1)).is_none());
        assert_eq!(pump.launch(3, INLINE_COPY_BYTES, InlineCopy::Denied), None);
        assert_eq!(pump.collect_timeout(Duration::from_secs(5)), Some(4));
    }

    #[test]
    fn launch_stamp_reaches_inline_and_worker() {
        let pump: CopyPump<u64, Instant> = CopyPump::spawn("core-test", |_mapped, started| started);
        let before = Instant::now();
        let inline = pump.launch(0, 1, InlineCopy::Allowed).expect("inline copy");
        assert!(inline >= before);
        assert_eq!(pump.launch(0, INLINE_COPY_BYTES, InlineCopy::Denied), None);
        let worker = pump.collect().expect("worker copy");
        assert!(worker >= before);
    }

    #[test]
    fn shutdown_disposes_uncollected() {
        let mut pump = pump();
        assert_eq!(pump.launch(1, INLINE_COPY_BYTES, InlineCopy::Denied), None);
        assert_eq!(pump.collect(), Some(2));
        assert_eq!(pump.launch(3, INLINE_COPY_BYTES, InlineCopy::Denied), None);
        let disposed = Arc::new(AtomicUsize::new(0));
        pump.shutdown_with(|done| {
            assert_eq!(done, 4);
            disposed.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        assert!(pump.harvest().is_none());
    }
}
