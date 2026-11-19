//! H.264 reference context and decode instructions.

use std::rc::Rc;

use crate::au::AccessUnit;
use crate::dpb::{join_coded, CodingFlags, Picture, RefPicture, ReferenceId};
use crate::h264;

use rust_video_caps::dpb::PocTracker;

/// Optional reference lists of one picture, L0 then L1.
type RefLists = (Option<Vec<RefPicture>>, Option<Vec<RefPicture>>);

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

#[cfg(test)]
mod tests {
    use super::H264Context;
    use crate::au::{AccessUnit, AuSplitter};
    use crate::fixtures::{hex, split264};

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

    #[test]
    fn poc_type2_fits_boundary_but_not_past() {
        assert_eq!(
            crate::h264::poc_type2(1_073_741_823, 0, true),
            Some(2_147_483_646)
        );
        assert_eq!(crate::h264::poc_type2(1_073_741_824, 0, true), None);
        assert_eq!(crate::h264::poc_type2(9, 1, false), Some(19));
    }

    /// Access units out of parsed H.264 `payloads`.
    fn units264(payloads: &[Vec<u8>]) -> Vec<AccessUnit<crate::h264::ParsedNalu>> {
        let mut parser = crate::h264::H264Parser::new();
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
        let mut parser = crate::h264::H264Parser::new();
        let crate::h264::ParsedNalu::Sps(sps) = parser.parse(&units[0]).expect("sps") else {
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
}
