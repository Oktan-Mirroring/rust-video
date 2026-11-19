//! Reference picture tracking and decode instructions.
//!
//! Each context turns one access unit into parameter-set updates plus at
//! most one picture: reference lists resolved to stable ids, DPB marking
//! applied, unknown references answered by resync, never by guessing.

use crate::error::ParseError;

/// Stable identity of one decoded reference picture.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ReferenceId(pub usize);

impl ReferenceId {
    /// Current id, advanced to the next.
    pub fn next(&mut self) -> Self {
        let current = *self;
        self.0 += 1;
        current
    }
}

/// DPB slot assignments by reference id.
pub(crate) struct SlotBook {
    /// Reference id per slot, if assigned.
    slots: Vec<Option<ReferenceId>>,
}

impl SlotBook {
    /// Book with `count` free slots.
    #[must_use]
    pub fn with_slots(count: usize) -> Self {
        Self {
            slots: vec![None; count],
        }
    }

    /// Claim a free slot for `id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when every slot is taken.
    pub fn claim(&mut self, id: ReferenceId) -> Result<usize, ParseError> {
        if let Some(index) = self.slot_of(id) {
            return Ok(index);
        }
        let Some(index) = self.slots.iter().position(Option::is_none) else {
            return Err(ParseError::OutOfRange {
                syntax: "dpb slots",
            });
        };
        self.slots[index] = Some(id);
        Ok(index)
    }

    /// Slot assigned to `id`, if any.
    #[must_use]
    pub fn slot_of(&self, id: ReferenceId) -> Option<usize> {
        self.slots.iter().position(|slot| *slot == Some(id))
    }

    /// Identity behind `slot`, if the slot is claimed.
    #[must_use]
    pub fn identity_of(&self, slot: usize) -> Option<ReferenceId> {
        self.slots.get(slot).copied().flatten()
    }

    /// Free the slot `id` holds, if any.
    pub fn release(&mut self, id: ReferenceId) {
        if let Some(index) = self.slot_of(id) {
            self.slots[index] = None;
        }
    }

    /// Free every slot.
    pub fn release_all(&mut self) {
        self.slots.fill(None);
    }
}

/// Display-order buffer keyed by picture order count.
pub(crate) struct ReorderBuffer<T> {
    /// Reorder depth before the earliest picture releases.
    capacity: usize,
    /// Held pictures with their order counts.
    held: Vec<(i32, T)>,
}

impl<T> ReorderBuffer<T> {
    /// Buffer holding `capacity` pictures back.
    #[must_use]
    pub fn with_capacity(capacity: u32) -> Self {
        Self {
            capacity: capacity as usize,
            held: Vec::new(),
        }
    }

    /// Pictures held back before the earliest releases.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Push one picture; earliest pictures past the depth release.
    pub fn push(&mut self, poc: i32, item: T) -> Vec<T> {
        self.held.push((poc, item));
        let mut ready = Vec::new();
        while self.held.len() > self.capacity {
            let mut first = 0;
            for i in 1..self.held.len() {
                if self.held[i].0 < self.held[first].0 {
                    first = i;
                }
            }
            ready.push(self.held.remove(first).1);
        }
        ready
    }

    /// Push one picture, releasing everything held when `resets` starts a
    /// new sequence: order counts restart there, so held pictures predate it.
    pub fn push_flushing_on_reset(&mut self, resets: bool, poc: i32, item: T) -> Vec<T> {
        let mut ready = Vec::new();
        if resets {
            ready.extend(self.flush());
        }
        ready.extend(self.push(poc, item));
        ready
    }

    /// Every held picture in display order.
    pub fn flush(&mut self) -> Vec<T> {
        self.held.sort_by_key(|held| held.0);
        self.held.drain(..).map(|held| held.1).collect()
    }

    /// Drop every held picture without emitting.
    pub fn drop_held(&mut self) {
        self.held.clear();
    }

    /// Hold `capacity` pictures back, keeping held pictures.
    pub fn set_capacity(&mut self, capacity: u32) {
        self.capacity = capacity as usize;
    }
}

