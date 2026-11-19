//! Reference picture tracking and decode instructions.
//!
//! Each context turns one access unit into parameter-set updates plus at
//! most one picture: reference lists resolved to stable ids, DPB marking
//! applied, unknown references answered by resync, never by guessing.

use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::au::AccessUnit;
use crate::parse::av1;
use crate::parse::h264;
use crate::parse::h265;

use rust_video_caps::dpb::PocTracker;

/// Optional reference lists of one picture, L0 then L1.
type RefLists = (Option<Vec<RefPicture>>, Option<Vec<RefPicture>>);

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
    id: Option<ReferenceId>,
    /// Frame number, H.264 only.
    frame_num: u16,
    /// Picture order count.
    poc: i32,
    /// Long-term index, when long-term.
    long_term: Option<u32>,
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
    l0: Option<Vec<RefPicture>>,
    /// L1 references, absent on non-B pictures.
    l1: Option<Vec<RefPicture>>,
    /// Picture order count.
    poc: i32,
    /// Frame number, H.264 only.
    frame_num: u16,
    /// Sequence set id.
    sps_id: u8,
    /// Picture set id.
    pps_id: u8,
    /// Concatenated slice Annex-B bytes.
    bytes: Vec<u8>,
    /// Byte offset of each slice NAL.
    slice_offsets: Vec<u32>,
    /// Coding flags of the picture.
    coding: CodingFlags,
    /// IDR picture id, H.264 IDR only.
    idr_pic_id: u16,
    /// Stable identity when a reference.
    id: Option<ReferenceId>,
    /// Ids leaving the DPB with this picture.
    releases: Vec<ReferenceId>,
    /// Prior held pictures never display.
    drop_held: bool,
    /// Picture flushed the DPB and carries no references.
    resets_dpb: bool,
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

/// H.264 decoder instruction.
pub(crate) enum H264Instruction {
    /// Sequence set for the session parameters.
    Sps(Rc<h264::Sps>),
    /// Picture set for the session parameters.
    Pps(Rc<h264::Pps>),
    /// Picture for the session.
    Picture(Picture),
}

/// One H.264 DPB entry.
struct H264Entry {
    /// Stable identity.
    id: ReferenceId,
    /// Frame number.
    frame_num: u32,
    /// Picture order count.
    poc: i32,
    /// Long-term index, when long-term.
    long_term: Option<u32>,
}

/// H.264 reference context: POC, marking, reference lists.
pub(crate) struct H264Context {
    /// Decoded reference pictures.
    dpb: Vec<H264Entry>,
    /// Next reference identity.
    next_id: ReferenceId,
    /// Previous frame number.
    prev_frame_num: u32,
    /// Previous frame-number offset.
    prev_offset: i64,
    /// Previous reference picture ran MMCO 5.
    prev_mmco5: bool,
    /// Previous reference POC behind MSB derivation.
    poc: PocTracker,
    /// Pictures drop until the next IDR.
    waiting_idr: bool,
    /// First picture since reset.
    first: bool,
}

impl H264Context {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dpb: Vec::new(),
            next_id: ReferenceId::default(),
            prev_frame_num: 0,
            prev_offset: 0,
            prev_mmco5: false,
            poc: PocTracker::new(),
            waiting_idr: false,
            first: true,
        }
    }

    /// Instructions for one access unit.
    pub fn compile(&mut self, au: AccessUnit<h264::ParsedNalu>) -> Vec<H264Instruction> {
        let mut instructions = Vec::new();
        let mut slices = Vec::new();
        for nalu in au.into_nalus() {
            match nalu {
                h264::ParsedNalu::Sps(sps) => {
                    instructions.push(H264Instruction::Sps(sps));
                }
                h264::ParsedNalu::Pps(pps) => {
                    instructions.push(H264Instruction::Pps(pps));
                }
                h264::ParsedNalu::Slice(slice) => slices.push(*slice),
                h264::ParsedNalu::Other => {}
            }
        }
        if slices.is_empty() {
            return instructions;
        }
        if let Some(picture) = self.put_picture(&slices) {
            instructions.push(H264Instruction::Picture(picture));
        }
        instructions
    }
}

impl Default for H264Context {
    fn default() -> Self {
        Self::new()
    }
}

impl H264Context {
    /// One picture from its slices, or `None` when the picture drops.
    fn put_picture(&mut self, slices: &[h264::Slice]) -> Option<Picture> {
        let first = &slices[0];
        let sps = Rc::clone(&first.sps);
        let pps = Rc::clone(&first.pps);
        let header = &first.header;
        let frame_num = header.frame_num;
        let is_idr = first.idr;
        let is_reference = first.ref_idc != 0;
        if self.waiting_idr && !is_idr {
            return None;
        }
        if !is_idr
            && !self.first
            && is_reference
            && !sps.gaps_allowed
            && frame_num != (self.prev_frame_num + 1) % sps.max_frame_num()
        {
            self.resync();
            return None;
        }
        let reset = is_idr || self.prev_mmco5 || self.first;
        let offset = sps.frame_num_offset(
            (!reset).then_some((self.prev_frame_num, self.prev_offset)),
            frame_num,
        );
        let Some(poc) = self.picture_poc(&sps, header, offset, is_reference, reset) else {
            self.resync();
            return None;
        };
        let mut releases = Vec::new();
        let mut drop_held = false;
        // ITU-T H.264 (01/2012) 8.2.4 lists before 8.2.5 marking: marking
        // first evicts entries the current lists still need.
        let (l0, l1) = self.reference_lists(&sps, &pps, header, poc)?;
        if is_idr {
            drop_held = matches!(
                header.marking,
                h264::Marking::Idr {
                    no_output: true,
                    ..
                }
            );
            self.flush(&mut releases);
        } else if matches!(header.marking, h264::Marking::Adaptive(_)) {
            if !self.apply_mmco(&sps, header, frame_num, &mut releases) {
                self.resync();
                return None;
            }
        } else if is_reference {
            self.sliding_window(&sps, frame_num, &mut releases);
        }
        let id = is_reference.then(|| self.track_reference(header, poc, offset));
        self.waiting_idr = false;
        self.first = false;
        let (bytes, slice_offsets) = join_coded(slices, &[0, 0, 0, 1], |slice| &slice.payload);
        let Ok(narrowed) = u16::try_from(frame_num) else {
            self.resync();
            return None;
        };
        Some(Picture {
            l0,
            l1,
            poc,
            frame_num: narrowed,
            sps_id: sps.seq_parameter_set_id,
            pps_id: pps.pic_parameter_set_id,
            bytes,
            slice_offsets,
            coding: CodingFlags {
                is_reference,
                is_intra: header.slice_type.is_intra(),
                is_idr,
            },
            idr_pic_id: header.idr_pic_id.unwrap_or(0),
            id,
            releases,
            drop_held,
            resets_dpb: is_idr,
        })
    }

    /// Picture order count of the current picture.
    fn picture_poc(
        &self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        offset: i64,
        is_reference: bool,
        reset: bool,
    ) -> Option<i32> {
        match (&sps.poc_type, &header.poc) {
            (h264::PocType::Lsb, h264::SlicePoc::Lsb(lsb, bottom)) => {
                let prev = (!reset).then(|| self.poc.previous());
                sps.poc_type0(*lsb, *bottom, prev)
            }
            (h264::PocType::Delta, h264::SlicePoc::Deltas(first, second)) => {
                let abs = h264::abs_frame_num(offset, header.frame_num, is_reference);
                sps.poc_type1(abs, *first, second.unwrap_or(*first), is_reference)
            }
            (h264::PocType::FrameNum, h264::SlicePoc::None) => {
                h264::poc_type2(offset, header.frame_num, is_reference)
            }
            _ => None,
        }
    }

