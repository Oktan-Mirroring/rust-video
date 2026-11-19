//! Pipelined readback: bounded holdback of decoded surfaces.
//!
//! Portable: holds pool slots plus geometry, no driver calls. Decoders
//! stage output slots here after submit and emit the oldest once the
//! queue exceeds the holdback, so the next submit lands before the
//! previous sync plus copy.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::h264_frontend::SurfaceSlot;

/// Bounded holdback queue over decoded surfaces.
///
/// Staged slots stay pinned by their leases, so a later submit never
/// reuses a slot awaiting its read. Depth bounds the pins: every pool
/// carries four slots past its reference capacity, and one slot stays
/// free for the current submit while depth caps at four.
pub(crate) struct PendingQueue {
    /// Staged slots, oldest first.
    queue: VecDeque<PendingSlot>,
    /// Submitted-ahead ceiling; the holdback keeps `depth - 1`.
    depth: usize,
}

impl PendingQueue {
    /// Synchronous floor: stage then emit, no submit ahead of a sync.
    pub(crate) const MIN_DEPTH: usize = 1;

    /// Pipeline ceiling: four in flight, one pool slot stays free.
    pub(crate) const MAX_DEPTH: usize = 4;

    /// Default submits ahead of a sync.
    pub(crate) const DEFAULT_DEPTH: usize = 4;

    /// Empty queue holding back `depth - 1` slots; depth clamps to 1..=4.
    pub(crate) fn new(depth: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            depth: depth.clamp(Self::MIN_DEPTH, Self::MAX_DEPTH),
        }
    }

    /// Submitted-ahead ceiling.
    pub(crate) fn depth(&self) -> usize {
        self.depth
    }

    /// Submitted-ahead ceiling, clamped to 1..=4.
    pub(crate) fn set_depth(&mut self, depth: usize) {
        self.depth = depth.clamp(Self::MIN_DEPTH, Self::MAX_DEPTH);
    }

    /// Staged slot count.
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    /// True when no slot is staged.
    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Stage `slot` behind the queue.
    pub(crate) fn push(&mut self, slot: PendingSlot) {
        self.queue.push_back(slot);
    }

    /// Oldest staged slot once the queue exceeds the holdback.
    pub(crate) fn pop_ready(&mut self) -> Option<PendingSlot> {
        if self.queue.len() > self.depth - 1 {
            self.queue.pop_front()
        } else {
            None
        }
    }

    /// Oldest staged slot, if any.
    pub(crate) fn pop_oldest(&mut self) -> Option<PendingSlot> {
        self.queue.pop_front()
    }
}

/// One decoded surface awaiting sync plus copy.
pub(crate) struct PendingSlot {
    /// Pool slot holding the picture.
    slot: SurfaceSlot,
    /// Frame size in luma samples.
    size: (u32, u32),
    /// Crop rectangle as `(x, y, width, height)`.
    crop: (u32, u32, u32, u32),
    /// Frame generation reserved at stage time.
    generation: u64,
    /// Pool-slot lease; the slot stays out of the free set while held.
    _guard: Arc<()>,
}

impl PendingSlot {
    /// Slot `slot` at `size` cropped to `crop`, emitted as `generation`.
    ///
    /// # Arguments
    ///
    /// * `guard` - pool lease pinning `slot` until the read lands.
    pub(crate) fn new(
        slot: SurfaceSlot,
        size: (u32, u32),
        crop: (u32, u32, u32, u32),
        generation: u64,
        guard: Arc<()>,
    ) -> Self {
        Self {
            slot,
            size,
            crop,
            generation,
            _guard: guard,
        }
    }

    /// Pool slot holding the picture.
    pub(crate) fn slot(&self) -> SurfaceSlot {
        self.slot
    }

    /// Frame size in luma samples.
    pub(crate) fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Crop rectangle as `(x, y, width, height)`.
    pub(crate) fn crop(&self) -> (u32, u32, u32, u32) {
        self.crop
    }

    /// Frame generation reserved at stage time.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{PendingQueue, PendingSlot};
    use crate::h264_frontend::SurfaceSlot;

    /// Staged slot over pool index `index`.
    fn slot(index: u32) -> PendingSlot {
        PendingSlot::new(
            SurfaceSlot::new(index),
            (64, 64),
            (0, 0, 64, 64),
            u64::from(index),
            Arc::new(()),
        )
    }

    #[test]
    fn depth_clamps_to_supported_range() {
        assert_eq!(PendingQueue::new(0).depth(), PendingQueue::MIN_DEPTH);
        assert_eq!(PendingQueue::new(99).depth(), PendingQueue::MAX_DEPTH);
        let mut queue = PendingQueue::new(PendingQueue::DEFAULT_DEPTH);
        assert_eq!(queue.depth(), 4);
        queue.set_depth(0);
        assert_eq!(queue.depth(), 1);
        queue.set_depth(99);
        assert_eq!(queue.depth(), 4);
    }

    #[test]
    fn depth_one_releases_every_stage() {
        let mut queue = PendingQueue::new(1);
        queue.push(slot(0));
        assert_eq!(queue.pop_ready().map(|slot| slot.generation()), Some(0));
        assert!(queue.pop_ready().is_none());
        assert!(queue.is_empty());
    }

    #[test]
    fn holdback_keeps_newest_behind_bound() {
        let mut queue = PendingQueue::new(3);
        for index in 0..5 {
            queue.push(slot(index));
        }
        assert_eq!(queue.len(), 5);
        let mut released = Vec::new();
        while let Some(staged) = queue.pop_ready() {
            released.push(staged.slot().index());
        }
        assert_eq!(released, [0, 1, 2]);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn drain_returns_oldest_first() {
        let mut queue = PendingQueue::new(4);
        for index in 0..3 {
            queue.push(slot(index));
        }
        assert!(queue.pop_ready().is_none());
        let mut drained = Vec::new();
        while let Some(staged) = queue.pop_oldest() {
            drained.push((staged.slot().index(), staged.generation()));
        }
        assert_eq!(drained, [(0, 0), (1, 1), (2, 2)]);
        assert!(queue.is_empty());
    }

    #[test]
    fn staged_slot_carries_geometry() {
        let staged = slot(7);
        assert_eq!(staged.slot().index(), 7);
        assert_eq!(staged.size(), (64, 64));
        assert_eq!(staged.crop(), (0, 0, 64, 64));
        assert_eq!(staged.generation(), 7);
    }
}