/// One reference picture resolved to its DPB identity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RefPicture {
    /// Stable identity, absent for a placeholder the DPB cannot supply.
    pub(crate) id: Option<ReferenceId>,
    /// Frame number, H.264 only.
    pub(crate) frame_num: u16,
    /// Picture order count.
    pub(crate) poc: i32,
    /// Long-term index, when long-term.
    pub(crate) long_term: Option<u32>,
}

impl RefPicture {
    /// Stable identity, absent for a placeholder the DPB cannot supply.
    #[must_use]
    pub fn id(&self) -> Option<ReferenceId> {
        self.id
    }

    /// Frame number, H.264 only.
    #[must_use]
    pub fn frame_num(&self) -> u16 {
        self.frame_num
    }

    /// Picture order count.
    #[must_use]
    pub fn poc(&self) -> i32 {
        self.poc
    }

    /// Long-term index, when long-term.
    #[must_use]
    pub fn long_term(&self) -> Option<u32> {
        self.long_term
    }
}

/// Coding flags of one decoded picture.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CodingFlags {
    /// Picture stays in the DPB as a reference.
    pub(crate) is_reference: bool,
    /// Picture is intra-coded.
    pub(crate) is_intra: bool,
    /// Picture is an IDR.
    pub(crate) is_idr: bool,
}

/// One decoded picture ready for the session.
#[derive(Clone, Debug)]
pub(crate) struct Picture {
    /// L0 references, absent on intra pictures.
    pub(crate) l0: Option<Vec<RefPicture>>,
    /// L1 references, absent on non-B pictures.
    pub(crate) l1: Option<Vec<RefPicture>>,
    /// Picture order count.
    pub(crate) poc: i32,
    /// Frame number, H.264 only.
    pub(crate) frame_num: u16,
    /// Sequence set id.
    pub(crate) sps_id: u8,
    /// Picture set id.
    pub(crate) pps_id: u8,
    /// Concatenated slice Annex-B bytes.
    pub(crate) bytes: Vec<u8>,
    /// Byte offset of each slice NAL.
    pub(crate) slice_offsets: Vec<u32>,
    /// Coding flags of the picture.
    pub(crate) coding: CodingFlags,
    /// IDR picture id, H.264 IDR only.
    pub(crate) idr_pic_id: u16,
    /// Stable identity when a reference.
    pub(crate) id: Option<ReferenceId>,
    /// Ids leaving the DPB with this picture.
    pub(crate) releases: Vec<ReferenceId>,
    /// Prior held pictures never display.
    pub(crate) drop_held: bool,
    /// Picture flushed the DPB and carries no references.
    pub(crate) resets_dpb: bool,
}

impl Picture {
    /// L0 references, absent on intra pictures.
    #[must_use]
    pub fn l0(&self) -> Option<&[RefPicture]> {
        self.l0.as_deref()
    }

    /// L1 references, absent on non-B pictures.
    #[must_use]
    pub fn l1(&self) -> Option<&[RefPicture]> {
        self.l1.as_deref()
    }

    /// Picture order count.
    #[must_use]
    pub fn poc(&self) -> i32 {
        self.poc
    }

    /// Frame number, H.264 only.
    #[must_use]
    pub fn frame_num(&self) -> u16 {
        self.frame_num
    }

    /// Sequence set id.
    #[must_use]
    pub fn sps_id(&self) -> u8 {
        self.sps_id
    }

    /// Picture set id.
    #[must_use]
    pub fn pps_id(&self) -> u8 {
        self.pps_id
    }

    /// Concatenated slice Annex-B bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Byte offset of each slice NAL.
    #[must_use]
    pub fn slice_offsets(&self) -> &[u32] {
        &self.slice_offsets
    }