    /// Reference lists of the picture by slice type.
    fn reference_lists(
        &mut self,
        sps: &h264::Sps,
        pps: &h264::Pps,
        header: &h264::SliceHeader,
        poc: i32,
    ) -> Option<RefLists> {
        if header.slice_type.is_intra() {
            return Some((None, None));
        }
        let curr = header.frame_num;
        let max = sps.max_frame_num();
        let cands: Vec<h264::RefCandidate> = self
            .dpb
            .iter()
            .map(|entry| h264::RefCandidate {
                frame_num: entry.frame_num,
                pic_num: h264::frame_num_wrap(entry.frame_num, curr, max),
                poc: entry.poc,
                long_term_idx: entry.long_term,
            })
            .collect();
        let (mut l0, mut l1) = h264::initial_ref_lists(&cands, header.slice_type, poc);
        if !h264::apply_ref_mods(&mut l0, &cands, curr, max, &header.ref_mod_l0) {
            self.resync();
            return None;
        }
        l0.truncate(usize::from(header.num_ref_l0(pps)) + 1);
        let l0 = self.resolve_refs(&l0)?;
        if !header.slice_type.is_b() {
            return Some((Some(l0), None));
        }
        if !h264::apply_ref_mods(&mut l1, &cands, curr, max, &header.ref_mod_l1) {
            self.resync();
            return None;
        }
        l1.truncate(usize::from(header.num_ref_l1(pps)) + 1);
        let l1 = self.resolve_refs(&l1)?;
        Some((Some(l0), Some(l1)))
    }

    /// Reference pictures behind list `order`.
    fn resolve_refs(&self, order: &[usize]) -> Option<Vec<RefPicture>> {
        order
            .iter()
            .map(|index| {
                let entry = &self.dpb[*index];
                Some(RefPicture {
                    id: Some(entry.id),
                    frame_num: u16::try_from(entry.frame_num).ok()?,
                    poc: entry.poc,
                    long_term: entry.long_term,
                })
            })
            .collect()
    }

    /// Reference slot of the picture with its POC tracking state.
    fn track_reference(
        &mut self,
        header: &h264::SliceHeader,
        poc: i32,
        offset: i64,
    ) -> ReferenceId {
        let assigned = self.next_id.next();
        let long_term = header.marking.current_to_long();
        self.dpb.push(H264Entry {
            id: assigned,
            frame_num: header.frame_num,
            poc,
            long_term,
        });
        self.prev_frame_num = header.frame_num;
        self.prev_offset = offset;
        self.prev_mmco5 = header.marking.has_mmco5();
        if let h264::SlicePoc::Lsb(lsb, bottom) = header.poc {
            let delta = i64::from(bottom);
            let top = i64::from(poc) - delta.min(0);
            self.poc.observe(top - i64::from(lsb), lsb);
        }
        assigned
    }

    /// Mark every DPB entry unused.
    fn flush(&mut self, releases: &mut Vec<ReferenceId>) {
        releases.extend(self.dpb.iter().map(|entry| entry.id));
        self.dpb.clear();
    }

    /// Drop the DPB and wait for the next IDR.
    fn resync(&mut self) {
        self.dpb.clear();
        self.prev_mmco5 = false;
        self.waiting_idr = true;
    }

    /// Sliding-window marking for one reference picture.
    fn sliding_window(
        &mut self,
        sps: &h264::Sps,
        curr_frame_num: u32,
        releases: &mut Vec<ReferenceId>,
    ) {
        let shorts = self
            .dpb
            .iter()
            .filter(|entry| entry.long_term.is_none())
            .count();
        if shorts < usize::from(sps.max_num_ref_frames) {
            return;
        }
        // ITU-T H.264 (01/2012) 8.2.5.3 evicts the smallest FrameNumWrap.
        let max = sps.max_frame_num();
        let mut first = None;
        for (index, entry) in self.dpb.iter().enumerate() {
            if entry.long_term.is_some() {
                continue;
            }
            let earlier = first.is_none_or(|held: usize| {
                h264::frame_num_wrap(entry.frame_num, curr_frame_num, max)
                    < h264::frame_num_wrap(self.dpb[held].frame_num, curr_frame_num, max)
            });
            if earlier {
                first = Some(index);
            }
        }
        if let Some(index) = first {
            releases.push(self.dpb[index].id);
            self.dpb.remove(index);
        }
    }

    /// Adaptive marking operations; false answers unknown pictures by resync.
    fn apply_mmco(
        &mut self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        curr_frame_num: u32,
        releases: &mut Vec<ReferenceId>,
    ) -> bool {
        let h264::Marking::Adaptive(ops) = &header.marking else {
            return true;
        };
        let max = sps.max_frame_num();
        let wrapped = |frame_num: u32| h264::frame_num_wrap(frame_num, curr_frame_num, max);
        for op in ops {
            match *op {
                h264::Mmco::ForgetShort(diff) => {
                    let target = h264::mmco_short_target(diff, curr_frame_num);
                    let Some(index) = self.dpb.iter().position(|entry| {
                        entry.long_term.is_none() && i64::from(wrapped(entry.frame_num)) == target
                    }) else {
                        return false;
                    };
                    releases.push(self.dpb[index].id);
                    self.dpb.remove(index);
                }
                h264::Mmco::ForgetLong(index) => {
                    let Some(slot) = self
                        .dpb
                        .iter()
                        .position(|entry| entry.long_term == Some(index))
                    else {
                        return false;
                    };
                    releases.push(self.dpb[slot].id);
                    self.dpb.remove(slot);
                }
                h264::Mmco::ShortToLong(diff, index) => {
                    let target = h264::mmco_short_target(diff, curr_frame_num);
                    if let Some(held) = self
                        .dpb
                        .iter()
                        .position(|entry| entry.long_term == Some(index))
                    {
                        releases.push(self.dpb[held].id);
                        self.dpb.remove(held);
                    }
                    let Some(slot) = self.dpb.iter().position(|entry| {
                        entry.long_term.is_none() && i64::from(wrapped(entry.frame_num)) == target
                    }) else {
                        return false;
                    };
                    self.dpb[slot].long_term = Some(index);
                }
                h264::Mmco::SetMaxLong(max_plus1) => {
                    let max = max_plus1.checked_sub(1);
                    let mut held = Vec::new();
                    for entry in self.dpb.drain(..) {
                        let over = entry.long_term.is_some_and(|term| Some(term) > max);
                        if over {
                            releases.push(entry.id);
                        } else {
                            held.push(entry);
                        }
                    }
                    self.dpb = held;
                }
                h264::Mmco::MarkAllUnused => {
                    self.flush(releases);
                }
                h264::Mmco::CurrentToLong(index) => {
                    let mut held = Vec::new();
                    for entry in self.dpb.drain(..) {
                        if entry.long_term == Some(index) {
                            releases.push(entry.id);
                        } else {
                            held.push(entry);
                        }
                    }
                    self.dpb = held;
                }
            }
        }
        true
    }
}

