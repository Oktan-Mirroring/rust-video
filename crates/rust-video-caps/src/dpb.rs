//! Slot bookkeeping shared by every decode tier.
//!
//! Allocation, eviction, and display-order output over tier payloads.

/// Shared slot-array bookkeeping over any decoded-picture state.
pub trait DpbBookkeeping<O: SlotState> {
    /// First free slot outside `pinned`.
    ///
    /// # Errors
    ///
    /// Returns `PinnedBlocked` when free slots exist but live surfaces
    /// pin them, `Full` when every slot holds a picture.
    fn alloc_where(&self, pinned: impl Fn(usize) -> bool) -> Result<usize, SlotAllocFailure>;

    /// Slot contents out of the reference set, emitted when undisplayed.
    ///
    /// # Panics
    ///
    /// Panics when `index` names no slot.
    fn evict(&mut self, index: usize, emits: &mut Vec<(i32, usize)>);

    /// One slot emitted, when held and undisplayed.
    ///
    /// # Panics
    ///
    /// Panics when `index` names no slot.
    fn emit_slot(&mut self, index: usize, emits: &mut Vec<(i32, usize)>);

    /// Every undisplayed picture emitted in slot order.
    fn emit_undisplayed(&mut self, emits: &mut Vec<(i32, usize)>);

    /// Lowest undisplayed picture below `below` emitted.
    #[must_use]
    fn pop_earliest(&mut self, emits: &mut Vec<(i32, usize)>, below: Option<i32>) -> bool;

    /// Displayed pictures held without reference freed.
    fn free_displayed_non_refs(&mut self);

    /// Every picture out of the reference set.
    fn release_all(&mut self);

    /// Every slot dropped without output.
    fn clear_slots(&mut self);

    /// Stored picture count.
    #[must_use]
    fn stored(&self) -> usize;

    /// Undisplayed picture count.
    #[must_use]
    fn buffered(&self) -> usize;

    /// Reference picture count.
    #[must_use]
    fn references(&self) -> usize;
}

impl<O: SlotState> DpbBookkeeping<O> for [Option<O>] {
    fn alloc_where(&self, pinned: impl Fn(usize) -> bool) -> Result<usize, SlotAllocFailure> {
        let mut blocked = false;
        for (index, slot) in self.iter().enumerate() {
            if slot.is_none() {
                if pinned(index) {
                    blocked = true;
                } else {
                    return Ok(index);
                }
            }
        }
        Err(if blocked {
            SlotAllocFailure::PinnedBlocked
        } else {
            SlotAllocFailure::Full
        })
    }

    fn evict(&mut self, index: usize, emits: &mut Vec<(i32, usize)>) {
        let Some(occupant) = self[index].as_mut() else {
            return;
        };
        occupant.release();
        if occupant.is_displayed() {
            self[index] = None;
        } else {
            occupant.mark_displayed();
            emits.push((occupant.poc(), index));
        }
    }

    fn emit_slot(&mut self, index: usize, emits: &mut Vec<(i32, usize)>) {
        if let Some(occupant) = self[index].as_mut() {
            if !occupant.is_displayed() {
                occupant.mark_displayed();
                emits.push((occupant.poc(), index));
            }
        }
    }

    fn emit_undisplayed(&mut self, emits: &mut Vec<(i32, usize)>) {
        for (index, slot) in self.iter_mut().enumerate() {
            if let Some(occupant) = slot {
                if !occupant.is_displayed() {
                    occupant.mark_displayed();
                    emits.push((occupant.poc(), index));
                }
            }
        }
    }

    fn pop_earliest(&mut self, emits: &mut Vec<(i32, usize)>, below: Option<i32>) -> bool {
        let mut next: Option<(i32, usize)> = None;
        for (index, slot) in self.iter().enumerate() {
            if let Some(occupant) = slot {
                if !occupant.is_displayed() && below.is_none_or(|max| occupant.poc() < max) {
                    let candidate = (occupant.poc(), index);
                    if next.is_none_or(|best| candidate < best) {
                        next = Some(candidate);
                    }
                }
            }
        }
        let Some((poc, index)) = next else {
            return false;
        };
        emits.push((poc, index));
        if let Some(occupant) = self.get_mut(index).and_then(Option::as_mut) {
            occupant.mark_displayed();
        }
        true
    }

    fn free_displayed_non_refs(&mut self) {
        for slot in self.iter_mut() {
            if slot
                .as_ref()
                .is_some_and(|occupant| occupant.is_displayed() && !occupant.is_reference())
            {
                *slot = None;
            }
        }
    }

    fn release_all(&mut self) {
        for slot in self.iter_mut().flatten() {
            slot.release();
        }
    }

    fn clear_slots(&mut self) {
        for slot in self.iter_mut() {
            *slot = None;
        }
    }

    fn stored(&self) -> usize {
        self.iter().flatten().count()
    }

    fn buffered(&self) -> usize {
        self.iter()
            .flatten()
            .filter(|slot| !slot.is_displayed())
            .count()
    }

    fn references(&self) -> usize {
        self.iter()
            .flatten()
            .filter(|slot| slot.is_reference())
            .count()
    }
}

/// Decoded-picture state behind shared slot bookkeeping.
pub trait SlotState {
    /// Picture order count.
    fn poc(&self) -> i32;

    /// Used for reference.
    fn is_reference(&self) -> bool;

    /// Already output.
    fn is_displayed(&self) -> bool;

    /// Mark output.
    fn mark_displayed(&mut self);

    /// Out of the reference set, with codec long-term state cleared.
    fn release(&mut self);
}

