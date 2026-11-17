//! Decoder slot pins held by live surfaces.

use std::sync::atomic::{AtomicU32, Ordering};

/// Decoder slot pins held by live surfaces.
#[derive(Debug, Default)]
pub(crate) struct SlotPins {
    /// Bit per slot; set while a live surface names the slot.
    pinned: AtomicU32,
}

impl SlotPins {
    /// Unpinned slots.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            pinned: AtomicU32::new(0),
        }
    }

    /// Pin `slot` for a live surface.
    pub(crate) fn pin(&self, slot: u8) {
        if let Some(bit) = 1u32.checked_shl(u32::from(slot)) {
            self.pinned.fetch_or(bit, Ordering::AcqRel);
        }
    }

    /// Release `slot` behind a dropped surface.
    pub(crate) fn unpin(&self, slot: u8) {
        if let Some(bit) = 1u32.checked_shl(u32::from(slot)) {
            self.pinned.fetch_and(!bit, Ordering::AcqRel);
        }
    }

    /// True while a live surface names `slot`.
    #[must_use]
    pub(crate) fn is_pinned(&self, slot: usize) -> bool {
        let Ok(slot) = u32::try_from(slot) else {
            return false;
        };
        let Some(bit) = 1u32.checked_shl(slot) else {
            return false;
        };
        self.pinned.load(Ordering::Acquire) & bit != 0
    }
}

#[cfg(test)]
mod tests {
    use super::SlotPins;

    #[test]
    fn pin_holds_until_unpin() {
        let pins = SlotPins::new();
        assert!(!pins.is_pinned(3));
        pins.pin(3);
        assert!(pins.is_pinned(3));
        assert!(!pins.is_pinned(4));
        pins.unpin(3);
        assert!(!pins.is_pinned(3));
    }

    #[test]
    fn out_of_range_slots_stay_clear() {
        let pins = SlotPins::new();
        pins.pin(31);
        assert!(pins.is_pinned(31));
        pins.pin(u8::MAX);
        assert!(!pins.is_pinned(usize::MAX));
        pins.unpin(31);
        assert!(!pins.is_pinned(31));
    }
}