/// Concatenated slice bytes with per-slice offsets behind `code`.
fn join_coded<S>(slices: &[S], code: &[u8], payload: impl Fn(&S) -> &[u8]) -> (Vec<u8>, Vec<u32>) {
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

/// H.265 driver echo of the slice reference sets.
#[derive(Clone, Debug)]
pub(crate) struct RpsEcho {
    /// Short-term set comes from the SPS.
    sps_flag: bool,
    /// Delta pictures of the predicted-from set.
    ref_count: u8,
    /// Syntax bit length of an explicit short-term set.
    bits: u16,
    /// Current-before ids in list order.
    before: Vec<Option<ReferenceId>>,
    /// Current-after ids in list order.
    after: Vec<Option<ReferenceId>>,
    /// Current long-term ids in list order.
    lt: Vec<Option<ReferenceId>>,
    /// Picture is an IRAP.
    is_irap: bool,
    /// Picture is an IDR.
    is_idr: bool,
    /// Active video set id.
    vps_id: u8,
}

impl RpsEcho {
    /// Short-term set comes from the SPS.
    #[must_use]
    pub fn sps_flag(&self) -> bool {
        self.sps_flag
    }

    /// Delta pictures of the predicted-from set.
    #[must_use]
    pub fn ref_count(&self) -> u8 {
        self.ref_count
    }

    /// Syntax bit length of an explicit short-term set.
    #[must_use]
    pub fn bits(&self) -> u16 {
        self.bits
    }

    /// Current-before ids in list order.
    #[must_use]
    pub fn before(&self) -> &[Option<ReferenceId>] {
        &self.before
    }

    /// Current-after ids in list order.
    #[must_use]
    pub fn after(&self) -> &[Option<ReferenceId>] {
        &self.after
    }

    /// Current long-term ids in list order.
    #[must_use]
    pub fn lt(&self) -> &[Option<ReferenceId>] {
        &self.lt
    }

    /// Picture is an IRAP.
    #[must_use]
    pub fn is_irap(&self) -> bool {
        self.is_irap
    }

    /// Picture is an IDR.
    #[must_use]
    pub fn is_idr(&self) -> bool {
        self.is_idr
    }

    /// Active video set id.
    #[must_use]
    pub fn vps_id(&self) -> u8 {
        self.vps_id
    }
}

/// H.265 picture with its driver echo.
#[derive(Clone, Debug)]
pub(crate) struct H265Picture {
    /// Shared picture payload.
    picture: Picture,
    /// Driver echo of the reference sets.
    echo: RpsEcho,
}

impl H265Picture {
    /// Shared picture payload.
    #[must_use]
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    /// Driver echo of the reference sets.
    #[must_use]
    pub fn echo(&self) -> &RpsEcho {
        &self.echo
    }
}

/// H.265 decoder instruction.
pub(crate) enum H265Instruction {
    /// Video set for the session parameters.
    Vps(Rc<h265::Vps>),
    /// Sequence set for the session parameters.
    Sps(Rc<h265::Sps>),
    /// Picture set for the session parameters.
    Pps(Rc<h265::Pps>),
    /// Picture for the session.
    Picture(Box<H265Picture>),
}

/// One H.265 DPB entry.
struct H265Entry {
    /// Stable identity.
    id: ReferenceId,
    /// Picture order count.
    poc: i32,
}

/// H.265 reference context: POC, RPS marking, reference union.
pub(crate) struct H265Context {
    /// Decoded pictures by order count.
    dpb: Vec<H265Entry>,
    /// Next reference identity.
    next_id: ReferenceId,
    /// POC anchor of the last temporal-0 picture.
    poc_tid0: Option<i32>,
    /// Leading pictures drop until the next IRAP.
    suppress_rasl: bool,
    /// Pictures drop until the next IDR or BLA.
    waiting: bool,
    /// First picture since reset.
    seen_first: bool,
}

impl H265Context {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dpb: Vec::new(),
            next_id: ReferenceId::default(),
            poc_tid0: None,
            suppress_rasl: false,
            waiting: false,
            seen_first: false,
        }
    }

    /// Instructions for one access unit.
    pub fn compile(&mut self, au: AccessUnit<h265::ParsedNalu>) -> Vec<H265Instruction> {
        let mut instructions = Vec::new();
        let mut slices = Vec::new();
        for nalu in au.into_nalus() {
            match nalu {
                h265::ParsedNalu::Vps(vps) => {
                    instructions.push(H265Instruction::Vps(vps));
                }
                h265::ParsedNalu::Sps(sps) => {
                    instructions.push(H265Instruction::Sps(sps));
                }
                h265::ParsedNalu::Pps(pps) => {
                    instructions.push(H265Instruction::Pps(pps));
                }
                h265::ParsedNalu::Slice(slice) => slices.push(*slice),
                h265::ParsedNalu::Other => {}
            }
        }
        if slices.is_empty() {
            return instructions;
        }
        if let Some(picture) = self.put_picture(&slices) {
            instructions.push(H265Instruction::Picture(Box::new(picture)));
        }
        instructions
    }

    /// One picture from its slices, or `None` when the picture drops.
    fn put_picture(&mut self, slices: &[h265::Slice]) -> Option<H265Picture> {
        let first = &slices[0];
        let unit = first.unit_type;
        if h265::is_rasl(unit) && self.suppress_rasl {
            return None;
        }
        if self.waiting && !(h265::is_idr(unit) || h265::is_bla(unit)) {
            return None;
        }
        let sps = Rc::clone(&first.sps);
        let header = &first.header;
        let new_sequence =
            h265::is_idr(unit) || h265::is_bla(unit) || (h265::is_cra(unit) && !self.seen_first);
        let Some(poc) = self.picture_poc(header, unit, first.temporal_id, &sps) else {
            self.resync();
            return None;
        };
        let mut releases = Vec::new();
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut lt = Vec::new();
        if new_sequence {
            releases.extend(self.dpb.iter().map(|entry| entry.id));
            self.dpb.clear();
        } else {
            if self.dpb.iter().any(|entry| entry.poc == poc) {
                self.resync();
                return None;
            }
            let Some(rps) = self.apply_rps(poc, header, &sps, &mut releases) else {
                self.resync();
                return None;
            };
            before = rps.before;
            after = rps.after;
            lt = rps.lt;
        }
        if header.slice_type != h265::SliceType::I
            && before.is_empty()
            && after.is_empty()
            && lt.is_empty()
        {
            self.resync();
            return None;
        }
        let id = self.next_id.next();
        self.dpb.push(H265Entry { id, poc });
        if h265::is_irap(unit) {
            self.suppress_rasl = new_sequence;
        }
        self.seen_first = true;
        self.waiting = false;
        let l0 = if header.slice_type == h265::SliceType::I {
            None
        } else {
            let mut union = Vec::new();
            union.extend(before.iter().copied());
            union.extend(after.iter().copied());
            union.extend(lt.iter().copied());
            Some(union)
        };
        let echo = RpsEcho {
            sps_flag: header.rps_bits.as_ref().is_some_and(|bits| !bits.explicit),
            ref_count: header
                .rps_bits
                .as_ref()
                .map_or(0, |bits| bits.ref_num_delta_pocs),
            bits: header.rps_bits.as_ref().map_or(0, |bits| bits.bits),
            before: before.iter().map(RefPicture::id).collect(),
            after: after.iter().map(RefPicture::id).collect(),
            lt: lt.iter().map(RefPicture::id).collect(),
            is_irap: h265::is_irap(unit),
            is_idr: h265::is_idr(unit),
            vps_id: first.vps.id,
        };
        let (bytes, slice_offsets) = join_coded(slices, &[0, 0, 1], |slice| &slice.payload);
        let picture = Picture {
            l0,
            l1: None,
            poc,
            frame_num: 0,
            sps_id: sps.id,
            pps_id: first.pps.id,
            bytes,
            slice_offsets,
            coding: CodingFlags {
                is_reference: !h265::is_slnr(unit),
                is_intra: header.slice_type == h265::SliceType::I,
                is_idr: h265::is_idr(unit),
            },
            idr_pic_id: 0,
            id: Some(id),
            releases,
            drop_held: header.no_output_prior && h265::is_irap(unit),
            resets_dpb: new_sequence,
        };
        Some(H265Picture { picture, echo })
    }

    /// Picture order count of the current picture.
    fn picture_poc(
        &mut self,
        header: &h265::SliceHeader,
        unit: u8,
        temporal_id: u8,
        sps: &h265::Sps,
    ) -> Option<i32> {
        let poc = if h265::is_idr(unit) {
            0
        } else {
            let lsb = header.poc_lsb?;
            sps.poc_from_lsb(self.poc_tid0, lsb, h265::is_bla(unit))?
        };
        if h265::anchors_tid0(unit, temporal_id) {
            self.poc_tid0 = Some(poc);
        }
        Some(poc)
    }

    /// Reference sets resolved against the DPB with evictions collected.
    fn apply_rps(
        &mut self,
        poc: i32,
        header: &h265::SliceHeader,
        sps: &h265::Sps,
        releases: &mut Vec<ReferenceId>,
    ) -> Option<ResolvedSets> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut unused = Vec::new();
        if let Some(rps) = header.rps.as_ref() {
            let negative = usize::from(rps.negative_count);
            let positive = usize::from(rps.positive_count);
            for (delta, used) in rps.negative[..negative]
                .iter()
                .zip(rps.negative_used[..negative].iter())
                .chain(
                    rps.positive[..positive]
                        .iter()
                        .zip(rps.positive_used[..positive].iter()),
                )
            {
                let entry_poc = poc.checked_add(*delta)?;
                if *delta == 0 {
                    return None;
                }
                let entry = RefPicture {
                    id: self.find_exact(entry_poc),
                    frame_num: 0,
                    poc: entry_poc,
                    long_term: None,
                };
                if *used {
                    if *delta < 0 {
                        before.push(entry);
                    } else {
                        after.push(entry);
                    }
                } else {
                    unused.push(entry);
                }
            }
        }
        let max = sps.max_poc_lsb();
        let mask = i32::try_from(max - 1).ok()?;
        let mut lt = Vec::new();
        for lts in &header.lt {
            let (entry_poc, id) = if lts.msb_present {
                let narrowed = lts.poc_msb_present(poc, max)?;
                (narrowed, self.find_exact(narrowed))
            } else if let Some(found) = self.find_lsb(i32::try_from(lts.poc_lsb).ok()?, mask) {
                found
            } else {
                (i32::try_from(lts.poc_lsb).ok()?, None)
            };
            let entry = RefPicture {
                id,
                frame_num: 0,
                poc: entry_poc,
                long_term: Some(0),
            };
            if lts.used {
                lt.push(entry);
            } else {
                unused.push(entry);
            }
        }
        let mut keep = Vec::new();
        for entry in before
            .iter()
            .chain(after.iter())
            .chain(unused.iter())
            .chain(lt.iter())
        {
            if let Some(id) = entry.id() {
                if !keep.contains(&id) {
                    keep.push(id);
                }
            }
        }
        for entry in &self.dpb {
            if !keep.contains(&entry.id) {
                releases.push(entry.id);
            }
        }
        self.dpb.retain(|entry| keep.contains(&entry.id));
        Some(ResolvedSets { before, after, lt })
    }

    /// DPB identity of the picture with `poc`, if present.
    fn find_exact(&self, poc: i32) -> Option<ReferenceId> {
        self.dpb
            .iter()
            .find(|entry| entry.poc == poc)
            .map(|entry| entry.id)
    }

    /// DPB picture matching `lsb` under `mask`, if present.
    fn find_lsb(&self, lsb: i32, mask: i32) -> Option<(i32, Option<ReferenceId>)> {
        self.dpb
            .iter()
            .find(|entry| entry.poc & mask == lsb)
            .map(|entry| (entry.poc, Some(entry.id)))
    }

    /// Drop the DPB and wait for the next IDR or BLA.
    fn resync(&mut self) {
        self.dpb.clear();
        self.poc_tid0 = None;
        self.waiting = true;
    }
}

