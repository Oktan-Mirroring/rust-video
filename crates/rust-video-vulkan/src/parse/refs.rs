//! Reference picture tracking and decode instructions.
//!
//! Each context turns one access unit into parameter-set updates plus at
//! most one picture: reference lists resolved to stable ids, DPB marking
//! applied, unknown references answered by resync, never by guessing.

use std::rc::Rc;

use crate::error::ParseError;
use crate::parse::au::AccessUnit;
use crate::parse::h264;
use crate::parse::h265;

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
    frame_num: u16,
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
    prev_frame_num: u16,
    /// Previous frame-number offset.
    prev_offset: i32,
    /// Previous POC most significant bits.
    prev_msb: i64,
    /// Previous POC least significant bits.
    prev_lsb: u32,
    /// Previous reference picture ran MMCO 5.
    prev_mmco5: bool,
    /// Highest long-term index in use.
    max_long_term: Option<u32>,
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
            prev_msb: 0,
            prev_lsb: 0,
            prev_mmco5: false,
            max_long_term: None,
            waiting_idr: false,
            first: true,
        }
    }

    /// Instructions for one access unit.
    pub fn compile(&mut self, au: AccessUnit<h264::Nalu>) -> Vec<H264Instruction> {
        let mut instructions = Vec::new();
        let mut slices = Vec::new();
        for nalu in au.into_nalus() {
            match nalu.parsed() {
                h264::ParsedNalu::Sps(sps) => {
                    instructions.push(H264Instruction::Sps(Rc::clone(sps)));
                }
                h264::ParsedNalu::Pps(pps) => {
                    instructions.push(H264Instruction::Pps(Rc::clone(pps)));
                }
                h264::ParsedNalu::Slice(slice) => slices.push(slice.clone()),
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
        let sps = Rc::clone(first.sps());
        let pps = Rc::clone(first.pps());
        let header = first.header();
        let is_idr = first.nal().is_idr();
        let is_reference = first.nal().ref_idc() != 0;
        if self.waiting_idr && !is_idr {
            return None;
        }
        if !is_idr
            && !self.first
            && is_reference
            && !sps.gaps_in_frame_num_allowed()
            && !self.check_gap(&sps, header.frame_num())
        {
            self.resync();
            return None;
        }
        let offset = self.frame_num_offset(&sps, header.frame_num(), is_idr);
        let Some(poc) = self.picture_poc(&sps, header, offset, is_idr, is_reference) else {
            self.resync();
            return None;
        };
        let mut releases = Vec::new();
        let mut drop_held = false;
        // ITU-T H.264 (01/2012) 8.2.4 lists before 8.2.5 marking: marking
        // first evicts entries the current lists still need.
        let (l0, l1) = self.reference_lists(&sps, header, poc)?;
        if is_idr {
            drop_held = header.marking().no_output_of_prior_pics();
            self.flush(&mut releases);
            self.max_long_term = header.marking().long_term_reference().then_some(0);
        } else if header.marking().adaptive() {
            if !self.apply_mmco(&sps, header, &mut releases) {
                self.resync();
                return None;
            }
        } else if is_reference {
            self.sliding_window(&sps, header.frame_num(), &mut releases);
        }
        let id = is_reference.then(|| self.track_reference(&sps, header, poc, offset, is_idr));
        self.waiting_idr = false;
        self.first = false;
        let (bytes, slice_offsets) = join_slices(slices);
        Some(Picture {
            l0,
            l1,
            poc,
            frame_num: header.frame_num(),
            sps_id: sps.id(),
            pps_id: pps.id(),
            bytes,
            slice_offsets,
            coding: CodingFlags {
                is_reference,
                is_intra: header.slice_type() == h264::SliceType::I,
                is_idr,
            },
            idr_pic_id: header.idr_pic_id(),
            id,
            releases,
            drop_held,
            resets_dpb: is_idr,
        })
    }

    /// Reference lists of the picture by slice type.
    fn reference_lists(
        &mut self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        poc: i32,
    ) -> Option<RefLists> {
        match header.slice_type() {
            h264::SliceType::I => Some((None, None)),
            h264::SliceType::P => {
                let Some(list) = self.init_list(sps, header, poc, true) else {
                    self.resync();
                    return None;
                };
                Some((Some(list), None))
            }
            h264::SliceType::B => {
                let (Some(before), Some(after)) = (
                    self.init_list(sps, header, poc, true),
                    self.init_list(sps, header, poc, false),
                ) else {
                    self.resync();
                    return None;
                };
                Some((Some(before), Some(after)))
            }
        }
    }

    /// Reference slot of the picture with its POC tracking state.
    fn track_reference(
        &mut self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        poc: i32,
        offset: i32,
        is_idr: bool,
    ) -> ReferenceId {
        let assigned = self.next_id.next();
        let long_term = if is_idr && header.marking().long_term_reference() {
            Some(0)
        } else {
            Self::current_long_term(header)
        };
        self.dpb.push(H264Entry {
            id: assigned,
            frame_num: header.frame_num(),
            poc,
            long_term,
        });
        self.prev_frame_num = header.frame_num();
        self.prev_offset = offset;
        self.prev_mmco5 = header.marking().ops().contains(&h264::Mmco::Reset);
        if sps.pic_order_cnt_type() == 0 {
            let delta = i64::from(header.delta_pic_order_cnt_bottom());
            let top = i64::from(poc) - delta.min(0);
            self.prev_msb = top - i64::from(header.pic_order_cnt_lsb());
            self.prev_lsb = header.pic_order_cnt_lsb();
        }
        assigned
    }

    /// Frame-number continuity against the previous picture.
    fn check_gap(&self, sps: &h264::Sps, frame_num: u16) -> bool {
        let max = 1_u32 << sps.frame_num_bits();
        let expected = (u32::from(self.prev_frame_num) + 1) % max;
        u32::from(frame_num) == expected
    }

    /// Frame-number offset of the current picture.
    fn frame_num_offset(&self, sps: &h264::Sps, frame_num: u16, is_idr: bool) -> i32 {
        if is_idr || self.prev_mmco5 || self.first {
            return 0;
        }
        if self.prev_frame_num > frame_num {
            let max = 1_i32 << sps.frame_num_bits();
            self.prev_offset.saturating_add(max)
        } else {
            self.prev_offset
        }
    }

    /// Picture order count of the current picture.
    fn picture_poc(
        &self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        offset: i32,
        is_idr: bool,
        is_reference: bool,
    ) -> Option<i32> {
        match sps.pic_order_cnt_type() {
            0 => poc_type0(sps, self, header, is_idr),
            1 => poc_type1(sps, self, header, offset, is_reference),
            _ => poc_type2(offset, header.frame_num(), is_reference),
        }
    }

    /// Mark every DPB entry unused.
    fn flush(&mut self, releases: &mut Vec<ReferenceId>) {
        releases.extend(self.dpb.iter().map(|entry| entry.id));
        self.dpb.clear();
        self.max_long_term = None;
    }

    /// Drop the DPB and wait for the next IDR.
    fn resync(&mut self) {
        self.dpb.clear();
        self.max_long_term = None;
        self.prev_mmco5 = false;
        self.waiting_idr = true;
    }

    /// Sliding-window marking for one reference picture.
    fn sliding_window(
        &mut self,
        sps: &h264::Sps,
        curr_frame_num: u16,
        releases: &mut Vec<ReferenceId>,
    ) {
        let shorts = self
            .dpb
            .iter()
            .filter(|entry| entry.long_term.is_none())
            .count();
        if shorts < usize::from(sps.max_num_ref_frames()) {
            return;
        }
        // ITU-T H.264 (01/2012) 8.2.5.3 evicts the smallest FrameNumWrap.
        let max = 1_i32 << sps.frame_num_bits();
        let curr = i32::from(curr_frame_num);
        let mut first = None;
        for (index, entry) in self.dpb.iter().enumerate() {
            if entry.long_term.is_some() {
                continue;
            }
            let earlier = first.is_none_or(|held: usize| {
                pic_num_of(entry.frame_num, curr, max)
                    < pic_num_of(self.dpb[held].frame_num, curr, max)
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
        releases: &mut Vec<ReferenceId>,
    ) -> bool {
        let max = 1_i32 << sps.frame_num_bits();
        let curr = i32::from(header.frame_num());
        for op in header.marking().ops() {
            match op {
                h264::Mmco::ForgetShort(gap) => {
                    let Ok(gap) = i32::try_from(*gap) else {
                        return false;
                    };
                    let Some(target) = gap.checked_add(1).and_then(|step| curr.checked_sub(step))
                    else {
                        return false;
                    };
                    let Some(index) = self.dpb.iter().position(|entry| {
                        entry.long_term.is_none()
                            && pic_num_of(entry.frame_num, curr, max) == target
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
                        .position(|entry| entry.long_term == Some(*index))
                    else {
                        return false;
                    };
                    releases.push(self.dpb[slot].id);
                    self.dpb.remove(slot);
                }
                h264::Mmco::LongToShort { index, max_plus1 } => {
                    let Some(slot) = self
                        .dpb
                        .iter()
                        .position(|entry| entry.long_term == Some(*index))
                    else {
                        return false;
                    };
                    self.dpb[slot].long_term = None;
                    self.max_long_term = max_plus1.checked_sub(1);
                }
                h264::Mmco::SetMaxLong(max_plus1) => {
                    self.max_long_term = max_plus1.checked_sub(1);
                    let mut held = Vec::new();
                    for entry in self.dpb.drain(..) {
                        let over = entry
                            .long_term
                            .is_some_and(|term| Some(term) > self.max_long_term);
                        if over {
                            releases.push(entry.id);
                        } else {
                            held.push(entry);
                        }
                    }
                    self.dpb = held;
                }
                h264::Mmco::Reset => {
                    self.flush(releases);
                }
                h264::Mmco::CurrentToLong(index) => {
                    let mut held = Vec::new();
                    for entry in self.dpb.drain(..) {
                        if entry.long_term == Some(*index) {
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

    /// Long-term index the current picture takes, if any.
    fn current_long_term(header: &h264::SliceHeader) -> Option<u32> {
        header.marking().ops().iter().find_map(|op| match op {
            h264::Mmco::CurrentToLong(index) => Some(*index),
            _ => None,
        })
    }

    /// Initial reference list with modifications applied.
    fn init_list(
        &self,
        sps: &h264::Sps,
        header: &h264::SliceHeader,
        poc: i32,
        l0: bool,
    ) -> Option<Vec<RefPicture>> {
        let curr = i32::from(header.frame_num());
        let max = 1_i32 << sps.frame_num_bits();
        let mut shorts: Vec<&H264Entry> = self
            .dpb
            .iter()
            .filter(|entry| entry.long_term.is_none())
            .collect();
        let mut longs: Vec<&H264Entry> = self
            .dpb
            .iter()
            .filter(|entry| entry.long_term.is_some())
            .collect();
        if header.slice_type() == h264::SliceType::P {
            shorts.sort_by_key(|entry| {
                std::cmp::Reverse(i64::from(pic_num_of(entry.frame_num, curr, max)))
            });
        } else if l0 {
            shorts.sort_by_key(|entry| l0_key(entry.poc, poc));
        } else {
            shorts.sort_by_key(|entry| l1_key(entry.poc, poc));
        }
        longs.sort_by_key(|entry| entry.long_term);
        let mut list: Vec<RefPicture> = shorts
            .iter()
            .chain(longs.iter())
            .map(|entry| RefPicture {
                id: Some(entry.id),
                frame_num: entry.frame_num,
                poc: entry.poc,
                long_term: entry.long_term,
            })
            .collect();
        let mods = if l0 {
            header.ref_mods().l0()
        } else {
            header.ref_mods().l1()
        };
        if !mods.is_empty() && !apply_ref_mods(&mut list, curr, max, mods) {
            return None;
        }
        let active = if l0 {
            header.num_ref_idx_l0_active()
        } else {
            header.num_ref_idx_l1_active()
        };
        let active = usize::try_from(active).unwrap_or(usize::MAX);
        list.truncate(active);
        Some(list)
    }
}

/// Wrap-relative picture number of one short-term entry.
fn pic_num_of(frame_num: u16, curr: i32, max: i32) -> i32 {
    let num = i32::from(frame_num);
    if num > curr {
        num - max
    } else {
        num
    }
}

/// L0 sort key: past pictures by descending POC, then future ascending.
fn l0_key(entry: i32, poc: i32) -> (u8, i64) {
    if entry < poc {
        (0, -i64::from(entry))
    } else {
        (1, i64::from(entry))
    }
}

/// L1 sort key: future pictures by ascending POC, then past descending.
fn l1_key(entry: i32, poc: i32) -> (u8, i64) {
    if entry > poc {
        (0, i64::from(entry))
    } else {
        (1, -i64::from(entry))
    }
}

/// Reference-list modification loop over the initial list.
fn apply_ref_mods(list: &mut Vec<RefPicture>, curr: i32, max: i32, mods: &[h264::RefMod]) -> bool {
    let mut pic_num = curr;
    let mut index = 0;
    for modification in mods {
        match modification.idc() {
            0 | 1 => {
                let Ok(gap) = i32::try_from(modification.value()) else {
                    return false;
                };
                let Some(step) = gap.checked_add(1) else {
                    return false;
                };
                let next = if modification.idc() == 0 {
                    pic_num.checked_sub(step)
                } else {
                    pic_num.checked_add(step)
                };
                let Some(next) = next else {
                    return false;
                };
                pic_num = next.rem_euclid(max);
                let Some(slot) = list.iter().position(|entry| {
                    entry.long_term.is_none() && i32::from(entry.frame_num) == pic_num
                }) else {
                    return false;
                };
                let entry = list.remove(slot);
                list.insert(index.min(list.len()), entry);
                index += 1;
            }
            2 => {
                let Some(slot) = list
                    .iter()
                    .position(|entry| entry.long_term == Some(modification.value()))
                else {
                    return false;
                };
                let entry = list.remove(slot);
                list.insert(index.min(list.len()), entry);
                index += 1;
            }
            _ => return false,
        }
    }
    true
}

/// Type-0 picture order count from the LSB and previous reference.
fn poc_type0(
    sps: &h264::Sps,
    context: &H264Context,
    header: &h264::SliceHeader,
    is_idr: bool,
) -> Option<i32> {
    let max = 1_i64 << sps.pic_order_cnt_lsb_bits();
    let lsb = i64::from(header.pic_order_cnt_lsb());
    let prev_lsb = i64::from(context.prev_lsb);
    let msb = if is_idr || context.prev_mmco5 || context.first {
        0
    } else if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
        context.prev_msb + max
    } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
        context.prev_msb - max
    } else {
        context.prev_msb
    };
    let top = msb + lsb;
    let bottom = top + i64::from(header.delta_pic_order_cnt_bottom());
    i32::try_from(top.min(bottom)).ok()
}

/// Type-1 picture order count from frame-number cycles.
fn poc_type1(
    sps: &h264::Sps,
    context: &H264Context,
    header: &h264::SliceHeader,
    offset: i32,
    is_reference: bool,
) -> Option<i32> {
    let _ = context;
    let mut abs_frame_num = offset.checked_add(i32::from(header.frame_num()))?;
    if !is_reference && abs_frame_num > 0 {
        abs_frame_num -= 1;
    }
    let cycle = i32::from(sps.num_ref_frames_in_poc_cycle());
    let mut expected = 0_i32;
    if cycle > 0 {
        let full = abs_frame_num / cycle;
        let rest = usize::try_from(abs_frame_num % cycle).ok()?;
        let mut per_cycle = 0_i32;
        for offset in sps
            .offset_for_ref_frame()
            .iter()
            .take(usize::from(sps.num_ref_frames_in_poc_cycle()))
        {
            per_cycle = per_cycle.checked_add(*offset)?;
        }
        expected = full.checked_mul(per_cycle)?;
        for offset in sps.offset_for_ref_frame().iter().take(rest) {
            expected = expected.checked_add(*offset)?;
        }
    }
    let mut top = expected.checked_add(header.delta_pic_order_cnt()[0])?;
    if !is_reference {
        top = top.checked_add(sps.offset_for_non_ref_pic())?;
    }
    Some(top)
}

/// Type-2 picture order count from the offset plus frame number.
fn poc_type2(offset: i32, frame_num: u16, is_reference: bool) -> Option<i32> {
    let base = offset.checked_add(i32::from(frame_num))?;
    let doubled = base.checked_mul(2)?;
    if is_reference {
        Some(doubled)
    } else {
        doubled.checked_sub(1)
    }
}

/// Concatenated slice bytes with per-slice offsets.
fn join_slices(slices: &[h264::Slice]) -> (Vec<u8>, Vec<u32>) {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(slices.len());
    for slice in slices {
        offsets.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(slice.payload());
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
    poc_tid0: i32,
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
            poc_tid0: 0,
            suppress_rasl: false,
            waiting: false,
            seen_first: false,
        }
    }

    /// Instructions for one access unit.
    pub fn compile(&mut self, au: AccessUnit<h265::Nalu>) -> Vec<H265Instruction> {
        let mut instructions = Vec::new();
        let mut slices = Vec::new();
        for nalu in au.into_nalus() {
            match nalu.parsed() {
                h265::ParsedNalu::Vps(vps) => {
                    instructions.push(H265Instruction::Vps(Rc::clone(vps)));
                }
                h265::ParsedNalu::Sps(sps) => {
                    instructions.push(H265Instruction::Sps(Rc::clone(sps)));
                }
                h265::ParsedNalu::Pps(pps) => {
                    instructions.push(H265Instruction::Pps(Rc::clone(pps)));
                }
                h265::ParsedNalu::Slice(slice) => slices.push(slice.as_ref().clone()),
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
        let unit = first.nal().unit_type().id();
        if is_rasl(unit) && self.suppress_rasl {
            return None;
        }
        if self.waiting && !is_restart(unit) {
            return None;
        }
        let sps = Rc::clone(first.sps());
        let header = first.header();
        let new_sequence = is_idr(unit) || is_bla(unit) || (unit == 21 && !self.seen_first);
        let Some(poc) = self.picture_poc(header, unit, first.nal().temporal_id(), &sps) else {
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
        if header.slice_type() != h265::SliceType::I
            && before.is_empty()
            && after.is_empty()
            && lt.is_empty()
        {
            self.resync();
            return None;
        }
        let id = self.next_id.next();
        self.dpb.push(H265Entry { id, poc });
        if first.nal().unit_type().is_irap() {
            self.suppress_rasl = new_sequence;
        }
        self.seen_first = true;
        self.waiting = false;
        let l0 = if header.slice_type() == h265::SliceType::I {
            None
        } else {
            let mut union = Vec::new();
            union.extend(before.iter().copied());
            union.extend(after.iter().copied());
            union.extend(lt.iter().copied());
            Some(union)
        };
        let echo = RpsEcho {
            sps_flag: header.rps_sps_flag(),
            ref_count: header.rps_ref_count(),
            bits: header.rps_bits(),
            before: before.iter().map(RefPicture::id).collect(),
            after: after.iter().map(RefPicture::id).collect(),
            lt: lt.iter().map(RefPicture::id).collect(),
            is_irap: first.nal().unit_type().is_irap(),
            is_idr: is_idr(unit),
            vps_id: first.vps().id(),
        };
        let (bytes, slice_offsets) = join_h265_slices(slices);
        let picture = Picture {
            l0,
            l1: None,
            poc,
            frame_num: 0,
            sps_id: sps.id(),
            pps_id: first.pps().id(),
            bytes,
            slice_offsets,
            coding: CodingFlags {
                is_reference: is_reference(unit),
                is_intra: header.slice_type() == h265::SliceType::I,
                is_idr: is_idr(unit),
            },
            idr_pic_id: 0,
            id: Some(id),
            releases,
            drop_held: header.no_output_of_prior_pics() && first.nal().unit_type().is_irap(),
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
        let poc = if is_idr(unit) {
            0
        } else {
            let lsb = header.poc_lsb()?;
            poc_from_lsb(sps, self.poc_tid0, lsb, is_bla(unit))?
        };
        if temporal_id == 0 && !is_excluded(unit) {
            self.poc_tid0 = poc;
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
        for (delta, used) in header.rps().short() {
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
        let max = 1_i64 << sps.pic_order_cnt_lsb_bits();
        let mask = i32::try_from(max - 1).ok()?;
        let mut lt = Vec::new();
        for lts in header.rps().long() {
            let (entry_poc, id) = if lts.msb_cycle_present() {
                let lsb = i64::from(header.poc_lsb()?);
                let full = i64::from(lts.poc_lsb()) + i64::from(poc)
                    - i64::from(lts.msb_cycle()) * max
                    - lsb;
                let narrowed = i32::try_from(full).ok()?;
                (narrowed, self.find_exact(narrowed))
            } else if let Some(found) = self.find_lsb(i32::try_from(lts.poc_lsb()).ok()?, mask) {
                found
            } else {
                (i32::try_from(lts.poc_lsb()).ok()?, None)
            };
            let entry = RefPicture {
                id,
                frame_num: 0,
                poc: entry_poc,
                long_term: Some(0),
            };
            if lts.used() {
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
        self.poc_tid0 = 0;
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

/// True for IDR pictures.
fn is_idr(unit: u8) -> bool {
    unit == 19 || unit == 20
}

/// True for BLA pictures.
fn is_bla(unit: u8) -> bool {
    (16..=18).contains(&unit)
}

/// True for RASL pictures.
fn is_rasl(unit: u8) -> bool {
    unit == 8 || unit == 9
}

/// True for pictures restarting a resyncing context.
fn is_restart(unit: u8) -> bool {
    is_idr(unit) || is_bla(unit)
}

/// True for pictures usable as references.
fn is_reference(unit: u8) -> bool {
    unit >= 16 || unit % 2 == 1
}

/// True for pictures never anchoring the POC carry.
fn is_excluded(unit: u8) -> bool {
    matches!(unit, 0 | 2 | 4 | 6 | 7 | 8 | 9)
}

/// POC from the LSB against the temporal-0 anchor.
fn poc_from_lsb(sps: &h265::Sps, anchor: i32, lsb: u32, bla: bool) -> Option<i32> {
    let max = 1_i64 << sps.pic_order_cnt_lsb_bits();
    let anchor = i64::from(anchor);
    let lsb = i64::from(lsb);
    let prev_lsb = anchor.rem_euclid(max);
    let msb_base = anchor - prev_lsb;
    let mut msb = if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
        msb_base + max
    } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
        msb_base - max
    } else {
        msb_base
    };
    if bla {
        msb = 0;
    }
    i32::try_from(msb + lsb).ok()
}

/// Concatenated slice bytes with per-slice offsets.
fn join_h265_slices(slices: &[h265::Slice]) -> (Vec<u8>, Vec<u32>) {
    let mut bytes = Vec::new();
    let mut offsets = Vec::with_capacity(slices.len());
    for slice in slices {
        offsets.push(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
        bytes.extend_from_slice(&[0, 0, 1]);
        bytes.extend_from_slice(slice.payload());
    }
    (bytes, offsets)
}

#[cfg(test)]
mod tests {
    use super::{H264Context, H265Context, ReorderBuffer, SlotBook};
    use crate::parse::au::{AccessUnit, AuSplitter};
    use crate::parse::test_support::{hex, split};

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
        assert!(buffer.push(4, "a").is_empty());
        assert!(buffer.push(2, "b").is_empty());
        assert_eq!(buffer.push(6, "c"), vec!["b"]);
        assert_eq!(buffer.push(0, "d"), vec!["d"]);
        assert_eq!(buffer.flush(), vec!["a", "c"]);
    }

    #[test]
    fn reorder_drop_held_discards() {
        let mut buffer = ReorderBuffer::with_capacity(4);
        buffer.push(2, "a");
        buffer.drop_held();
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn poc_type2_fits_boundary_but_not_past() {
        assert_eq!(
            super::poc_type2(1_073_741_823, 0, true),
            Some(2_147_483_646)
        );
        assert_eq!(super::poc_type2(1_073_741_824, 0, true), None);
        assert_eq!(super::poc_type2(9, 1, false), Some(19));
    }

    /// Access units out of parsed H.264 `payloads`.
    fn units264(payloads: &[Vec<u8>]) -> Vec<AccessUnit<super::h264::Nalu>> {
        let mut parser = super::h264::H264Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            let nalu = super::h264::Nalu::new(parsed);
            if let Some(unit) = splitter.put(nalu) {
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
        let Some(super::H264Instruction::Picture(idr)) = first.into_iter().last() else {
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
        let Some(super::H264Instruction::Picture(picture)) = recovered.into_iter().last() else {
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
        assert!(p.releases().is_empty());
    }

    /// Single-slot 64x64 fixture SPS (`max_num_ref_frames` 1, type 2).
    const SPS1: &str = "6764000aacb421360220000003002000000781e244d400";
    /// Single-slot 64x64 fixture PPS.
    const PPS1: &str = "68ef0fcb";
    /// Single-slot 64x64 fixture IDR slice head.
    const IDR1: &str = "6588841dffbc35c57861ffe1d5931b630a8e06ea5eac31d954786d4359becf7fee26d3375d779cde1e1b686dfad19aa274aa15132fa314135310ed550d38020a7a493c9545c4608c385801ca225f0fa45d2a5c62a8f8d19a5f229c1a18111423ffc6c89ffaed6ae99a2556546bd7089cdfb3c059e15a8226737dbe6bf75af56e78df9aec3a94717b75f04f9d57aa4ab121dc49b1bcff6fa27e10498f5c8ee8ceedf7cb820c14d1710726daf53e0bde5bd7bc7002c669738687cadf4a142efe26bb1a8980996e65925cfa8d6a24ab3f66a198e5f44d77a44ebc634f41eedf268d837948303a5f1ed9f6a2f5a4138c353f0f0acfcdff731b35322f0e47d535d50bfe4680fc9c30a7ef51d6ba381cbe50b01a89e8c24d16baeb7765f2db0b4f69ac259cc56356b62ccd6f50077d4f64babcfc9b68195c5258d8ac175d9b460991ce4ad98a9dca7824ae06512fca561166b72e02571f9fcd8b0cef4d371e52e0e9bcdb5a9cde32d7b431b8fa8ea56106221129b06a39d68504f26cb674d897e6015a41e2960738f225b67ba24d42d87b4130cac70fe213e3de780395f85084da032e11383e4580a9b2be7d912d5bf0ba678b4665ea5f648f6a2d82575cab35be4f0dea19d577d133ba0d08c42291dd876e555d2a9e8c06f5d8355cef35967674e052500eee4d8c226e09c79a4f440abd68dd37020a28f483d707ba5552b79631b93d345cae6d79a64366399912bc895927df5eb23e16a48838491afb5c162f00b3de7c2bf1f5a0f5dd2d3e6407c9125b923279d9717a0b11f25e562259b4145edd1f8be937891cfae7417b9fec1111697bc38624d42b870461f560793c5956edfe5dda9e7c7b2b6c026182b7f1b43f57f3a4a4b3d9f5d62a799c7f37248b836fd378e8eebe56e9674700";
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
        let units = split(&[hex(SPS264)]);
        let mut parser = super::h264::H264Parser::new();
        let super::h264::ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
            panic!("want sps");
        };
        let cap = usize::from(sps.max_num_ref_frames());
        let max = 1_i32 << sps.frame_num_bits();
        let cap_i32 = i32::try_from(cap).expect("cap");
        assert!(cap >= 2 && max >= cap_i32 + 2);
        let mut context = H264Context::new();
        let curr = u16::try_from(cap).expect("cap") - 2;
        let mut frames = vec![
            u16::try_from(max - 2).expect("frame"),
            u16::try_from(max - 1).expect("frame"),
        ];
        frames.extend(0..curr);
        assert_eq!(frames.len(), cap);
        let mut wrapped = None;
        for frame in frames {
            let id = context.next_id.next();
            if frame == u16::try_from(max - 2).expect("frame") {
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
        assert!(buffer.push(56, "a").is_empty());
        assert!(buffer.push(58, "b").is_empty());
        assert_eq!(buffer.push_flushing_on_reset(true, 0, "c"), vec!["a", "b"]);
        assert!(buffer.push_flushing_on_reset(false, 2, "d").is_empty());
        assert_eq!(buffer.flush(), vec!["c", "d"]);
    }

    /// Access units out of parsed H.265 `payloads`.
    fn units265(payloads: &[Vec<u8>]) -> Vec<AccessUnit<super::h265::Nalu>> {
        let mut parser = super::h265::H265Parser::new();
        let mut splitter = AuSplitter::new();
        let mut units = Vec::new();
        for raw in split(payloads) {
            let parsed = parser.parse(&raw).expect("parse");
            let nalu = super::h265::Nalu::new(parsed);
            if let Some(unit) = splitter.put(nalu) {
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
        let Some(super::H265Instruction::Picture(idr)) = first.into_iter().last() else {
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
        let Some(super::H265Instruction::Picture(picture)) = recovered.into_iter().last() else {
            panic!("want picture");
        };
        assert!(picture.picture().resets_dpb());
    }
}