/// Pin-aware view allocation over reference slots.
pub trait ViewAlloc<S> {
    /// First view outside `pinned` and the reference map.
    #[must_use]
    fn free_view(
        &self,
        views: u8,
        view_of: impl Fn(&S) -> u8,
        pinned: impl Fn(usize) -> bool,
    ) -> Option<u8>;
}

impl<S> ViewAlloc<S> for [Option<S>] {
    fn free_view(
        &self,
        views: u8,
        view_of: impl Fn(&S) -> u8,
        pinned: impl Fn(usize) -> bool,
    ) -> Option<u8> {
        (0..views).find(|view| {
            !pinned(usize::from(*view)) && !self.iter().flatten().any(|slot| view_of(slot) == *view)
        })
    }
}

/// Exhausted slot array.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SlotAllocFailure {
    /// Free slots exist but live surfaces pin them.
    PinnedBlocked,
    /// Every slot holds a picture.
    Full,
}

/// Previous reference POC behind MSB derivation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PocTracker {
    /// Previous POC most significant bits.
    msb: i64,
    /// Previous POC least significant bits.
    lsb: u32,
}

impl PocTracker {
    /// Zeroed tracker.
    #[must_use]
    pub fn new() -> Self {
        Self { msb: 0, lsb: 0 }
    }

    /// Most significant bits for `lsb` against the previous reference.
    #[must_use]
    pub fn msb_for(&self, lsb: i64, max_lsb: i64) -> i64 {
        let half = max_lsb / 2;
        let previous = i64::from(self.lsb);
        if lsb < previous && previous - lsb >= half {
            self.msb + max_lsb
        } else if lsb > previous && lsb - previous > half {
            self.msb - max_lsb
        } else {
            self.msb
        }
    }

    /// Observe the current reference POC decomposition.
    pub fn observe(&mut self, msb: i64, lsb: u32) {
        self.msb = msb;
        self.lsb = lsb;
    }

    /// Zeroed pair, as before the first reference.
    pub fn reset(&mut self) {
        self.msb = 0;
        self.lsb = 0;
    }
}

impl Default for PocTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DpbBookkeeping, PocTracker, SlotAllocFailure, SlotState, ViewAlloc};

    /// Minimal picture behind bookkeeping tests.
    struct Entry {
        /// Picture order count.
        poc: i32,
        /// Used for reference.
        reference: bool,
        /// Already output.
        displayed: bool,
    }

    impl SlotState for Entry {
        fn poc(&self) -> i32 {
            self.poc
        }

        fn is_reference(&self) -> bool {
            self.reference
        }

        fn is_displayed(&self) -> bool {
            self.displayed
        }

        fn mark_displayed(&mut self) {
            self.displayed = true;
        }

        fn release(&mut self) {
            self.reference = false;
        }
    }

    /// Stored entry.
    fn stored(poc: i32, reference: bool, displayed: bool) -> Entry {
        Entry {
            poc,
            reference,
            displayed,
        }
    }

    #[test]
    fn alloc_skips_pinned() {
        let slots: [Option<Entry>; 3] = [None, None, None];
        assert_eq!(slots.as_slice().alloc_where(|index| index == 0), Ok(1));
        assert_eq!(
            slots.as_slice().alloc_where(|_| true),
            Err(SlotAllocFailure::PinnedBlocked)
        );
        let full: [Option<Entry>; 2] = [Some(stored(0, true, false)), Some(stored(2, true, false))];
        assert_eq!(
            full.as_slice().alloc_where(|_| false),
            Err(SlotAllocFailure::Full)
        );
    }

    #[test]
    fn evict_emits_undisplayed_and_drops_displayed() {
        let mut slots: [Option<Entry>; 2] =
            [Some(stored(4, true, false)), Some(stored(6, true, true))];
        let mut emits = Vec::new();
        slots.as_mut_slice().evict(0, &mut emits);
        assert_eq!(emits, vec![(4, 0)]);
        assert!(slots[0].as_ref().is_some_and(|entry| entry.displayed));
        slots.as_mut_slice().evict(1, &mut emits);
        assert!(slots[1].is_none());
    }

    #[test]
    fn pop_earliest_orders_by_poc_then_slot() {
        let mut slots: [Option<Entry>; 3] = [
            Some(stored(8, false, false)),
            Some(stored(2, false, false)),
            Some(stored(2, false, false)),
        ];
        let mut emits = Vec::new();
        assert!(slots.as_mut_slice().pop_earliest(&mut emits, None));
        assert!(slots.as_mut_slice().pop_earliest(&mut emits, None));
        assert_eq!(emits, vec![(2, 1), (2, 2)]);
        assert!(!slots.as_mut_slice().pop_earliest(&mut emits, Some(8)));
        assert!(slots.as_mut_slice().pop_earliest(&mut emits, Some(9)));
        assert_eq!(emits, vec![(2, 1), (2, 2), (8, 0)]);
    }

    #[test]
    fn free_view_skips_pinned_and_mapped() {
        let slots: [Option<u8>; 2] = [Some(1), None];
        assert_eq!(
            slots
                .as_slice()
                .free_view(4, |view| *view, |index| index == 0),
            Some(2)
        );
        assert_eq!(slots.as_slice().free_view(2, |view| *view, |_| true), None);
    }

    #[test]
    fn msb_wraps_both_directions() {
        let mut tracker = PocTracker::new();
        tracker.observe(64, 48);
        assert_eq!(tracker.msb_for(4, 64), 128);
        assert_eq!(tracker.msb_for(50, 64), 64);
        tracker.observe(64, 4);
        assert_eq!(tracker.msb_for(60, 64), 0);
        tracker.reset();
        assert_eq!(tracker.msb_for(4, 64), 0);
    }
}