impl Default for H265Context {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolved reference sets of one H.265 picture.
struct ResolvedSets {
    /// Current-before references in list order.
    before: Vec<RefPicture>,
    /// Current-after references in list order.
    after: Vec<RefPicture>,
    /// Current long-term references in list order.
    lt: Vec<RefPicture>,
}

/// One AV1 reference slot occupant.
#[derive(Clone)]
struct Av1Entry {
    /// Stored frame header.
    header: Rc<av1::FrameHeader>,
    /// Stable identity.
    id: ReferenceId,
}

/// One AV1 picture ready for the session.
pub(crate) struct Av1Picture {
    /// Shared picture shell: bytes, order, marking.
    picture: Picture,
    /// Parsed frame header.
    header: Rc<av1::FrameHeader>,
    /// Byte offset of each tile within the picture bytes.
    tile_offsets: Vec<u32>,
    /// Byte size of each tile.
    tile_sizes: Vec<u32>,
    /// Reference id per name, `LAST` through `ALTREF`.
    name_slots: [Option<ReferenceId>; av1::REFS_PER_FRAME],
    /// Skip-mode frame pair, 1-based reference names.
    skip_frames: [u8; 2],
    /// Evicted ids no name cites, released before the submit.
    pre: Vec<ReferenceId>,
    /// Evicted ids a name still cites, released after the submit.
    post: Vec<ReferenceId>,
}

impl Av1Picture {
    /// Shared picture shell: bytes, order, marking.
    pub fn picture(&self) -> &Picture {
        &self.picture
    }

    /// Parsed frame header.
    pub fn header(&self) -> &Rc<av1::FrameHeader> {
        &self.header
    }

    /// Byte offset of each tile within the picture bytes.
    pub fn tile_offsets(&self) -> &[u32] {
        &self.tile_offsets
    }

    /// Byte size of each tile.
    pub fn tile_sizes(&self) -> &[u32] {
        &self.tile_sizes
    }

    /// Reference id per name, `LAST` through `ALTREF`.
    pub fn name_slots(&self) -> &[Option<ReferenceId>; av1::REFS_PER_FRAME] {
        &self.name_slots
    }

    /// Skip-mode frame pair, 1-based reference names.
    pub fn skip_frames(&self) -> [u8; 2] {
        self.skip_frames
    }

    /// Evicted ids no name cites, released before the submit.
    pub fn pre(&self) -> &[ReferenceId] {
        &self.pre
    }