    /// Picture stays in the DPB as a reference.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.coding.is_reference
    }

    /// Picture is intra-coded.
    #[must_use]
    pub fn is_intra(&self) -> bool {
        self.coding.is_intra
    }

    /// Picture is an IDR.
    #[must_use]
    pub fn is_idr(&self) -> bool {
        self.coding.is_idr
    }

    /// IDR picture id, H.264 IDR only.
    #[must_use]
    pub fn idr_pic_id(&self) -> u16 {
        self.idr_pic_id
    }

    /// Stable identity when a reference.
    #[must_use]
    pub fn id(&self) -> Option<ReferenceId> {
        self.id
    }

    /// Ids leaving the DPB with this picture.
    #[must_use]
    pub fn releases(&self) -> &[ReferenceId] {
        &self.releases
    }

    /// Prior held pictures never display.
    #[must_use]
    pub fn drop_held(&self) -> bool {
        self.drop_held
    }

    /// Picture flushed the DPB and carries no references.
    #[must_use]
    pub fn resets_dpb(&self) -> bool {
        self.resets_dpb
    }
}

/// Concatenated slice bytes with per-slice offsets behind `code`.
pub(crate) fn join_coded<S>(
    slices: &[S],
    code: &[u8],
    payload: impl Fn(&S) -> &[u8],
) -> (Vec<u8>, Vec<u32>) {
    let total: usize = slices
        .iter()
        .map(|slice| payload(slice).len() + code.len())
        .sum();
    let mut bytes = Vec::with_capacity(total);
    let mut offsets = Vec::with_capacity(slices.len());
    for slice in slices {
        offsets.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        bytes.extend_from_slice(code);
        bytes.extend_from_slice(payload(slice));
    }
    (bytes, offsets)
}

#[cfg(test)]
mod tests {
    use super::{ReorderBuffer, SlotBook};

    #[test]
    fn slots_claim_and_release() {
        let mut book = SlotBook::with_slots(2);
        let first = super::ReferenceId(0);
        let second = super::ReferenceId(1);
        assert_eq!(book.claim(first).expect("slot"), 0);
        assert_eq!(book.claim(first).expect("same slot"), 0);
        assert_eq!(book.claim(second).expect("slot"), 1);
        assert!(book.claim(super::ReferenceId(2)).is_err());
        assert_eq!(book.slot_of(second), Some(1));
        book.release(first);
        assert_eq!(book.slot_of(first), None);
        assert_eq!(book.claim(super::ReferenceId(2)).expect("slot"), 0);
        book.release_all();
        assert_eq!(book.slot_of(second), None);
    }

    #[test]
    fn reorder_holds_depth_then_releases_earliest() {
        let mut buffer = ReorderBuffer::with_capacity(2);
        assert_eq!(buffer.push(4, "a"), [] as [&str; 0]);
        assert_eq!(buffer.push(2, "b"), [] as [&str; 0]);
        assert_eq!(buffer.push(6, "c"), vec!["b"]);
        assert_eq!(buffer.push(0, "d"), vec!["d"]);
        assert_eq!(buffer.flush(), vec!["a", "c"]);
    }

    #[test]
    fn reorder_drop_held_discards() {
        let mut buffer = ReorderBuffer::with_capacity(4);
        buffer.push(2, "a");
        buffer.drop_held();
        assert_eq!(buffer.flush(), [] as [&str; 0]);
    }

    #[test]
    fn reorder_zero_capacity_releases_each_push() {
        let mut buffer = ReorderBuffer::with_capacity(0);
        assert_eq!(buffer.push(2, "a"), vec!["a"]);
        assert_eq!(buffer.push(1, "b"), vec!["b"]);
        assert_eq!(buffer.flush(), [] as [&str; 0]);
    }

    #[test]
    fn reorder_reset_releases_held_before_new_sequence() {
        let mut buffer = ReorderBuffer::with_capacity(2);
        assert_eq!(buffer.push(56, "a"), [] as [&str; 0]);
        assert_eq!(buffer.push(58, "b"), [] as [&str; 0]);
        assert_eq!(buffer.push_flushing_on_reset(true, 0, "c"), vec!["a", "b"]);
        assert_eq!(
            buffer.push_flushing_on_reset(false, 2, "d"),
            [] as [&str; 0]
        );
        assert_eq!(buffer.flush(), vec!["c", "d"]);
    }
}
