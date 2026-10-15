//! Frame-only decoded picture buffer with short slice submissions.
//!
//! The accelerator parses slice headers from the bitstream; reference
//! lists travel in the picture parameters only.

use crate::decode::H264Submission;
use crate::dxva::{
    PicEntryH264, PicParamsH264, QmatrixH264, SliceShortH264, BITS_CHROMA_420,
    BITS_CONSTRAINED_INTRA, BITS_FRAME_MBS_ONLY, BITS_INTRA_PIC, BITS_MBS_CONSECUTIVE,
    BITS_MIN_LUMA_BIPRED_8X8, BITS_REF_PIC, BITS_TRANSFORM_8X8, BITS_WEIGHTED_BIPRED_SHIFT,
    BITS_WEIGHTED_PRED,
};
use crate::error::D3d12Error;
use crate::h264::{Marking, Mmco, Picture, PocType, Pps, SlicePoc, Sps};

/// DPB slot count.
const SLOTS: usize = 16;

/// Stored picture.
#[derive(Clone, Debug)]
struct Occupant {
    /// Transmitted frame number.
    frame_num: u32,
    /// Wrapped frame number for short-term pictures.
    wrap: i32,
    /// Picture order count.
    poc: i32,
    /// Long-term reference.
    long_term: bool,
    /// Long-term index.
    long_term_idx: u8,
    /// Used for reference.
    reference: bool,
    /// Already output.
    displayed: bool,
}

/// Reference picture behind parameter construction.
#[derive(Clone, Copy)]
struct RefState {
    /// DPB slot.
    slot: usize,
    /// Transmitted frame number.
    frame_num: u32,
    /// Picture order count.
    poc: i32,
    /// Long-term reference.
    long_term: bool,
    /// Long-term index.
    long_term_idx: u8,
}

/// Decoded picture buffer for one sequence.
pub(crate) struct Dpb {
    /// Picture slots.
    slots: [Option<Occupant>; SLOTS],
    /// Maximum reference count.
    max_refs: u8,
    /// Frame number modulus.
    max_frame_num: u32,
    /// Maximum long-term index, -1 for none.
    max_long_idx: i32,
    /// Next transmitted frame number.
    expected_frame_num: Option<u32>,
    /// Previous reference POC MSB.
    prev_poc_msb: i64,
    /// Previous reference POC LSB.
    prev_poc_lsb: u32,
    /// Previous reference frame number.
    prev_ref_frame_num: u32,
    /// Previous frame number offset.
    prev_frame_num_offset: i64,
    /// True past the first reference picture.
    has_prev_ref: bool,
    /// Highest decoded POC.
    max_decoded_poc: Option<i32>,
    /// Driver-echoed picture serial.
    feedback: u32,
}