    /// Evicted ids a name still cites, released after the submit.
    pub fn post(&self) -> &[ReferenceId] {
        &self.post
    }
}

/// AV1 decoder instruction.
pub(crate) enum Av1Instruction {
    /// Sequence header for the session parameters.
    Sequence(Rc<av1::SequenceHeader>),
    /// Picture for the session.
    Picture(Box<Av1Picture>),
    /// Stored frame to display without decoding.
    ShowExisting {
        /// Stable identity of the stored frame.
        id: ReferenceId,
        /// Ids leaving the DPB with this display.
        releases: Vec<ReferenceId>,
    },
}

/// AV1 reference context over temporal units.
pub(crate) struct Av1Context {
    /// Active sequence header.
    seq: Option<Rc<av1::SequenceHeader>>,
    /// Reference slot occupants.
    slots: [Option<Av1Entry>; av1::TOTAL_REFS],
    /// Fresh reference ids.
    next_id: ReferenceId,
    /// Display order counter.
    display_seq: i32,
}

impl Av1Context {
    /// Empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: None,
            slots: [None, None, None, None, None, None, None, None],
            next_id: ReferenceId::default(),
            display_seq: 0,
        }
    }

    /// Instructions for one temporal unit.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a frame header or tile group fails to parse,
    /// or a frame arrives before any sequence header.
    pub fn compile(
        &mut self,
        unit: &av1::TemporalUnit<'_>,
    ) -> Result<Vec<Av1Instruction>, crate::error::VulkanError> {
        use crate::error::{ParseError, VulkanError};
        let mut instructions = Vec::new();
        let obus = unit.obus();
        let mut index = 0;
        while index < obus.len() {
            let obu = &obus[index];
            index += 1;
            match obu.kind() {
                av1::ObuType::SequenceHeader => {
                    let seq = Rc::new(av1::parse_sequence_header::<ParseError>(obu.payload())?);
                    if self.seq.as_ref().is_some_and(|cur| cur != &seq) {
                        self.slots = [None, None, None, None, None, None, None, None];
                    }
                    self.seq = Some(Rc::clone(&seq));
                    instructions.push(Av1Instruction::Sequence(seq));
                }
                av1::ObuType::TemporalDelimiter
                | av1::ObuType::Metadata
                | av1::ObuType::Padding => {}
                av1::ObuType::TileList => {
                    return Err(ParseError::Unsupported {
                        what: "av1 tile list",
                    }
                    .into());
                }
                av1::ObuType::Frame
                | av1::ObuType::FrameHeader
                | av1::ObuType::RedundantFrameHeader => {
                    let Some(seq) = self.seq.clone() else {
                        return Err(VulkanError::NoSession);
                    };
                    let views = self.views();
                    let (parsed, group) = if obu.kind() == av1::ObuType::Frame {
                        parse_frame_obu(obu.payload(), &seq, &views)?
                    } else {
                        let parsed = parse_frame_header(obu.payload(), &seq, &views)?;
                        (parsed, None)
                    };
                    let mut groups = group.into_iter().collect::<Vec<_>>();
                    if !parsed.header.show_existing_frame() {
                        while index < obus.len() && obus[index].kind() == av1::ObuType::TileGroup {
                            let expect = groups.last().map_or(0, |group| group.end + 1);
                            groups.push(parse_tile_group(
                                obus[index].payload(),
                                &parsed.header,
                                expect,
                            )?);
                            index += 1;
                        }
                    } else if index < obus.len() && obus[index].kind() == av1::ObuType::TileGroup {
                        return Err(ParseError::OutOfRange {
                            syntax: "tile group range",
                        }
                        .into());
                    }
                    instructions.extend(self.put_frame(parsed, &groups)?);
                }
                av1::ObuType::TileGroup => {
                    return Err(ParseError::OutOfRange {
                        syntax: "tile group range",
                    }
                    .into());
                }
            }
        }
        Ok(instructions)
    }

    /// Current slot views for frame header inference.
    fn views(&self) -> [av1::RefInfo; av1::TOTAL_REFS] {
        let mut views = core::array::from_fn(|_| av1::RefInfo::default());
        for (slot, view) in self.slots.iter().zip(views.iter_mut()) {
            let Some(entry) = slot else {
                continue;
            };
            let header = &entry.header;
            *view = av1::RefInfo {
                valid: true,
                frame_type: header.frame_type,
                frame_width: header.upscaled_width,
                frame_height: header.frame_height,
                upscaled_width: header.upscaled_width,
                render_width: header.render_width,
                render_height: header.render_height,
                order_hint: header.order_hint,
                frame_id: header.current_frame_id,
                loop_ref_deltas: header.loop_ref_deltas,
                loop_mode_deltas: header.loop_mode_deltas,
                seg_enabled: header.seg_enabled,
                seg_value: header.seg_value,
                gm_params: core::array::from_fn(|name| header.global_motion[name].params),
                grain: header.film_grain,
            };
        }
        views
    }

    /// Instructions for one parsed frame plus its tile groups.
    fn put_frame(
        &mut self,
        parsed: ParsedFrame,
        groups: &[TileGroup<'_>],
    ) -> Result<Vec<Av1Instruction>, crate::error::VulkanError> {
        use crate::error::ParseError;
        let header = Rc::new(parsed.header);
        if header.show_existing_frame() {
            return Ok(vec![self.put_show_existing(&header)?]);
        }
        let count = u32::from(header.tile_cols) * u32::from(header.tile_rows);
        if groups.is_empty()
            || groups
                .iter()
                .map(|group| group.end - group.start + 1)
                .sum::<u32>()
                != count
            || groups.last().is_some_and(|group| group.end + 1 != count)
        {
            return Err(ParseError::Truncated {
                syntax: "tile group range",
            }
            .into());
        }
        let before = self.slot_ids();
        if parsed.reset_all {
            self.slots = [None, None, None, None, None, None, None, None];
        }
        for (index, invalidated) in parsed.invalidated.iter().enumerate() {
            if *invalidated {
                self.slots[index] = None;
            }
        }
        for slot in header.ref_frame_idx {
            if !header.frame_type.is_intra() && self.slots[usize::from(slot)].is_none() {
                return Err(ParseError::MissingSet {
                    id: u32::from(slot),
                }
                .into());
            }
        }
        let id = self.next_id.next();
        let reference = header.refresh_frame_flags != 0;
        let mut name_slots = [None; av1::REFS_PER_FRAME];
        if !header.frame_type.is_intra() {
            for (index, slot) in header.ref_frame_idx.iter().enumerate() {
                name_slots[index] = self.slots[usize::from(*slot)]
                    .as_ref()
                    .map(|entry| entry.id);
            }
        }
        let joined = join_tiles(groups)?;
        let poc = self.display_seq;
        self.display_seq = self.display_seq.wrapping_add(1);
        if reference {
            let entry = Av1Entry {
                header: Rc::clone(&header),
                id,
            };
            for (index, slot) in self.slots.iter_mut().enumerate() {
                if header.refresh_frame_flags & (1_u8 << index) != 0 {
                    *slot = Some(entry.clone());
                }
            }
        }
        let releases = self.evicted(&before);
        let live: Vec<ReferenceId> = name_slots.iter().flatten().copied().collect();
        let (post, pre): (Vec<ReferenceId>, Vec<ReferenceId>) =
            releases.iter().copied().partition(|id| live.contains(id));
        let picture = Picture {
            l0: None,
            l1: None,
            poc,
            frame_num: 0,
            sps_id: 0,
            pps_id: 0,
            bytes: joined.bytes,
            slice_offsets: Vec::new(),
            coding: CodingFlags {
                is_reference: reference,
                is_intra: header.frame_type.is_intra(),
                is_idr: parsed.reset_all,
            },
            idr_pic_id: 0,
            id: reference.then_some(id),
            releases,
            drop_held: false,
            resets_dpb: parsed.reset_all,
        };
        Ok(vec![Av1Instruction::Picture(Box::new(Av1Picture {
            picture,
            header,
            tile_offsets: joined.offsets,
            tile_sizes: joined.sizes,
            name_slots,
            skip_frames: parsed.skip_frames,
            pre,
            post,
        }))])
    }

    /// Show-existing instruction for `header`, refreshing on key types.
    fn put_show_existing(
        &mut self,
        header: &av1::FrameHeader,
    ) -> Result<Av1Instruction, crate::error::VulkanError> {
        use crate::error::ParseError;
        let slot = usize::from(header.frame_to_show_map_idx);
        let Some(entry) = self.slots[slot].clone() else {
            return Err(ParseError::MissingSet {
                id: u32::from(header.frame_to_show_map_idx),
            }
            .into());
        };
        let before = self.slot_ids();
        if header.frame_type == av1::FrameType::Key {
            for slot in &mut self.slots {
                *slot = Some(entry.clone());
            }
        }
        Ok(Av1Instruction::ShowExisting {
            id: entry.id,
            releases: self.evicted(&before),
        })
    }

    /// Stable identity per slot, if occupied.
    fn slot_ids(&self) -> [Option<ReferenceId>; av1::TOTAL_REFS] {
        let mut ids = [None; av1::TOTAL_REFS];
        for (slot, id) in self.slots.iter().zip(ids.iter_mut()) {
            *id = slot.as_ref().map(|entry| entry.id);
        }
        ids
    }

    /// Prior occupants of `before` held by no slot now, deduplicated.
    fn evicted(&self, before: &[Option<ReferenceId>; av1::TOTAL_REFS]) -> Vec<ReferenceId> {
        let mut releases = Vec::new();
        for old in before.iter().flatten() {
            let held = self
                .slots
                .iter()
                .any(|slot| slot.as_ref().is_some_and(|entry| entry.id == *old));
            if !held && !releases.contains(old) {
                releases.push(*old);
            }
        }
        releases
    }
}

