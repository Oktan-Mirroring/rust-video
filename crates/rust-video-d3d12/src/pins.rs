//! Decoder pool slot pins behind live surfaces.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Decoder pool slot count behind the pin table.
pub(crate) const PIN_SLOTS: usize = 16;

/// Live-surface pin counts over one decoder pool.
pub(crate) struct SlotPins {
    /// Held-surface count per slot.
    held: [AtomicUsize; PIN_SLOTS],
}

impl SlotPins {
    /// Pin table with every slot free.
    pub(crate) fn new() -> Self {
        Self {
            held: std::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    /// True while a live surface pins `slot`.
    pub(crate) fn pinned(&self, slot: u8) -> bool {
        self.held
            .get(usize::from(slot))
            .is_some_and(|held| held.load(Ordering::Acquire) > 0)
    }

    /// Pin over `slot` on `pins`, held until dropped.
    pub(crate) fn pin(self: &Arc<Self>, slot: u8) -> SlotPin {
        if let Some(held) = self.held.get(usize::from(slot)) {
            held.fetch_add(1, Ordering::AcqRel);
        }
        SlotPin {
            pins: Arc::clone(self),
            slot,
        }
    }

    /// Release one pin over `slot`.
    fn release(&self, slot: u8) {
        if let Some(held) = self.held.get(usize::from(slot)) {
            held.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One live-surface pin over a decoder pool slot.
pub(crate) struct SlotPin {
    /// Pin table behind the pool.
    pins: Arc<SlotPins>,
    /// Pinned pool slot.
    slot: u8,
}

impl SlotPin {
    /// Pinned pool slot.
    pub(crate) fn slot(&self) -> u8 {
        self.slot
    }
}

impl Drop for SlotPin {
    fn drop(&mut self) {
        self.pins.release(self.slot);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::SlotPins;

    #[test]
    fn pin_holds_until_dropped() {
        let pins = Arc::new(SlotPins::new());
        assert!(!pins.pinned(3));
        let held = pins.pin(3);
        assert_eq!(held.slot(), 3);
        assert!(pins.pinned(3));
        assert!(!pins.pinned(4));
        drop(held);
        assert!(!pins.pinned(3));
    }

    #[test]
    fn pins_share_one_table() {
        let pins = Arc::new(SlotPins::new());
        let first = pins.pin(0);
        let second = pins.pin(0);
        drop(first);
        assert!(pins.pinned(0));
        drop(second);
        assert!(!pins.pinned(0));
    }
}
