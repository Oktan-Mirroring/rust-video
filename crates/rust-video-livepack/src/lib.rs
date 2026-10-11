//! Livepack frame packing: slots and bounded queues.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

/// Full-queue behavior.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Policy {
    /// Evict the oldest entry and report it.
    DropOldest,
    /// Refuse the push; the caller retries.
    Block,
}

/// Push refusal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PushError {
    /// Queue full under blocking policy.
    WouldBlock,
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WouldBlock => write!(f, "queue full"),
        }
    }
}

impl std::error::Error for PushError {}

/// Fixed-capacity FIFO with explicit overflow behavior.
#[derive(Debug)]
pub struct BoundedQueue<T> {
    queue: Mutex<VecDeque<T>>,
    cap: usize,
    policy: Policy,
}

impl<T> BoundedQueue<T> {
    /// Queue holding at most `cap` entries; zero capacity is rejected.
    #[must_use]
    pub fn new(cap: usize, policy: Policy) -> Option<Self> {
        (cap > 0).then(|| Self {
            queue: Mutex::new(VecDeque::with_capacity(cap)),
            cap,
            policy,
        })
    }

    /// Maximum entry count.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Full-queue behavior.
    #[must_use]
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Entry count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True when no entry is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Push `value`; the evicted entry under drop-oldest, if any.
    ///
    /// # Arguments
    ///
    /// * `value` - entry to enqueue.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the queue is full under blocking policy.
    pub fn push(&self, value: T) -> Result<Option<T>, PushError> {
        let mut queue = self.lock();
        if queue.len() < self.cap {
            queue.push_back(value);
            return Ok(None);
        }
        match self.policy {
            Policy::DropOldest => {
                let evicted = queue.pop_front();
                queue.push_back(value);
                Ok(evicted)
            }
            Policy::Block => Err(PushError::WouldBlock),
        }
    }

    /// Oldest entry, if any.
    #[must_use]
    pub fn pop(&self) -> Option<T> {
        self.lock().pop_front()
    }

    /// Queue guard, poison-tolerant.
    fn lock(&self) -> MutexGuard<'_, VecDeque<T>> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Single latest-value cell.
#[derive(Debug)]
pub struct FrameSlot<T> {
    cell: Mutex<Option<T>>,
}

impl<T> FrameSlot<T> {
    /// Empty cell.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cell: Mutex::new(None),
        }
    }

    /// Store `value`, discarding the previous one.
    pub fn store(&self, value: T) {
        *self.lock() = Some(value);
    }

    /// Store `value`, returning the previous one.
    #[must_use]
    pub fn swap(&self, value: T) -> Option<T> {
        self.lock().replace(value)
    }

    /// Remove and return the stored value, if any.
    #[must_use]
    pub fn take(&self) -> Option<T> {
        self.lock().take()
    }

    /// Cell guard, poison-tolerant.
    fn lock(&self) -> MutexGuard<'_, Option<T>> {
        self.cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<T> Default for FrameSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedQueue, FrameSlot, Policy, PushError};

    #[test]
    fn queue_rejects_zero_capacity() {
        assert!(BoundedQueue::<u8>::new(0, Policy::Block).is_none());
    }

    #[test]
    fn queue_pops_fifo() {
        let queue = BoundedQueue::new(3, Policy::Block).expect("valid cap");
        assert!(queue.is_empty());
        assert_eq!(queue.push(1).expect("room"), None);
        assert_eq!(queue.push(2).expect("room"), None);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn drop_oldest_evicts_and_reports() {
        let queue = BoundedQueue::new(2, Policy::DropOldest).expect("valid cap");
        assert_eq!(queue.push(1).expect("room"), None);
        assert_eq!(queue.push(2).expect("room"), None);
        assert_eq!(queue.push(3).expect("eviction"), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn blocking_push_fails_when_full() {
        let queue = BoundedQueue::new(1, Policy::Block).expect("valid cap");
        assert_eq!(queue.push(1).expect("room"), None);
        assert_eq!(queue.push(2).unwrap_err(), PushError::WouldBlock);
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.push(2).expect("room"), None);
    }

    #[test]
    fn push_error_display_names_cause() {
        assert_eq!(PushError::WouldBlock.to_string(), "queue full");
    }

    #[test]
    fn slot_stores_swaps_takes() {
        let slot = FrameSlot::new();
        assert_eq!(slot.take(), None);
        slot.store(1);
        assert_eq!(slot.swap(2), Some(1));
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.take(), None);
    }

    #[test]
    fn slot_default_is_empty() {
        let slot = FrameSlot::<u8>::default();
        assert_eq!(slot.take(), None);
    }
}