impl Default for Av1Context {
    fn default() -> Self {
        Self::new()
    }
}

/// One parsed frame: header plus slot effects.
struct ParsedFrame {
    /// Parsed frame header.
    header: av1::FrameHeader,
    /// Keyframe display cleared every slot.
    reset_all: bool,
    /// Slots invalidated during the parse.
    invalidated: [bool; av1::TOTAL_REFS],
    /// Skip-mode frame pair, 1-based reference names.
    skip_frames: [u8; 2],
}

/// Frame header of one OBU payload against `seq` and `views`.
///
/// Reads reference state for inference and validation; slot effects land
/// in the returned invalidations for the caller to apply.
fn parse_frame_header(
    payload: &[u8],
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
) -> Result<ParsedFrame, ParseError> {
    let mut refs = views.clone();
    let before = valid_snapshot(&refs);
    let (header, _) = av1::parse_frame_header::<ParseError>(payload, seq, &mut refs, 0, 0)?;
    Ok(convert_parsed(header, seq, views, &refs, before))
}

/// Frame OBU: header plus its tile group over one OBU payload.
fn parse_frame_obu<'a>(
    payload: &'a [u8],
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
) -> Result<(ParsedFrame, Option<TileGroup<'a>>), ParseError> {
    let mut refs = views.clone();
    let before = valid_snapshot(&refs);
    let (header, consumed) = av1::parse_frame_header::<ParseError>(payload, seq, &mut refs, 0, 0)?;
    let parsed = convert_parsed(header, seq, views, &refs, before);
    if parsed.header.show_existing_frame() {
        return Ok((parsed, None));
    }
    let aligned = check_align_zero(payload, consumed)?;
    let rest = payload.get(aligned / 8..).ok_or(ParseError::Truncated {
        syntax: "tile group align",
    })?;
    let group = parse_tile_group(rest, &parsed.header, 0)?;
    Ok((parsed, Some(group)))
}

/// Parsed frame with slot effects and skip-mode frames.
fn convert_parsed(
    header: av1::FrameHeader,
    seq: &av1::SequenceHeader,
    views: &[av1::RefInfo; av1::TOTAL_REFS],
    refs: &[av1::RefInfo; av1::TOTAL_REFS],
    before: [bool; av1::TOTAL_REFS],
) -> ParsedFrame {
    let reset_all = header.frame_type == av1::FrameType::Key && header.show_frame();
    let mut invalidated = [false; av1::TOTAL_REFS];
    if !reset_all {
        for (index, slot) in refs.iter().enumerate() {
            invalidated[index] = before[index] && !slot.valid;
        }
    }
    let skip_frames = if header.show_existing_frame() {
        [0, 0]
    } else {
        av1::skip_mode_frames(seq, &header, views).unwrap_or([0, 0])
    };
    ParsedFrame {
        header,
        reset_all,
        invalidated,
        skip_frames,
    }
}

/// Validity of `refs` per slot.
fn valid_snapshot(refs: &[av1::RefInfo; av1::TOTAL_REFS]) -> [bool; av1::TOTAL_REFS] {
    core::array::from_fn(|index| refs[index].valid)
}

/// Byte alignment past `consumed` bits, rejecting nonzero padding.
fn check_align_zero(payload: &[u8], consumed: usize) -> Result<usize, ParseError> {
    let aligned = consumed.next_multiple_of(8);
    for bit in consumed..aligned {
        if payload[bit / 8] >> (7 - bit % 8) & 1 != 0 {
            return Err(ParseError::OutOfRange {
                syntax: "tile group align",
            });
        }
    }
    Ok(aligned)
}

/// Tile group tile ranges over one OBU payload.
struct TileGroup<'a> {
    /// First tile index of the group.
    start: u32,
    /// Last tile index of the group.
    end: u32,
    /// Tile data past the group header.
    data: &'a [u8],
    /// Byte offset and size of each tile within `data`.
    tiles: Vec<(usize, usize)>,
}