impl Dpb {
    /// Empty buffer for `sps`.
    pub(crate) fn new(sps: &Sps) -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            max_refs: sps.max_num_ref_frames,
            max_frame_num: sps.max_frame_num(),
            max_long_idx: -1,
            expected_frame_num: None,
            prev_poc_msb: 0,
            prev_poc_lsb: 0,
            prev_ref_frame_num: 0,
            prev_frame_num_offset: 0,
            has_prev_ref: false,
            max_decoded_poc: None,
            feedback: 1,
        }
    }

    /// Submission plus display-order outputs for `picture`.
    pub(crate) fn decode(
        &mut self,
        picture: &Picture,
        sps: &Sps,
        pps: &Pps,
    ) -> Result<Decoded, D3d12Error> {
        if picture.slices.is_empty() {
            return Err(D3d12Error::Parse("picture without slices".to_owned()));
        }
        let frame_num = picture.frame_num();
        if frame_num >= self.max_frame_num {
            return Err(D3d12Error::Parse("frame_num range".to_owned()));
        }
        self.check_gap(picture, sps)?;
        let mut emits: Vec<(i32, usize)> = Vec::with_capacity(SLOTS);
        let refs = self.ref_snapshot();
        if picture.idr {
            self.reset_idr(picture, &mut emits);
        } else {
            self.mark(picture, &mut emits)?;
        }
        let poc = self.picture_poc(picture, sps)?;
        let slot = self.alloc()?;
        let wrap = Self::current_wrap(picture, frame_num)?;
        let (long_term, long_term_idx) = Self::current_long_term(picture);
        self.slots[slot] = Some(Occupant {
            frame_num,
            wrap,
            poc,
            long_term,
            long_term_idx,
            reference: picture.ref_pic(),
            displayed: false,
        });
        self.close_picture(picture, frame_num, poc);
        self.bump(&mut emits);
        let submission = self.submit(picture, sps, pps, &refs, slot, poc)?;
        Ok(Decoded {
            submission,
            outputs: outputs_sorted(emits),
        })
    }

    /// Undisplayed pictures in display order.
    pub(crate) fn drain(&mut self) -> Vec<Output> {
        let mut emits: Vec<(i32, usize)> = Vec::with_capacity(SLOTS);
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(occupant) = slot {
                if !occupant.displayed {
                    emits.push((occupant.poc, index));
                    occupant.displayed = true;
                }
            }
        }
        self.free_displayed_non_refs();
        outputs_sorted(emits)
    }

    /// Expected transmitted frame number after `frame_num`.
    fn check_gap(&mut self, picture: &Picture, sps: &Sps) -> Result<(), D3d12Error> {
        if picture.idr {
            return Ok(());
        }
        let frame_num = picture.frame_num();
        if let Some(expected) = self.expected_frame_num {
            if frame_num != expected {
                if self.repeated_number(picture, frame_num, expected) {
                    return Ok(());
                }
                if sps.gaps_allowed {
                    return Err(D3d12Error::Unsupported("frame_num gap".to_owned()));
                }
                return Err(D3d12Error::Parse(
                    "frame_num gap with gaps forbidden".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// True when `frame_num` repeats the previous number without ambiguity.
    fn repeated_number(&self, picture: &Picture, frame_num: u32, expected: u32) -> bool {
        if frame_num != (expected + self.max_frame_num - 1) % self.max_frame_num {
            return false;
        }
        if !picture.ref_pic() {
            return true;
        }
        let Ok(wrap) = Self::current_wrap(picture, frame_num) else {
            return false;
        };
        !self
            .slots
            .iter()
            .flatten()
            .any(|occupant| occupant.reference && !occupant.long_term && occupant.wrap == wrap)
    }

    /// IDR reset: prior pictures out, state zeroed.
    fn reset_idr(&mut self, picture: &Picture, emits: &mut Vec<(i32, usize)>) {
        let no_output = picture
            .slices
            .iter()
            .any(|slice| match &slice.header.marking {
                Marking::Idr { no_output, .. } => *no_output,
                _ => false,
            });
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(occupant) = slot.as_mut() else {
                continue;
            };
            occupant.reference = false;
            occupant.long_term = false;
            if occupant.displayed {
                *slot = None;
            } else {
                occupant.displayed = true;
                if !no_output {
                    emits.push((occupant.poc, index));
                }
            }
        }
        self.max_long_idx = -1;
        self.prev_poc_msb = 0;
        self.prev_poc_lsb = 0;
        self.prev_ref_frame_num = 0;
        self.prev_frame_num_offset = 0;
        self.has_prev_ref = false;
        self.max_decoded_poc = None;
    }

    /// Sliding window plus adaptive marking for non-IDR pictures.
    fn mark(&mut self, picture: &Picture, emits: &mut Vec<(i32, usize)>) -> Result<(), D3d12Error> {
        let adaptive = picture
            .slices
            .iter()
            .find_map(|slice| match &slice.header.marking {
                Marking::Adaptive(operations) => Some(operations),
                _ => None,
            });
        match adaptive {
            Some(operations) => {
                for op in operations {
                    self.apply_mmco(*op, picture, emits)?;
                }
                Ok(())
            }
            None => self.slide(picture, emits),
        }
    }

    /// Sliding window eviction of the smallest short-term picture number.
    fn slide(
        &mut self,
        picture: &Picture,
        emits: &mut Vec<(i32, usize)>,
    ) -> Result<(), D3d12Error> {
        if !picture.ref_pic() {
            return Ok(());
        }
        let refs = self
            .slots
            .iter()
            .flatten()
            .filter(|slot| slot.reference)
            .count();
        if refs < usize::from(self.max_refs) {
            return Ok(());
        }
        if self.max_refs == 0 {
            return Err(D3d12Error::Parse(
                "reference with zero ref frames".to_owned(),
            ));
        }
        let mut victim: Option<usize> = None;
        let mut smallest = i32::MAX;
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(occupant) = slot {
                if occupant.reference && !occupant.long_term && occupant.wrap < smallest {
                    smallest = occupant.wrap;
                    victim = Some(index);
                }
            }
        }
        let Some(victim) = victim else {
            return Err(D3d12Error::BrokenReference(
                "no short-term to slide".to_owned(),
            ));
        };
        self.evict(victim, emits);
        Ok(())
    }

    /// One memory management operation.
    fn apply_mmco(
        &mut self,
        op: Mmco,
        picture: &Picture,
        emits: &mut Vec<(i32, usize)>,
    ) -> Result<(), D3d12Error> {
        match op {
            Mmco::ForgetShort(diff) => {
                let target = self.short_target(picture, diff)?;
                self.forget_where(
                    |occupant| !occupant.long_term && occupant.wrap == target,
                    emits,
                )?;
            }
            Mmco::ForgetLong(index) => {
                let index = index_u8(index)?;
                self.forget_where(
                    |occupant| occupant.long_term && occupant.long_term_idx == index,
                    emits,
                )?;
            }
            Mmco::ShortToLong(diff, index) => {
                let target = self.short_target(picture, diff)?;
                let index = index_u8(index)?;
                let _ = self.forget_where(
                    |occupant| occupant.long_term && occupant.long_term_idx == index,
                    emits,
                );
                let mut moved = false;
                for slot in self.slots.iter_mut().flatten() {
                    if slot.reference && !slot.long_term && slot.wrap == target {
                        slot.long_term = true;
                        slot.long_term_idx = index;
                        moved = true;
                    }
                }
                if !moved {
                    return Err(D3d12Error::BrokenReference(
                        "short-term target absent".to_owned(),
                    ));
                }
                if self.max_long_idx < i32::from(index) {
                    self.max_long_idx = i32::from(index);
                }
            }
            Mmco::SetMaxLong(plus1) => {
                let max = i32::try_from(plus1)
                    .map_err(|_| D3d12Error::Parse("max_long_term_frame_idx range".to_owned()))?
                    - 1;
                self.max_long_idx = max;
                let mut victims = Vec::new();
                for (index, slot) in self.slots.iter().enumerate() {
                    if let Some(occupant) = slot {
                        if occupant.long_term && i32::from(occupant.long_term_idx) > max {
                            victims.push(index);
                        }
                    }
                }
                for victim in victims {
                    self.evict(victim, emits);
                }
            }
            Mmco::MarkAllUnused => {
                self.max_long_idx = -1;
                for slot in self.slots.iter_mut().flatten() {
                    slot.reference = false;
                    slot.long_term = false;
                }
            }
            Mmco::CurrentToLong => {
                let _ = self.forget_where(
                    |occupant| occupant.long_term && occupant.long_term_idx == 0,
                    emits,
                );
                if self.max_long_idx < 0 {
                    self.max_long_idx = 0;
                }
            }
        }
        Ok(())
    }

    /// Short-term picture number named by a difference.
    fn short_target(&self, picture: &Picture, diff: u32) -> Result<i32, D3d12Error> {
        let max = i64::from(self.max_frame_num);
        let current = i64::from(picture.frame_num());
        let target = (current - i64::from(diff) - 1).rem_euclid(max);
        i32::try_from(target).map_err(|_| D3d12Error::Parse("pic_num range".to_owned()))
    }

    /// Forget references matching `matches`, emitting undisplayed victims.
    fn forget_where(
        &mut self,
        matches: impl Fn(&Occupant) -> bool,
        emits: &mut Vec<(i32, usize)>,
    ) -> Result<(), D3d12Error> {
        let mut victims = Vec::new();
        for (index, slot) in self.slots.iter().enumerate() {
            if slot
                .as_ref()
                .is_some_and(|occupant| occupant.reference && matches(occupant))
            {
                victims.push(index);
            }
        }
        if victims.is_empty() {
            return Err(D3d12Error::BrokenReference(
                "marking target absent".to_owned(),
            ));
        }
        for victim in victims {
            self.evict(victim, emits);
        }
        Ok(())
    }

    /// Slot contents out of the reference set, emitted when undisplayed.
    fn evict(&mut self, index: usize, emits: &mut Vec<(i32, usize)>) {
        let Some(occupant) = self.slots[index].as_mut() else {
            return;
        };
        occupant.reference = false;
        occupant.long_term = false;
        if occupant.displayed {
            self.slots[index] = None;
        } else {
            occupant.displayed = true;
            emits.push((occupant.poc, index));
        }
    }

    /// Free displayed pictures held without reference.
    fn free_displayed_non_refs(&mut self) {
        for slot in &mut self.slots {
            if slot
                .as_ref()
                .is_some_and(|occupant| occupant.displayed && !occupant.reference)
            {
                *slot = None;
            }
        }
    }

    /// First free slot.
    fn alloc(&self) -> Result<usize, D3d12Error> {
        self.slots
            .iter()
            .position(Option::is_none)
            .ok_or(D3d12Error::DpbFull)
    }

    /// Wrapped frame number for the current picture.
    fn current_wrap(picture: &Picture, frame_num: u32) -> Result<i32, D3d12Error> {
        if has_mmco5(picture) {
            return Ok(0);
        }
        i32::try_from(frame_num).map_err(|_| D3d12Error::Parse("frame_num range".to_owned()))
    }

    /// Long-term assignment for the current picture.
    fn current_long_term(picture: &Picture) -> (bool, u8) {
        for slice in &picture.slices {
            match &slice.header.marking {
                Marking::Idr {
                    long_term: true, ..
                } => return (true, 0),
                Marking::Adaptive(operations) if operations.contains(&Mmco::CurrentToLong) => {
                    return (true, 0)
                }
                _ => {}
            }
        }
        (false, 0)
    }

    /// Frame number and POC state after storing the current picture.
    fn close_picture(&mut self, picture: &Picture, frame_num: u32, poc: i32) {
        self.expected_frame_num = Some((frame_num + 1) % self.max_frame_num);
        if has_mmco5(picture) {
            self.prev_poc_msb = 0;
            self.prev_poc_lsb = 0;
            self.prev_ref_frame_num = 0;
            self.prev_frame_num_offset = 0;
            self.has_prev_ref = true;
        }
        self.max_decoded_poc = Some(self.max_decoded_poc.map_or(poc, |max| max.max(poc)));
    }

    /// Picture order count for `picture`.
    fn picture_poc(&mut self, picture: &Picture, sps: &Sps) -> Result<i32, D3d12Error> {
        if has_mmco5(picture) {
            return Ok(0);
        }
        let first = &picture.slices[0].header;
        match (sps.poc_type, &first.poc) {
            (PocType::Lsb, SlicePoc::Lsb(lsb)) => self.poc_lsb(*lsb, sps, picture.ref_pic()),
            (PocType::Delta, SlicePoc::Deltas(first_delta, second)) => {
                self.poc_delta(picture, sps, *first_delta, *second)
            }
            (PocType::FrameNum, SlicePoc::None) => self.poc_frame_num(picture),
            _ => Err(D3d12Error::Parse(
                "poc syntax against sequence method".to_owned(),
            )),
        }
    }

    /// Type 0 POC from the slice LSB.
    fn poc_lsb(&mut self, lsb: u32, sps: &Sps, ref_pic: bool) -> Result<i32, D3d12Error> {
        let max_lsb = i64::from(sps.max_poc_lsb());
        let msb = self.poc_msb(i64::from(lsb), max_lsb);
        let poc = msb + i64::from(lsb);
        let poc =
            i32::try_from(poc).map_err(|_| D3d12Error::Parse("pic_order_cnt range".to_owned()))?;
        if ref_pic {
            self.prev_poc_msb = msb;
            self.prev_poc_lsb = lsb;
            self.has_prev_ref = true;
        }
        Ok(poc)
    }

    /// Type 0 MSB against the previous reference LSB.
    fn poc_msb(&self, wide: i64, max_lsb: i64) -> i64 {
        if !self.has_prev_ref {
            return 0;
        }
        let half = max_lsb / 2;
        let previous = i64::from(self.prev_poc_lsb);
        if wide < previous && previous - wide >= half {
            self.prev_poc_msb + max_lsb
        } else if wide > previous && wide - previous > half {
            self.prev_poc_msb - max_lsb
        } else {
            self.prev_poc_msb
        }
    }

    /// Type 1 POC from slice deltas.
    fn poc_delta(
        &mut self,
        picture: &Picture,
        sps: &Sps,
        first: i32,
        second: Option<i32>,
    ) -> Result<i32, D3d12Error> {
        let frame_num = picture.frame_num();
        let offset = self.frame_num_offset(frame_num, picture.idr);
        let abs_frame_num = offset + i64::from(frame_num);
        let expected = Self::expected_poc(abs_frame_num, sps);
        let top = expected + i64::from(first);
        let bottom =
            expected + i64::from(sps.offset_top_bottom) + i64::from(second.unwrap_or(first));
        let mut poc = top.min(bottom);
        if !picture.ref_pic() {
            poc += i64::from(sps.offset_non_ref);
        }
        let poc =
            i32::try_from(poc).map_err(|_| D3d12Error::Parse("pic_order_cnt range".to_owned()))?;
        if picture.ref_pic() {
            self.prev_ref_frame_num = frame_num;
            self.prev_frame_num_offset = offset;
            self.has_prev_ref = true;
        }
        Ok(poc)
    }

    /// Expected POC for an absolute frame number.
    fn expected_poc(abs_frame_num: i64, sps: &Sps) -> i64 {
        if abs_frame_num <= 0 || sps.cycle == 0 {
            return 0;
        }
        let cycle = i64::from(sps.cycle);
        let cycle_count = (abs_frame_num - 1) / cycle;
        let in_cycle = (abs_frame_num - 1) % cycle;
        let mut expected = cycle_count * i64::from(sps.expected_delta);
        for offset in sps
            .cycle_offsets
            .iter()
            .take(usize::try_from(in_cycle).unwrap_or(0))
        {
            expected += i64::from(*offset);
        }
        expected
    }

    /// Type 2 POC from the frame number.
    fn poc_frame_num(&mut self, picture: &Picture) -> Result<i32, D3d12Error> {
        let frame_num = picture.frame_num();
        let offset = self.frame_num_offset(frame_num, picture.idr);
        let poc = 2 * (offset + i64::from(frame_num));
        let poc =
            i32::try_from(poc).map_err(|_| D3d12Error::Parse("pic_order_cnt range".to_owned()))?;
        if picture.ref_pic() {
            self.prev_ref_frame_num = frame_num;
            self.prev_frame_num_offset = offset;
            self.has_prev_ref = true;
        }
        Ok(poc)
    }

    /// Frame number offset with wraparound tracking.
    fn frame_num_offset(&self, frame_num: u32, idr: bool) -> i64 {
        if idr || !self.has_prev_ref {
            return 0;
        }
        if frame_num < self.prev_ref_frame_num {
            self.prev_frame_num_offset + i64::from(self.max_frame_num)
        } else {
            self.prev_frame_num_offset
        }
    }

    /// Undisplayed pictures below the decoded maximum, in POC order.
    fn bump(&mut self, emits: &mut Vec<(i32, usize)>) {
        let Some(max) = self.max_decoded_poc else {
            return;
        };
        loop {
            let mut next: Option<(i32, usize)> = None;
            for (index, slot) in self.slots.iter().enumerate() {
                if let Some(occupant) = slot {
                    if !occupant.displayed && occupant.poc < max {
                        let candidate = (occupant.poc, index);
                        if next.is_none_or(|best| candidate < best) {
                            next = Some(candidate);
                        }
                    }
                }
            }
            let Some((poc, index)) = next else {
                break;
            };
            emits.push((poc, index));
            if let Some(occupant) = self.slots[index].as_mut() {
                occupant.displayed = true;
            }
        }
        self.free_displayed_non_refs();
    }

    /// Reference pictures behind list construction, before marking.
    fn ref_snapshot(&self) -> Vec<RefState> {
        let mut refs = Vec::with_capacity(SLOTS);
        for (slot, occupant) in self.slots.iter().enumerate() {
            if let Some(occupant) = occupant {
                if occupant.reference {
                    refs.push(RefState {
                        slot,
                        frame_num: occupant.frame_num,
                        poc: occupant.poc,
                        long_term: occupant.long_term,
                        long_term_idx: occupant.long_term_idx,
                    });
                }
            }
        }
        refs
    }

    /// Submission payload for the stored picture.
    fn submit(
        &mut self,
        picture: &Picture,
        sps: &Sps,
        pps: &Pps,
        refs: &[RefState],
        slot: usize,
        poc: i32,
    ) -> Result<H264Submission, D3d12Error> {
        let mut pp_refs: Vec<RefState> = Vec::with_capacity(refs.len());
        pp_refs.extend(refs.iter().copied().filter(|state| !state.long_term));
        pp_refs.extend(refs.iter().copied().filter(|state| state.long_term));
        let mut params = Self::fill_params(picture, sps, pps, slot, poc, &pp_refs)?;
        let qmatrix = QmatrixH264 {
            lists_4x4: pps.scaling.four,
            lists_8x8: pps.scaling.eight,
        };
        let nals: Vec<&[u8]> = picture
            .slices
            .iter()
            .map(|slice| slice.nal.as_slice())
            .collect();
        let (spans, bitstream) = crate::decode::start_coded_bitstream(&nals)?;
        let slices = spans
            .into_iter()
            .map(|(location, bytes)| SliceShortH264 {
                location,
                bytes,
                chopping: 0,
            })
            .collect();
        let feedback = self.feedback;
        self.feedback = self.feedback.wrapping_add(1);
        params.status_report_feedback_number = feedback;
        Ok(H264Submission {
            params,
            qmatrix,
            slices,
            bitstream,
            slot: slot_u8(slot)?,
        })
    }

    /// Picture parameters for the stored picture.
    fn fill_params(
        picture: &Picture,
        sps: &Sps,
        pps: &Pps,
        slot: usize,
        poc: i32,
        pp_refs: &[RefState],
    ) -> Result<PicParamsH264, D3d12Error> {
        let mut params = PicParamsH264::empty();
        params.frame_width_in_mbs_minus1 = sps.width_in_mbs_minus1;
        params.frame_height_in_mbs_minus1 = sps.height_in_mbs_minus1;
        params.curr_pic = PicEntryH264::frame(slot_u8(slot)?);
        params.num_ref_frames = sps.max_num_ref_frames;
        params.bit_fields = Self::bit_fields(picture, sps, pps);
        params.reserved_16_a = 3;
        params.curr_field_order_cnt = [poc, poc];
        params.pic_init_qs_minus26 = pps.pic_init_qs;
        params.chroma_qp_index_offset = pps.chroma_qp;
        params.second_chroma_qp_index_offset = pps.second_chroma_qp;
        params.continuation_flag = 1;
        params.pic_init_qp_minus26 = pps.pic_init_qp;
        params.num_ref_idx_l0_active_minus1 = pps.num_ref_l0;
        params.num_ref_idx_l1_active_minus1 = pps.num_ref_l1;
        params.frame_num = u16::try_from(picture.frame_num())
            .map_err(|_| D3d12Error::Parse("frame_num range".to_owned()))?;
        params.log2_max_frame_num_minus4 = sps.log2_max_frame_num_minus4;
        params.pic_order_cnt_type = sps.poc_type.id();
        params.log2_max_pic_order_cnt_lsb_minus4 = sps.log2_max_poc_lsb_minus4;
        params.delta_pic_order_always_zero_flag = u8::from(sps.delta_zero);
        params.direct_8x8_inference_flag = u8::from(sps.direct_8x8);
        params.entropy_coding_mode_flag = u8::from(pps.entropy());
        params.pic_order_present_flag = u8::from(pps.pic_order_present());
        params.deblocking_filter_control_present_flag = u8::from(pps.deblocking_present());
        params.redundant_pic_cnt_present_flag = u8::from(pps.redundant_present());
        Self::fill_ref_lists(&mut params, pp_refs)?;
        Ok(params)
    }

    /// Packed picture bit fields.
    fn bit_fields(picture: &Picture, sps: &Sps, pps: &Pps) -> u16 {
        let mut fields = BITS_CHROMA_420 | BITS_FRAME_MBS_ONLY;
        if picture.ref_pic() {
            fields |= BITS_REF_PIC;
        }
        if pps.constrained_intra() {
            fields |= BITS_CONSTRAINED_INTRA;
        }
        if pps.weighted_pred() {
            fields |= BITS_WEIGHTED_PRED;
        }
        fields |= u16::from(pps.weighted_bipred()) << BITS_WEIGHTED_BIPRED_SHIFT;
        fields |= BITS_MBS_CONSECUTIVE;
        if pps.transform_8x8() {
            fields |= BITS_TRANSFORM_8X8;
        }
        if sps.level_idc >= 31 {
            fields |= BITS_MIN_LUMA_BIPRED_8X8;
        }
        if picture.intra() {
            fields |= BITS_INTRA_PIC;
        }
        fields
    }

    /// Reference triples compacted from the pre-marking snapshot.
    fn fill_ref_lists(params: &mut PicParamsH264, pp_refs: &[RefState]) -> Result<(), D3d12Error> {
        for (index, state) in pp_refs.iter().enumerate().take(16) {
            params.ref_frame_list[index] = PicEntryH264::new(slot_u8(state.slot)?, state.long_term);
            params.field_order_cnt_list[index] = [state.poc, state.poc];
            params.frame_num_list[index] = if state.long_term {
                u16::from(state.long_term_idx)
            } else {
                u16::try_from(state.frame_num)
                    .map_err(|_| D3d12Error::Parse("frame_num range".to_owned()))?
            };
            params.used_for_reference_flags |= 0b11 << (2 * index);
        }
        Ok(())
    }
}

/// True when any slice carries memory management operation 5.
fn has_mmco5(picture: &Picture) -> bool {
    picture
        .slices
        .iter()
        .any(|slice| match &slice.header.marking {
            Marking::Adaptive(operations) => operations.contains(&Mmco::MarkAllUnused),
            _ => false,
        })
}

/// Long-term index as a byte.
fn index_u8(index: u32) -> Result<u8, D3d12Error> {
    u8::try_from(index).map_err(|_| D3d12Error::Parse("long_term_frame_idx range".to_owned()))
}

/// Slot number as a byte.
fn slot_u8(slot: usize) -> Result<u8, D3d12Error> {
    u8::try_from(slot).map_err(|_| D3d12Error::Parse("slot range".to_owned()))
}

/// Outputs for `emits` in display order.
fn outputs_sorted(mut emits: Vec<(i32, usize)>) -> Vec<Output> {
    emits.sort_unstable();
    emits
        .into_iter()
        .filter_map(|(_, slot)| slot_u8(slot).ok().map(|slot| Output { slot }))
        .collect()
}

/// Picture ready for display from `slot`.
#[derive(Debug)]
pub(crate) struct Output {
    /// Output surface slot.
    slot: u8,
}

impl Output {
    /// Output surface slot.
    pub(crate) const fn slot(&self) -> u8 {
        self.slot
    }
}

/// Submission plus display-order outputs.
#[derive(Debug)]
pub(crate) struct Decoded {
    /// Driver submission.
    pub(crate) submission: H264Submission,
    /// Display-order outputs.
    pub(crate) outputs: Vec<Output>,
}

#[cfg(test)]
mod tests {
    use super::{Dpb, Output};
    use crate::dxva::{
        BITS_CHROMA_420, BITS_FRAME_MBS_ONLY, BITS_MBS_CONSECUTIVE, BITS_MIN_LUMA_BIPRED_8X8,
        BITS_REF_PIC,
    };
    use crate::h264::{
        Marking, Mmco, PendingSlice, Picture, PocType, Pps, ScalingSets, SliceHeader, SlicePoc,
        SliceType, Sps, Weights, PPS_DEBLOCKING,
    };

    /// Sequence set with type 0 POC and four references.
    fn sps() -> Sps {
        Sps {
            level_idc: 30,
            seq_parameter_set_id: 0,
            scaling: ScalingSets {
                four: [[16; 16]; 6],
                eight: [[16; 64]; 2],
            },
            log2_max_frame_num_minus4: 0,
            poc_type: PocType::Lsb,
            log2_max_poc_lsb_minus4: 0,
            delta_zero: false,
            offset_non_ref: 0,
            offset_top_bottom: 0,
            cycle: 0,
            cycle_offsets: [0; 255],
            expected_delta: 0,
            max_num_ref_frames: 4,
            gaps_allowed: false,
            width_in_mbs_minus1: 119,
            height_in_mbs_minus1: 67,
            direct_8x8: true,
            crop: None,
        }
    }

    /// Picture set with deblocking control.
    fn pps() -> Pps {
        Pps {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            flags: PPS_DEBLOCKING,
            num_ref_l0: 0,
            num_ref_l1: 0,
            pic_init_qp: 0,
            pic_init_qs: 0,
            chroma_qp: 0,
            second_chroma_qp: 0,
            scaling: ScalingSets {
                four: [[7; 16]; 6],
                eight: [[9; 64]; 2],
            },
        }
    }

    /// Single-slice picture.
    fn picture(
        idr: bool,
        ref_idc: u8,
        frame_num: u32,
        poc: SlicePoc,
        marking: Marking,
        slice_type: SliceType,
    ) -> Picture {
        Picture {
            slices: vec![PendingSlice {
                header: SliceHeader {
                    first_mb: 0,
                    slice_type,
                    fixed: false,
                    pps_id: 0,
                    frame_num,
                    idr_pic_id: if idr { Some(0) } else { None },
                    poc,
                    direct_spatial: false,
                    ref_override: None,
                    ref_mod_l0: Vec::new(),
                    ref_mod_l1: Vec::new(),
                    weights: Weights::default(),
                    marking,
                    cabac_init_idc: 0,
                    qp_delta: 0,
                    deblocking_idc: 0,
                    deblocking_alpha: 0,
                    deblocking_beta: 0,
                    bit_offset: 0,
                },
                nal: vec![0x65, 0x88],
                ref_idc,
                idr,
            }],
            idr,
        }
    }

    /// IDR picture with `frame_num` and `lsb`.
    fn idr(frame_num: u32, lsb: u32) -> Picture {
        picture(
            true,
            2,
            frame_num,
            SlicePoc::Lsb(lsb),
            Marking::Idr {
                no_output: false,
                long_term: false,
            },
            SliceType::I,
        )
    }

    /// P picture with `frame_num` and `lsb`.
    fn p(frame_num: u32, lsb: u32) -> Picture {
        picture(
            false,
            2,
            frame_num,
            SlicePoc::Lsb(lsb),
            Marking::Silent,
            SliceType::P,
        )
    }

    /// Output slots in order.
    fn slots(outputs: &[Output]) -> Vec<u8> {
        outputs.iter().map(|output| output.slot).collect()
    }

    #[test]
    fn idr_occupies_slot_zero() {
        let mut dpb = Dpb::new(&sps());
        let decoded = dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        assert!(decoded.outputs.is_empty());
        assert_eq!(decoded.submission.slot, 0);
        let params = &decoded.submission.params;
        assert_eq!(params.frame_num, 0);
        assert_eq!(params.curr_pic.entry, 0);
        assert_eq!(params.curr_field_order_cnt, [0, 0]);
        assert_eq!(params.used_for_reference_flags, 0);
        assert!(params
            .ref_frame_list
            .iter()
            .all(|entry| entry.entry == 0xFF));
        assert_eq!(params.status_report_feedback_number, 1);
    }

    #[test]
    fn p_references_idr() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(1, 2), &sps(), &pps()).expect("p");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        assert_eq!(decoded.submission.slot, 1);
        let params = &decoded.submission.params;
        assert_eq!(params.curr_pic.entry, 1);
        assert_eq!(params.ref_frame_list[0].entry, 0);
        assert_eq!(params.field_order_cnt_list[0], [0, 0]);
        assert_eq!(params.frame_num_list[0], 0);
        assert_eq!(params.used_for_reference_flags, 0b11);
        assert_eq!(params.curr_field_order_cnt, [2, 2]);
        assert_eq!(
            params.bit_fields,
            BITS_CHROMA_420 | BITS_FRAME_MBS_ONLY | BITS_REF_PIC | BITS_MBS_CONSECUTIVE
        );
    }

    #[test]
    fn b_displays_before_p() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        dpb.decode(&p(1, 4), &sps(), &pps()).expect("p");
        let b = picture(false, 0, 2, SlicePoc::Lsb(2), Marking::Silent, SliceType::B);
        let decoded = dpb.decode(&b, &sps(), &pps()).expect("b");
        assert_eq!(slots(&decoded.outputs), vec![2]);
        assert_eq!(slots(&dpb.drain()), vec![1]);
    }

    #[test]
    fn mmco5_resets_poc() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        dpb.decode(&p(1, 2), &sps(), &pps()).expect("p1");
        let reset = picture(
            false,
            2,
            2,
            SlicePoc::Lsb(4),
            Marking::Adaptive(vec![Mmco::MarkAllUnused]),
            SliceType::P,
        );
        let decoded = dpb.decode(&reset, &sps(), &pps()).expect("mmco5");
        assert_eq!(decoded.submission.params.curr_field_order_cnt, [0, 0]);
        assert_eq!(slots(&decoded.outputs), vec![2]);
        let decoded = dpb.decode(&p(3, 4), &sps(), &pps()).expect("p3");
        assert_eq!(decoded.submission.params.curr_field_order_cnt, [4, 4]);
        assert_eq!(decoded.submission.slot, 0);
        assert_eq!(slots(&decoded.outputs), vec![1]);
        assert_eq!(slots(&dpb.drain()), vec![0]);
    }

    #[test]
    fn gap_refused() {
        let allowed = Sps {
            gaps_allowed: true,
            ..sps()
        };
        let mut dpb = Dpb::new(&allowed);
        dpb.decode(&idr(0, 0), &allowed, &pps()).expect("idr");
        let error = dpb.decode(&p(2, 2), &allowed, &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::Unsupported(_)));

        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        let error = dpb.decode(&p(2, 2), &sps(), &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::Parse(_)));
    }

    #[test]
    fn sliding_window_keeps_evicted_surface() {
        let mut sequence = sps();
        sequence.max_num_ref_frames = 1;
        let mut dpb = Dpb::new(&sequence);
        dpb.decode(&idr(0, 0), &sequence, &pps()).expect("idr");
        let decoded = dpb.decode(&p(1, 2), &sequence, &pps()).expect("p");
        assert_eq!(decoded.submission.slot, 1);
        assert_eq!(slots(&decoded.outputs), vec![0]);
        assert_eq!(decoded.submission.params.ref_frame_list[0].entry, 0);
        assert_eq!(decoded.submission.params.used_for_reference_flags, 0b11);
    }

    #[test]
    fn submission_carries_bitstream_and_matrices() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(1, 2), &sps(), &pps()).expect("p");
        let submission = &decoded.submission;
        assert_eq!(submission.bitstream.len(), 128);
        assert_eq!(submission.bitstream[..5], [0, 0, 1, 0x65, 0x88]);
        assert!(submission.bitstream[5..].iter().all(|byte| *byte == 0));
        assert_eq!(submission.slices.len(), 1);
        let slice = &submission.slices[0];
        assert_eq!(slice.location, 0);
        assert_eq!(slice.bytes, 128);
        assert_eq!(slice.chopping, 0);
        assert_eq!(submission.qmatrix.lists_4x4, [[7; 16]; 6]);
        assert_eq!(submission.qmatrix.lists_8x8, [[9; 64]; 2]);
        assert_eq!(submission.params.frame_width_in_mbs_minus1, 119);
        assert_eq!(submission.params.frame_height_in_mbs_minus1, 67);
        assert_eq!(submission.params.status_report_feedback_number, 2);
    }

    #[test]
    fn drain_breaks_poc_ties_by_slot() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        for frame_num in 1..=2 {
            let non_ref = picture(
                false,
                0,
                frame_num,
                SlicePoc::Lsb(2),
                Marking::Silent,
                SliceType::P,
            );
            dpb.decode(&non_ref, &sps(), &pps()).expect("non-ref");
        }
        assert_eq!(slots(&dpb.drain()), vec![1, 2]);
    }

    #[test]
    fn dpb_full_without_output() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        for frame_num in 1..16 {
            let non_ref = picture(
                false,
                0,
                frame_num,
                SlicePoc::Lsb(0),
                Marking::Silent,
                SliceType::P,
            );
            dpb.decode(&non_ref, &sps(), &pps()).expect("fills");
        }
        let wrapped = picture(false, 0, 0, SlicePoc::Lsb(0), Marking::Silent, SliceType::P);
        let error = dpb.decode(&wrapped, &sps(), &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::DpbFull));
    }

    #[test]
    fn rejects_empty_pictures() {
        let mut dpb = Dpb::new(&sps());
        let picture = Picture {
            slices: Vec::new(),
            idr: true,
        };
        assert!(dpb.decode(&picture, &sps(), &pps()).is_err());
    }

    #[test]
    fn poc_type2_doubles_frame_num() {
        let mut sequence = sps();
        sequence.poc_type = PocType::FrameNum;
        let mut dpb = Dpb::new(&sequence);
        let idr = picture(
            true,
            2,
            0,
            SlicePoc::None,
            Marking::Idr {
                no_output: false,
                long_term: false,
            },
            SliceType::I,
        );
        dpb.decode(&idr, &sequence, &pps()).expect("idr");
        let p = picture(false, 2, 1, SlicePoc::None, Marking::Silent, SliceType::P);
        let decoded = dpb.decode(&p, &sequence, &pps()).expect("p");
        assert_eq!(decoded.submission.params.curr_field_order_cnt, [2, 2]);
        assert_eq!(slots(&decoded.outputs), vec![0]);
    }

    #[test]
    fn poc_type1_follows_cycle() {
        let mut sequence = sps();
        sequence.poc_type = PocType::Delta;
        sequence.cycle = 1;
        sequence.cycle_offsets[0] = 2;
        sequence.expected_delta = 2;
        let mut dpb = Dpb::new(&sequence);
        let idr = picture(
            true,
            2,
            0,
            SlicePoc::Deltas(0, None),
            Marking::Idr {
                no_output: false,
                long_term: false,
            },
            SliceType::I,
        );
        dpb.decode(&idr, &sequence, &pps()).expect("idr");
        for (frame_num, poc) in [(1, 2), (2, 4)] {
            let p = picture(
                false,
                2,
                frame_num,
                SlicePoc::Deltas(2, None),
                Marking::Silent,
                SliceType::P,
            );
            let decoded = dpb.decode(&p, &sequence, &pps()).expect("p");
            assert_eq!(decoded.submission.params.curr_field_order_cnt, [poc, poc]);
        }
    }

    #[test]
    fn frame_num_wraps_without_gap() {
        let mut dpb = Dpb::new(&sps());
        let frames = [(14, 0), (15, 2), (0, 4), (1, 6)];
        for (index, (frame_num, lsb)) in frames.iter().enumerate() {
            let picture = if index == 0 {
                picture(
                    true,
                    2,
                    *frame_num,
                    SlicePoc::Lsb(*lsb),
                    Marking::Idr {
                        no_output: false,
                        long_term: false,
                    },
                    SliceType::I,
                )
            } else {
                p(*frame_num, *lsb)
            };
            dpb.decode(&picture, &sps(), &pps()).expect("wraps");
        }
    }

    #[test]
    fn idr_no_output_discards() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        dpb.decode(&p(1, 4), &sps(), &pps()).expect("p");
        let second = picture(
            true,
            2,
            2,
            SlicePoc::Lsb(0),
            Marking::Idr {
                no_output: true,
                long_term: false,
            },
            SliceType::I,
        );
        let decoded = dpb.decode(&second, &sps(), &pps()).expect("idr2");
        assert!(decoded.outputs.is_empty());
        assert_eq!(decoded.submission.slot, 0);
    }

    #[test]
    fn min_luma_flag_follows_level() {
        let high = Sps {
            level_idc: 41,
            ..sps()
        };
        let mut dpb = Dpb::new(&high);
        dpb.decode(&idr(0, 0), &high, &pps()).expect("idr");
        let decoded = dpb.decode(&p(1, 2), &high, &pps()).expect("p");
        assert_eq!(
            decoded.submission.params.bit_fields,
            BITS_CHROMA_420
                | BITS_FRAME_MBS_ONLY
                | BITS_REF_PIC
                | BITS_MBS_CONSECUTIVE
                | BITS_MIN_LUMA_BIPRED_8X8
        );
    }

    #[test]
    fn long_term_names_index() {
        let mut dpb = Dpb::new(&sps());
        dpb.decode(&idr(0, 0), &sps(), &pps()).expect("idr");
        let promote = picture(
            false,
            2,
            1,
            SlicePoc::Lsb(2),
            Marking::Adaptive(vec![Mmco::CurrentToLong]),
            SliceType::P,
        );
        dpb.decode(&promote, &sps(), &pps()).expect("promote");
        let decoded = dpb.decode(&p(2, 4), &sps(), &pps()).expect("p");
        let params = &decoded.submission.params;
        assert_eq!(params.ref_frame_list[1].entry, 0x81);
        assert_eq!(params.frame_num_list[1], 0);
    }
}
