//! Fence-per-inflight-frame collection with a named-fence waiter.

use crate::error::VulkanError;

/// One completion fence behind submitted work.
pub trait Fence {
    /// True once the work completed.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the query fails.
    fn signaled(&self) -> Result<bool, VulkanError>;

    /// Block until signaled or `timeout_ns` elapses.
    ///
    /// Returns `false` on timeout, never blocks past it.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the wait fails.
    fn wait(&self, timeout_ns: u64) -> Result<bool, VulkanError>;
}

/// Completion of synchronous work: already signaled.
pub struct Signaled;

impl Fence for Signaled {
    fn signaled(&self) -> Result<bool, VulkanError> {
        Ok(true)
    }

    fn wait(&self, _timeout_ns: u64) -> Result<bool, VulkanError> {
        Ok(true)
    }
}

/// Pending fences, one per inflight frame.
pub struct Inflight<F> {
    /// Fences not yet reaped.
    pending: Vec<F>,
}

impl<F: Fence> Inflight<F> {
    /// Empty collection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Pending fence count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// True when nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Track `fence` for one submitted frame.
    pub fn submit(&mut self, fence: F) {
        self.pending.push(fence);
    }

    /// Drop signaled fences without blocking.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a fence query fails.
    pub fn reap(&mut self) -> Result<usize, VulkanError> {
        let mut kept = Vec::with_capacity(self.pending.len());
        let mut reaped = 0;
        for fence in self.pending.drain(..) {
            if fence.signaled()? {
                reaped += 1;
            } else {
                kept.push(fence);
            }
        }
        self.pending = kept;
        Ok(reaped)
    }

    /// Wait each pending fence out, then drop all.
    ///
    /// Each wait names its own fence and timeout; no idle wait exists.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a fence wait fails.
    pub fn drain(&mut self, timeout_each_ns: u64) -> Result<(), VulkanError> {
        for fence in self.pending.drain(..) {
            fence.wait(timeout_each_ns)?;
        }
        Ok(())
    }
}

impl<F: Fence> Default for Inflight<F> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{Fence, Inflight, Signaled};
    use crate::error::VulkanError;

    /// Scripted fence: signaled once `signal` runs.
    struct Stub {
        /// Signaled state.
        done: Cell<bool>,
        /// Waits observed.
        waits: Cell<u32>,
    }

    impl Stub {
        /// Unsignaled stub.
        fn pending() -> Self {
            Self {
                done: Cell::new(false),
                waits: Cell::new(0),
            }
        }

        /// Latched stub.
        fn signal(&self) {
            self.done.set(true);
        }
    }

    impl Fence for Stub {
        fn signaled(&self) -> Result<bool, VulkanError> {
            Ok(self.done.get())
        }

        fn wait(&self, _timeout_ns: u64) -> Result<bool, VulkanError> {
            self.waits.set(self.waits.get() + 1);
            self.signal();
            Ok(true)
        }
    }

    #[test]
    fn signaled_completes_without_wait() {
        let fence = Signaled;
        assert!(fence.signaled().expect("query"));
        assert!(fence.wait(0).expect("wait"));
    }

    #[test]
    fn reap_drops_only_signaled() {
        let mut inflight = Inflight::new();
        assert!(inflight.is_empty());
        inflight.submit(Stub::pending());
        inflight.submit(Stub::pending());
        assert_eq!(inflight.len(), 2);
        assert_eq!(inflight.reap().expect("reap"), 0);
        assert_eq!(inflight.len(), 2);
    }

    #[test]
    fn reap_keeps_pending_fences() {
        let mut inflight = Inflight::default();
        let quick = Stub::pending();
        quick.signal();
        inflight.submit(quick);
        inflight.submit(Stub::pending());
        assert_eq!(inflight.reap().expect("reap"), 1);
        assert_eq!(inflight.len(), 1);
    }

    #[test]
    fn drain_waits_each_fence() {
        let mut inflight = Inflight::new();
        inflight.submit(Stub::pending());
        inflight.submit(Stub::pending());
        inflight.drain(1_000).expect("drain");
        assert!(inflight.is_empty());
    }

    #[test]
    fn drain_of_empty_is_noop() {
        let mut inflight: Inflight<Stub> = Inflight::new();
        inflight.drain(1_000).expect("drain");
    }
}