/// Tile group of one OBU payload expecting tiles from `expect_start`.
fn parse_tile_group<'a>(
    payload: &'a [u8],
    header: &av1::FrameHeader,
    expect_start: u32,
) -> Result<TileGroup<'a>, ParseError> {
    let count = u32::from(header.tile_cols) * u32::from(header.tile_rows);
    if count == 0 {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    let head = av1::tile_group_range::<ParseError>(
        payload,
        usize::try_from(count).map_err(|_| ParseError::OutOfRange {
            syntax: "tile group range",
        })?,
        u8::try_from(av1::tile_log2(1, u32::from(header.tile_cols))).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "tile group range",
            }
        })?,
        u8::try_from(av1::tile_log2(1, u32::from(header.tile_rows))).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "tile group range",
            }
        })?,
    )?;
    let start = u32::try_from(head.start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = u32::try_from(head.end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    if start < expect_start || start >= count || end < start || end >= count {
        return Err(ParseError::OutOfRange {
            syntax: "tile group range",
        });
    }
    check_align_zero(payload, head.consumed_bits)?;
    let tiles = split_tiles(head.data, start, end, header.tile_size_bytes.max(1))?;
    Ok(TileGroup {
        start,
        end,
        data: head.data,
        tiles,
    })
}

/// Tile ranges of `data` for tiles `start` through `end`.
fn split_tiles(
    data: &[u8],
    start: u32,
    end: u32,
    size_bytes: u8,
) -> Result<Vec<(usize, usize)>, ParseError> {
    let start = usize::try_from(start).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    let end = usize::try_from(end).map_err(|_| ParseError::OutOfRange {
        syntax: "tile group range",
    })?;
    av1::split_tiles::<ParseError>(data, start, end, size_bytes)
}

/// Concatenated tile bytes plus per-tile offsets and sizes.
struct JoinedTiles {
    /// Concatenated tile bytes.
    bytes: Vec<u8>,
    /// Byte offset of each tile.
    offsets: Vec<u32>,
    /// Byte size of each tile.
    sizes: Vec<u32>,
}

/// Concatenated tile bytes plus per-tile offsets and sizes.
fn join_tiles(groups: &[TileGroup<'_>]) -> Result<JoinedTiles, ParseError> {
    let tiles: usize = groups.iter().map(|group| group.tiles.len()).sum();
    let total: usize = groups
        .iter()
        .flat_map(|group| group.tiles.iter().map(|(_, size)| size))
        .sum();
    let mut joined = JoinedTiles {
        bytes: Vec::with_capacity(total),
        offsets: Vec::with_capacity(tiles),
        sizes: Vec::with_capacity(tiles),
    };
    for group in groups {
        for (offset, size) in &group.tiles {
            let end = offset.saturating_add(*size);
            if group.data.len() < end {
                return Err(ParseError::Truncated {
                    syntax: "tile data",
                });
            }
            joined
                .offsets
                .push(
                    u32::try_from(joined.bytes.len()).map_err(|_| ParseError::OutOfRange {
                        syntax: "tile offset",
                    })?,
                );
            joined
                .sizes
                .push(u32::try_from(*size).map_err(|_| ParseError::OutOfRange {
                    syntax: "tile size",
                })?);
            joined.bytes.extend_from_slice(&group.data[*offset..end]);
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::{Av1Context, H264Context, H265Context, ReorderBuffer, SlotBook};
    use crate::parse::au::{AccessUnit, AuSplitter};
    use crate::parse::test_support::{hex, split264, split265};

    #[test]
    fn join_tiles_concatenates_groups() {
        let first = super::TileGroup {
            start: 0,
            end: 1,
            data: &[9, 1, 2, 3, 4, 5],
            tiles: vec![(1, 3), (4, 2)],
        };
        let second = super::TileGroup {
            start: 2,
            end: 2,
            data: &[6, 7, 8],
            tiles: vec![(0, 3)],
        };
        let joined = super::join_tiles(&[first, second]).expect("tiles");
        assert_eq!(joined.bytes, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(joined.offsets, vec![0, 3, 5]);
        assert_eq!(joined.sizes, vec![3, 2, 3]);
        let short = super::TileGroup {
            start: 0,
            end: 0,
            data: &[1, 2],
            tiles: vec![(1, 2)],
        };
        assert!(super::join_tiles(&[short]).is_err());
    }

    #[test]
    fn evicted_reports_only_unheld_priors() {
        use super::{Av1Entry, ReferenceId};
        use std::rc::Rc;
        let header = Rc::new(crate::parse::av1::FrameHeader::default());
        let mut context = Av1Context::new();
        context.slots[0] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(3),
        });
        context.slots[1] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(7),
        });
        context.slots[2] = Some(Av1Entry {
            header: Rc::clone(&header),
            id: ReferenceId(7),
        });
        let before = [
            Some(ReferenceId(3)),
            Some(ReferenceId(5)),
            Some(ReferenceId(7)),
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(context.evicted(&before), vec![ReferenceId(5)]);
    }

    /// Fork `demo.h264` parameter sets and first slices.
    const SPS264: &str = "6764000dacb20283f60220000003002000000641e2854900";
    /// Fork `demo.h264` PPS.
    const PPS264: &str = "68ebc3cb22c0";
    /// Fork `demo.h264` IDR slice head.
    const IDR264: &str =
        "6588840cfffef6ecbe053614c85094a04007bb443688f75792a97d0e881b266f395d1803e3eea5efbae04cafddfd1d8f5aa3b94b6acca893a915d63dcd";
    /// Fork `demo.h264` first P slice head.
    const P264: &str = "419a3b10cffe9e2a5997d0339bc66a0ec39c80006fafc5a3a0c7e796ec144229f6721f2948";
    /// Fork `demo.h264` second P slice head.
    const P264B: &str =
        "419a4f0864ca610cfffe9e11dd171650e37a8a98e212dfd3996f00275b7083b6941776a47c";
    /// Fork `demo.h265` VPS.
    const VPS265: &str = "40010c01ffff01600000030090000003000003003c92809000";
    /// Fork `demo.h265` SPS.
    const SPS265: &str =
        "42010101600000030090000003000003003ca00a080f16592a4932bc05a0200000030020000003032100";
    /// Fork `demo.h265` PPS.
    const PPS265: &str = "4401c172b4624000";
    /// Fork `demo.h265` IDR slice head.
    const IDR265: &str =
        "2801af1d20a965478be0af3f4ae63151b2fffd273e7fa20b405b9ec15d0337a20427f6e32f9f583c57fee84d42429d33ffe3ff4aff62991873852f99fb8bf1a358a444ab906f2a97e66aa67a1c";
    /// Fork `demo.h265` first inter slice head.
    const T265: &str =
        "0201d0097e10c63a430a3d70fd7fb96259b8fc398744b4135036fbd45ed7fcdb5f3d45c649013c88aa2c8c2fdf";

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
    fn poc_type2_fits_boundary_but_not_past() {
        assert_eq!(
            crate::parse::h264::poc_type2(1_073_741_823, 0, true),
            Some(2_147_483_646)
        );
        assert_eq!(crate::parse::h264::poc_type2(1_073_741_824, 0, true), None);
        assert_eq!(crate::parse::h264::poc_type2(9, 1, false), Some(19));
    }

    /// Access units out of parsed H.264 `payloads`.
    fn units264(payloads: &[Vec<u8>]) -> Vec<AccessUnit<super::h264::ParsedNalu>> {
        let mut parser = super::h264::H264Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split264(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            if let Some(unit) = splitter.put(parsed) {
                units.push(unit);
            }
        }
        if let Some(unit) = splitter.flush() {
            units.push(unit);
        }
        units
    }

    #[test]
    fn h264_demo_idr_then_p() {
        let mut units = units264(&[hex(SPS264), hex(PPS264), hex(IDR264), hex(P264)]);
        assert_eq!(units.len(), 2);
        let mut context = H264Context::new();
        let first = context.compile(units.remove(0));
        assert_eq!(first.len(), 3);
        let Some(super::H264Instruction::Picture(idr)) = first.into_iter().next_back() else {
            panic!("want picture");
        };
        assert_eq!(idr.poc(), 0);
        assert!(idr.resets_dpb());
        assert!(idr.l0().is_none());
        assert!(idr.is_reference());
        let idr_id = idr.id().expect("idr id");
        let mut context = H264Context::default();
        let mut replay = units264(&[hex(SPS264), hex(PPS264), hex(IDR264), hex(P264)]);
        let _ = context.compile(replay.remove(0));
        let second = context.compile(replay.remove(0));
        assert_eq!(second.len(), 1);
        let Some(super::H264Instruction::Picture(p)) = second.into_iter().next() else {
            panic!("want picture");
        };
        assert!(p.poc() > 0);
        assert!(!p.resets_dpb());
        let l0 = p.l0().expect("p has l0");
        assert_eq!(l0.len(), 1);
        assert_eq!(l0[0].id(), Some(idr_id));
        assert_eq!(p.slice_offsets(), &[0]);
        assert_eq!(&p.bytes()[..4], &[0, 0, 0, 1]);
    }

    #[test]
    fn h264_repeated_frame_resyncs_then_recovers() {
        let mut context = H264Context::new();
        let mut replay = units264(&[hex(SPS264), hex(PPS264), hex(IDR264), hex(P264)]);
        let _ = context.compile(replay.remove(0));
        let _ = context.compile(replay.remove(0));
        let mut dup = units264(&[hex(SPS264), hex(PPS264), hex(P264)]);
        let dropped = context.compile(dup.remove(0));
        assert!(dropped
            .iter()
            .all(|instruction| !matches!(instruction, super::H264Instruction::Picture(_))));
        let mut idr = units264(&[hex(SPS264), hex(PPS264), hex(IDR264)]);
        let recovered = context.compile(idr.remove(0));
        let Some(super::H264Instruction::Picture(picture)) = recovered.into_iter().next_back()
        else {
            panic!("want picture");
        };
        assert!(picture.resets_dpb());
    }

    #[test]
    fn h264_second_p_keeps_window() {
        let mut context = H264Context::new();
        let mut replay = units264(&[hex(SPS264), hex(PPS264), hex(IDR264), hex(P264), hex(P264B)]);
        let _ = context.compile(replay.remove(0));
        let _ = context.compile(replay.remove(0));
        let third = context.compile(replay.remove(0));
        let Some(super::H264Instruction::Picture(p)) = third.into_iter().next() else {
            panic!("want picture");
        };
        let l0 = p.l0().expect("l0");
        assert_eq!(l0.len(), 2);
        assert_eq!(l0[0].poc(), 2);
        assert_eq!(l0[1].poc(), 0);
        assert_eq!(p.releases(), []);
    }

    /// Single-slot 64x64 fixture SPS (`max_num_ref_frames` 1, type 2).
    const SPS1: &str = "6764000aacb42136022000000030020000000781e244d400";
    /// Single-slot 64x64 fixture PPS.
    const PPS1: &str = "68ef0fcb";
    /// Single-slot 64x64 fixture IDR slice head.
    const IDR1: &str =
        "6588841dffbc35c57861ffe1d5931b630a8e06ea5eac31d954786d4359becf7fee26d3375d779cde1e1b686dfad19aa274aa15132fa314135310ed550d38020a7a493c9545c4608c385801ca225f0fa45d2a5c62a8f8d19a5f229c1a18111423ffc6c89ffaed6ae99a2556546bd7089cdfb3c059e15a8226737dbe6bf75af56e78df9aec3a94717b75f04f9d57aa4ab121dc49b1bcff6fa27e10498f5c8ee8ceedf7cb820c14d1710726daf53e0bde5bd7bc7002c669738687cadf4a142efe26bb1a8980996e65925cfa8d6a24ab3f66a198e5f44d77a44ebc634f41eedf268d837948303a5f1ed9f6a2f5a4138c353f0f0acfcdff731b35322f0e47d535d50bfe4680fc9c30a7ef51d6ba381cbe50b01a89e8c24d16baeb7765f2db0b4f69ac259cc56356b62ccd6f50077d4f64babcfc9b68195c5258d8ac175d9b460991ce4ad98a9dca7824ae06512fca561166b72e02571f9fcd8b0cef4d371e52e0e9bcdb5a9cde32d7b431b8fa8ea56106221129b06a39d68504f26cb674d897e6015a41e2960738f225b67ba24d42d87b4130cac70fe213e3de780395f85084da032e11383e4580a9b2be7d912d5bf0ba678b4665ea5f648f6a2d82575cab35be4f0dea19d577d133ba0d08c42291dd876e555d2a9e8c06f5d8355cef35967674e052500eee4d8c226e09c79a4f440abd68dd37020a28f483d707ba5552b79631b93d345cae6d79a64366399912bc895927df5eb23e16a48838491afb5c162f00b3de7c2bf1f5a0f5dd2d3e6407c9125b923279d9717a0b11f25e562259b4145edd1f8be937891cfae7417b9fec1111697bc38624d42b870461f560793c5956edfe5dda9e7c7b2b6c026182b7f1b43f57f3a4a4b3d9f5d62a799c7f37248b836fd378e8eebe56e9674700";
    /// Single-slot 64x64 fixture first P slice head.
    const P1A: &str = "419a26210f463f3073e8eea39700";
    /// Single-slot 64x64 fixture second P slice head.
    const P1B: &str = "419a46210f10ca80102c4d099d4b00";

    #[test]
    fn h264_single_slot_p_keeps_previous_reference() {
        let mut units = units264(&[hex(SPS1), hex(PPS1), hex(IDR1), hex(P1A), hex(P1B)]);
        assert_eq!(units.len(), 3);
        let mut context = H264Context::new();
        let _ = context.compile(units.remove(0));
        let second = context.compile(units.remove(0));
        let Some(super::H264Instruction::Picture(first)) = second.into_iter().next() else {
            panic!("want picture");
        };
        let l0 = first.l0().expect("p1 l0");
        assert_eq!(l0.len(), 1);
        assert_eq!(l0[0].poc(), 0);
        let third = context.compile(units.remove(0));
        let Some(super::H264Instruction::Picture(second)) = third.into_iter().next() else {
            panic!("want picture");
        };
        let l0 = second.l0().expect("p2 l0");
        assert_eq!(l0.len(), 1);
        assert_eq!(l0[0].poc(), 2);
    }

    #[test]
    fn sliding_window_evicts_smallest_wrap_not_raw() {
        let units = split264(&[hex(SPS264)]);
        let mut parser = super::h264::H264Parser::new();
        let super::h264::ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let cap = usize::from(sps.max_num_ref_frames);
        let max = sps.max_frame_num();
        assert!(cap >= 2 && max >= u32::try_from(cap).expect("cap") + 2);
        let mut context = H264Context::new();
        let curr = u32::try_from(cap).expect("cap") - 2;
        let mut frames = vec![max - 2, max - 1];
        frames.extend(0..curr);
        assert_eq!(frames.len(), cap);
        let mut wrapped = None;
        for frame in frames {
            let id = context.next_id.next();
            if frame == max - 2 {
                wrapped = Some(id);
            }
            context.dpb.push(super::H264Entry {
                id,
                frame_num: frame,
                poc: 0,
                long_term: None,
            });
        }
        let mut releases = Vec::new();
        context.sliding_window(&sps, curr, &mut releases);
        assert_eq!(releases, vec![wrapped.expect("wrapped id")]);
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

    /// Access units out of parsed H.265 `payloads`.
    fn units265(payloads: &[Vec<u8>]) -> Vec<AccessUnit<super::h265::ParsedNalu>> {
        let mut parser = super::h265::H265Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split265(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            if let Some(unit) = splitter.put(parsed) {
                units.push(unit);
            }
        }
        if let Some(unit) = splitter.flush() {
            units.push(unit);
        }
        units
    }

    #[test]
    fn h265_demo_idr_then_inter() {
        let mut context = H265Context::new();
        let mut replay = units265(&[
            hex(VPS265),
            hex(SPS265),
            hex(PPS265),
            hex(IDR265),
            hex(T265),
        ]);
        assert_eq!(replay.len(), 2);
        let first = context.compile(replay.remove(0));
        assert_eq!(first.len(), 4);
        let Some(super::H265Instruction::Picture(idr)) = first.into_iter().next_back() else {
            panic!("want picture");
        };
        assert_eq!(idr.picture().poc(), 0);
        assert!(idr.picture().resets_dpb());
        assert!(idr.picture().l0().is_none());
        assert!(idr.echo().is_idr());
        let idr_id = idr.picture().id().expect("idr id");
        let second = context.compile(replay.remove(0));
        let Some(super::H265Instruction::Picture(p)) = second.into_iter().next() else {
            panic!("want picture");
        };
        assert!(p.picture().poc() > 0);
        assert!(!p.picture().resets_dpb());
        let union = p.picture().l0().expect("union");
        assert!(!union.is_empty());
        assert!(union.iter().any(|entry| entry.id() == Some(idr_id)));
        assert!(!p.echo().is_idr());
        assert_eq!(&p.picture().bytes()[..3], &[0, 0, 1]);
    }

    #[test]
    fn h265_duplicate_poc_resyncs_then_recovers() {
        let mut context = H265Context::default();
        let mut replay = units265(&[
            hex(VPS265),
            hex(SPS265),
            hex(PPS265),
            hex(IDR265),
            hex(T265),
        ]);
        let _ = context.compile(replay.remove(0));
        let _ = context.compile(replay.remove(0));
        let mut dup = units265(&[hex(VPS265), hex(SPS265), hex(PPS265), hex(T265)]);
        let dropped = context.compile(dup.remove(0));
        assert!(dropped
            .iter()
            .all(|instruction| !matches!(instruction, super::H265Instruction::Picture(_))));
        let mut idr = units265(&[hex(VPS265), hex(SPS265), hex(PPS265), hex(IDR265)]);
        let recovered = context.compile(idr.remove(0));
        let Some(super::H265Instruction::Picture(picture)) = recovered.into_iter().next_back()
        else {
            panic!("want picture");
        };
        assert!(picture.picture().resets_dpb());
    }
}
