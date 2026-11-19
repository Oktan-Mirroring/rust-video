//! HEVC decoded picture buffer with DXVA submission payloads.

use std::sync::Arc;

use crate::dpb::Output;
use crate::dxva::{
    PicEntryHevc, PicParamsHevc, QmatrixHevc, SliceShortHevc, HEVC_FORMAT_CHROMA_420,
    HEVC_FORMAT_POC_SHIFT, HEVC_PIC_CHROMA_OFFSETS, HEVC_PIC_CONSTRAINED_INTRA,
    HEVC_PIC_CU_QP_DELTA, HEVC_PIC_DEBLOCK_DISABLED, HEVC_PIC_DEBLOCK_OVERRIDE, HEVC_PIC_IDR,
    HEVC_PIC_INTRA, HEVC_PIC_IRAP, HEVC_PIC_LISTS_MOD, HEVC_PIC_LOOP_ACROSS_SLICES,
    HEVC_PIC_LOOP_ACROSS_TILES, HEVC_PIC_SLICE_EXT, HEVC_PIC_SYNC, HEVC_PIC_TILES,
    HEVC_PIC_TRANSFORM_SKIP, HEVC_PIC_TRANSQUANT_BYPASS, HEVC_PIC_UNIFORM_SPACING,
    HEVC_PIC_WEIGHTED_BIPRED, HEVC_PIC_WEIGHTED_PRED, HEVC_TOOL_AMP, HEVC_TOOL_CABAC_INIT,
    HEVC_TOOL_DEPENDENT_SLICES, HEVC_TOOL_EXTRA_BITS_SHIFT, HEVC_TOOL_LONG_TERM,
    HEVC_TOOL_OUTPUT_FLAG, HEVC_TOOL_PCM, HEVC_TOOL_PCM_CHROMA_SHIFT, HEVC_TOOL_PCM_DIFF_SHIFT,
    HEVC_TOOL_PCM_LOOP_DISABLED, HEVC_TOOL_PCM_LUMA_SHIFT, HEVC_TOOL_PCM_MIN_SHIFT, HEVC_TOOL_SAO,
    HEVC_TOOL_SCALING, HEVC_TOOL_SIGN_HIDING, HEVC_TOOL_SMOOTHING, HEVC_TOOL_TEMPORAL_MVP,
};
use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason};
use crate::h265::{LtSpec, Picture, Pps, ShortTermRps, Sps};
use crate::pins::SlotPins;

use rust_video_caps::dpb::{DpbBookkeeping, PocTracker, SlotAllocFailure, SlotState};

/// DPB slot count.
const SLOTS: usize = 16;

/// Reference list length behind the picture parameters.
const REF_LIST_LEN: usize = 15;

/// Stored picture.
#[derive(Clone, Debug)]
struct Occupant {
    /// Picture order count.
    poc: i32,
    /// Long-term reference.
    long_term: bool,
    /// Used for reference.
    reference: bool,
    /// Already output or never needed for output.
    displayed: bool,
}

impl SlotState for Occupant {
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
        self.long_term = false;
    }
}

/// Reference picture behind parameter construction.
#[derive(Clone, Copy)]
struct RefState {
    /// DPB slot.
    slot: usize,
    /// Picture order count.
    poc: i32,
    /// Long-term reference.
    long_term: bool,
}

/// Decoded picture buffer for one sequence.
pub(crate) struct HevcDpb {
    /// Picture slots.
    slots: [Option<Occupant>; SLOTS],
    /// Pins held by live surfaces.
    pins: Arc<SlotPins>,
    /// Picture capacity.
    capacity: u8,
    /// Reorder limit.
    max_reorder: u8,
    /// Maximum POC LSB.
    max_poc_lsb: u32,
    /// Previous temporal-zero POC behind MSB derivation.
    poc: PocTracker,
    /// True past the first temporal-zero picture.
    has_prev_tid0: bool,
    /// Driver-echoed picture serial.
    feedback: u32,
}

impl HevcDpb {
    /// Empty buffer for `sps`.
    pub(crate) fn new(sps: &Sps) -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            pins: Arc::new(SlotPins::new()),
            capacity: sps.max_dec_pic_buffering_minus1 + 1,
            max_reorder: sps.max_num_reorder_pics,
            max_poc_lsb: sps.max_poc_lsb(),
            poc: PocTracker::new(),
            has_prev_tid0: false,
            feedback: 1,
        }
    }

    /// Pins shared with live surfaces.
    #[must_use]
    pub(crate) fn pins(&self) -> &Arc<SlotPins> {
        &self.pins
    }

    /// Submission plus display-order outputs for `picture`.
    pub(crate) fn decode(
        &mut self,
        picture: &Picture,
        sps: &Sps,
        pps: &Pps,
    ) -> Result<HevcDecoded, D3d11Error> {
        let poc = self.picture_poc(picture, sps)?;
        let mut emits: Vec<(i32, usize)> = Vec::with_capacity(SLOTS);
        let header = &picture.slices[0].header;
        if picture.irap && picture.no_output_prior() {
            self.empty_silent();
        } else {
            if picture.irap {
                self.emit_priors(&mut emits);
                if picture.idr || is_bla(picture) {
                    self.empty_refs();
                }
            }
            if !picture.idr {
                let rps = header
                    .rps
                    .as_ref()
                    .ok_or(D3d11Error::Parse(ParseReason::PictureWithoutReferenceSet))?;
                let lt_pocs = Self::lt_pocs(&header.lt, poc, sps)?;
                self.mark(rps, &lt_pocs, poc)?;
            }
            self.make_room(&mut emits)?;
        }
        let refs = self.ref_snapshot();
        let slot = self.alloc()?;
        self.slots[slot] = Some(Occupant {
            poc,
            long_term: false,
            reference: ref_pic(picture),
            displayed: !picture.slices[0].header.pic_output,
        });
        self.close_picture(picture, poc);
        self.reorder_bump(&mut emits);
        let submission = self.submit(picture, sps, pps, &refs, slot, poc)?;
        emits.sort_unstable();
        let current_index = emits
            .iter()
            .position(|(emit_poc, emit_slot)| *emit_poc == poc && *emit_slot == slot);
        let outputs = emits
            .into_iter()
            .map(|(_, slot)| slot_u8(slot).map(Output::from_slot))
            .collect::<Result<Vec<Output>, D3d11Error>>()?;
        Ok(HevcDecoded {
            submission,
            outputs,
            current_index,
        })
    }

    /// Undisplayed pictures in display order.
    pub(crate) fn drain(&mut self) -> Vec<Output> {
        let mut emits: Vec<(i32, usize)> = Vec::with_capacity(SLOTS);
        self.slots.emit_undisplayed(&mut emits);
        self.slots.free_displayed_non_refs();
        emits.sort_unstable();
        emits
            .into_iter()
            .filter_map(|(_, slot)| slot_u8(slot).ok().map(Output::from_slot))
            .collect()
    }

    /// Picture order count for `picture`.
    fn picture_poc(&self, picture: &Picture, sps: &Sps) -> Result<i32, D3d11Error> {
        if picture.idr {
            return Ok(0);
        }
        let lsb = picture
            .poc_lsb()
            .ok_or(D3d11Error::Parse(ParseReason::PictureWithoutPoc))?;
        if is_bla(picture) {
            return i32::try_from(lsb).map_err(|_| {
                D3d11Error::Parse(ParseReason::Range {
                    syntax: "pic_order_cnt",
                })
            });
        }
        let _ = sps;
        let msb = self.poc_msb(i64::from(lsb));
        let poc = msb + i64::from(lsb);
        i32::try_from(poc).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "pic_order_cnt",
            })
        })
    }

    /// POC MSB against the previous temporal-zero picture.
    fn poc_msb(&self, lsb: i64) -> i64 {
        if !self.has_prev_tid0 {
            return 0;
        }
        self.poc.msb_for(lsb, i64::from(self.max_poc_lsb))
    }

    /// Undisplayed priors emitted, references kept for set marking.
    fn emit_priors(&mut self, emits: &mut Vec<(i32, usize)>) {
        self.slots.emit_undisplayed(emits);
    }

    /// Every picture out of the reference set.
    fn empty_refs(&mut self) {
        self.slots.release_all();
        self.slots.free_displayed_non_refs();
    }

    /// Every picture dropped without output.
    fn empty_silent(&mut self) {
        self.slots.clear_slots();
    }

    /// Reference set marking for pictures carrying a set.
    fn mark(
        &mut self,
        rps: &ShortTermRps,
        lt_pocs: &[(i32, bool)],
        poc: i32,
    ) -> Result<(), D3d11Error> {
        for slot in self.slots.iter_mut().flatten() {
            if !slot.reference {
                continue;
            }
            match set_membership(slot.poc, rps, lt_pocs, poc) {
                SetMember::Unused => {
                    slot.release();
                }
                SetMember::ShortTerm => {
                    slot.long_term = false;
                }
                SetMember::LongTerm => {
                    slot.long_term = true;
                }
            }
        }
        for curr in curr_pocs(rps, lt_pocs, poc) {
            if !self.slots.iter().flatten().any(|slot| slot.poc == curr) {
                return Err(D3d11Error::BrokenReference(
                    BrokenReferenceReason::ReferencePocAbsent { poc: curr },
                ));
            }
        }
        self.slots.free_displayed_non_refs();
        Ok(())
    }

    /// Room for the next picture, emitting the lowest undisplayed POC first.
    fn make_room(&mut self, emits: &mut Vec<(i32, usize)>) -> Result<(), D3d11Error> {
        self.slots.free_displayed_non_refs();
        while self.slots.stored() >= usize::from(self.capacity) {
            if !self.slots.pop_earliest(emits, None) {
                return Err(D3d11Error::DpbFull);
            }
            self.slots.free_displayed_non_refs();
        }
        Ok(())
    }

    /// Undisplayed pictures past the reorder limit, lowest POC first.
    fn reorder_bump(&mut self, emits: &mut Vec<(i32, usize)>) {
        loop {
            if self.slots.buffered() <= usize::from(self.max_reorder) {
                break;
            }
            if !self.slots.pop_earliest(emits, None) {
                break;
            }
        }
        self.slots.free_displayed_non_refs();
    }

    /// POC state after storing the current picture.
    fn close_picture(&mut self, picture: &Picture, poc: i32) {
        if picture.temporal_id() != 0 {
            return;
        }
        let lsb = if picture.idr {
            0
        } else {
            picture.poc_lsb().unwrap_or(0)
        };
        self.poc.observe(i64::from(poc) - i64::from(lsb), lsb);
        self.has_prev_tid0 = true;
    }

    /// First free slot outside live surface pins.
    fn alloc(&self) -> Result<usize, D3d11Error> {
        self.slots
            .alloc_where(|index| self.pins.is_pinned(index))
            .map_err(|failure| match failure {
                SlotAllocFailure::PinnedBlocked => D3d11Error::NoFreeSlot,
                SlotAllocFailure::Full => D3d11Error::DpbFull,
            })
    }

    /// Reference pictures behind parameter construction.
    fn ref_snapshot(&self) -> Vec<RefState> {
        let mut refs = Vec::with_capacity(SLOTS);
        for (slot, occupant) in self.slots.iter().enumerate() {
            if let Some(occupant) = occupant {
                if occupant.reference {
                    refs.push(RefState {
                        slot,
                        poc: occupant.poc,
                        long_term: occupant.long_term,
                    });
                }
            }
        }
        refs
    }

    /// Long-term POCs with used flags for the current picture.
    fn lt_pocs(lt: &[LtSpec], poc: i32, sps: &Sps) -> Result<Vec<(i32, bool)>, D3d11Error> {
        lt.iter()
            .map(|pic| lt_poc(pic, poc, sps.max_poc_lsb()).map(|poc| (poc, pic.used)))
            .collect()
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
    ) -> Result<SubmittedHevc, D3d11Error> {
        let params = Self::fill_params(picture, sps, pps, refs, slot, poc)?;
        let qmatrix = if sps.scaling_enabled() {
            Some(QmatrixHevc {
                lists_0: pps.scaling.size0,
                lists_1: pps.scaling.size1,
                lists_2: pps.scaling.size2,
                lists_3: pps.scaling.size3,
                dc_size2: pps.scaling.dc2,
                dc_size3: pps.scaling.dc3,
            })
        } else {
            None
        };
        let mut bitstream = Vec::with_capacity(
            picture
                .slices
                .iter()
                .map(|slice| slice.nal.len() + 3)
                .sum::<usize>()
                + 128,
        );
        let mut slices = Vec::with_capacity(picture.slices.len());
        for slice in &picture.slices {
            let location =
                u32::try_from(bitstream.len()).map_err(|_| D3d11Error::BitstreamTooLarge {
                    len: bitstream.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            let bytes = slice
                .nal
                .len()
                .checked_add(3)
                .and_then(|len| u32::try_from(len).ok())
                .ok_or(D3d11Error::BitstreamTooLarge {
                    len: slice.nal.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            bitstream.extend_from_slice(&[0, 0, 1]);
            bitstream.extend_from_slice(&slice.nal);
            slices.push(SliceShortHevc {
                location,
                bytes,
                chopping: 0,
            });
        }
        let padding = 128 - (bitstream.len() % 128);
        bitstream.resize(bitstream.len() + padding, 0);
        if let Some(last) = slices.last_mut() {
            let grown = last
                .bytes
                .checked_add(u32::try_from(padding).unwrap_or(0))
                .ok_or(D3d11Error::BitstreamTooLarge {
                    len: bitstream.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            last.bytes = grown;
        }
        let feedback = self.feedback;
        self.feedback = self.feedback.wrapping_add(1);
        let mut params = params;
        params.feedback = feedback;
        Ok(SubmittedHevc {
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
        refs: &[RefState],
        slot: usize,
        poc: i32,
    ) -> Result<PicParamsHevc, D3d11Error> {
        let mut params = PicParamsHevc::empty();
        params.width_in_min_cbs =
            u16::try_from(sps.width_in_min_cbs()).map_err(|_| D3d11Error::OverCaps {
                what: "picture width",
            })?;
        params.height_in_min_cbs =
            u16::try_from(sps.height_in_min_cbs()).map_err(|_| D3d11Error::OverCaps {
                what: "picture height",
            })?;
        params.format_flags = HEVC_FORMAT_CHROMA_420
            | (u16::from(sps.log2_max_poc_lsb_minus4) << HEVC_FORMAT_POC_SHIFT);
        params.curr_pic = PicEntryHevc::frame(slot_u8(slot)?);
        params.max_dec_pic_buffering_minus1 = sps.max_dec_pic_buffering_minus1;
        params.log2_min_cb_size_minus3 = sps.log2_min_cb_size_minus3;
        params.log2_diff_max_min_cb_size = sps.log2_diff_max_min_cb_size;
        params.log2_min_tb_size_minus2 = sps.log2_min_tb_size_minus2;
        params.log2_diff_max_min_tb_size = sps.log2_diff_max_min_tb_size;
        params.max_transform_depth_inter = sps.max_transform_depth_inter;
        params.max_transform_depth_intra = sps.max_transform_depth_intra;
        params.num_short_term_rps = u8::try_from(sps.rps.len()).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "rps count",
            })
        })?;
        params.num_long_term_sps = u8::try_from(sps.lt_poc_lsb.len()).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "long-term count",
            })
        })?;
        params.num_ref_l0_default_minus1 = pps.num_ref_l0;
        params.num_ref_l1_default_minus1 = pps.num_ref_l1;
        params.init_qp_minus26 = pps.pic_init_qp_minus26;
        let header = &picture.slices[0].header;
        if let Some(bits) = header.rps_bits {
            if bits.explicit {
                params.num_delta_pocs_of_ref_rps = bits.ref_num_delta_pocs;
                params.num_bits_for_rps_in_slice = bits.bits;
            }
        }
        params.tool_flags = tool_flags(sps, pps);
        params.pic_flags = pic_flags(picture, pps);
        params.pps_cb_qp_offset = pps.cb_qp_offset;
        params.pps_cr_qp_offset = pps.cr_qp_offset;
        params.diff_cu_qp_delta_depth = pps.diff_cu_qp_delta_depth;
        params.pps_beta_offset_div2 = pps.beta_offset_div2;
        params.pps_tc_offset_div2 = pps.tc_offset_div2;
        params.log2_parallel_merge_minus2 = pps.log2_parallel_merge_minus2;
        params.curr_poc = poc;
        fill_tiles(&mut params, pps);
        fill_ref_lists(&mut params, refs)?;
        if let Some(rps) = header.rps.as_ref() {
            let lt = Self::lt_pocs(&header.lt, poc, sps)?;
            fill_ref_sets(&mut params, rps, &lt, poc, refs)?;
        }
        Ok(params)
    }
}

/// True when `picture` starts a closed-GOP BLA sequence.
fn is_bla(picture: &Picture) -> bool {
    picture
        .slices
        .first()
        .is_some_and(|slice| matches!(slice.nal_type, 16..=18))
}

/// True when the picture is used for reference, H.265 7.4.2.2.
fn ref_pic(picture: &Picture) -> bool {
    picture
        .slices
        .first()
        .is_some_and(|slice| slice.nal_type >= 16 || slice.nal_type % 2 == 1)
}

/// Set membership of a stored POC.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SetMember {
    /// In no set.
    Unused,
    /// In a short-term current set.
    ShortTerm,
    /// In a long-term current set.
    LongTerm,
}

/// Membership of stored `poc` in the resolved reference sets.
fn set_membership(
    poc: i32,
    rps: &ShortTermRps,
    lt_pocs: &[(i32, bool)],
    current: i32,
) -> SetMember {
    if short_term_member(poc, rps, current) {
        return SetMember::ShortTerm;
    }
    if lt_pocs.iter().any(|(lt, _)| *lt == poc) {
        return SetMember::LongTerm;
    }
    SetMember::Unused
}

/// True when stored `poc` sits in any short-term set against `current`.
fn short_term_member(poc: i32, rps: &ShortTermRps, current: i32) -> bool {
    rps.negative[..usize::from(rps.negative_count)]
        .iter()
        .chain(rps.positive[..usize::from(rps.positive_count)].iter())
        .any(|delta| delta.checked_add(current) == Some(poc))
}

/// Current-set POCs behind presence checks.
fn curr_pocs(rps: &ShortTermRps, lt_pocs: &[(i32, bool)], current: i32) -> Vec<i32> {
    let mut pocs = Vec::new();
    for (index, delta) in rps.negative[..usize::from(rps.negative_count)]
        .iter()
        .enumerate()
    {
        if rps.negative_used[index] {
            pocs.extend(delta.checked_add(current));
        }
    }
    for (index, delta) in rps.positive[..usize::from(rps.positive_count)]
        .iter()
        .enumerate()
    {
        if rps.positive_used[index] {
            pocs.extend(delta.checked_add(current));
        }
    }
    pocs.extend(
        lt_pocs
            .iter()
            .filter(|(_, used)| *used)
            .map(|(poc, _)| *poc),
    );
    pocs
}

/// Long-term POC from its description against the current picture.
fn lt_poc(spec: &LtSpec, poc: i32, max_lsb: u32) -> Result<i32, D3d11Error> {
    let max_lsb = i64::from(max_lsb);
    let lsb = i64::from(spec.poc_lsb);
    let current = i64::from(poc);
    let derived = if spec.msb_present {
        let cycle_pos = current.rem_euclid(max_lsb);
        current - i64::from(spec.msb_cycle) * max_lsb - cycle_pos + lsb
    } else {
        let cycle_base = current - current.rem_euclid(max_lsb);
        let cycle_pos = current.rem_euclid(max_lsb);
        if lsb < cycle_pos && cycle_pos - lsb >= max_lsb / 2 {
            cycle_base + max_lsb + lsb
        } else if lsb > cycle_pos && lsb - cycle_pos > max_lsb / 2 {
            cycle_base - max_lsb + lsb
        } else {
            cycle_base + lsb
        }
    };
    i32::try_from(derived).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "long_term poc",
        })
    })
}

/// Packed coding tool flags.
fn tool_flags(sps: &Sps, pps: &Pps) -> u32 {
    let mut flags = 0u32;
    if sps.scaling_enabled() {
        flags |= HEVC_TOOL_SCALING;
    }
    if sps.amp() {
        flags |= HEVC_TOOL_AMP;
    }
    if sps.sao() {
        flags |= HEVC_TOOL_SAO;
    }
    if sps.pcm() {
        flags |= HEVC_TOOL_PCM;
        flags |= u32::from(sps.pcm_depth_luma) << HEVC_TOOL_PCM_LUMA_SHIFT;
        flags |= u32::from(sps.pcm_depth_chroma) << HEVC_TOOL_PCM_CHROMA_SHIFT;
        flags |= u32::from(sps.pcm_log2_min_minus3) << HEVC_TOOL_PCM_MIN_SHIFT;
        flags |= u32::from(sps.pcm_log2_diff) << HEVC_TOOL_PCM_DIFF_SHIFT;
        if sps.pcm_loop_disabled() {
            flags |= HEVC_TOOL_PCM_LOOP_DISABLED;
        }
    }
    if sps.long_term_present() {
        flags |= HEVC_TOOL_LONG_TERM;
    }
    if sps.temporal_mvp() {
        flags |= HEVC_TOOL_TEMPORAL_MVP;
    }
    if sps.smoothing() {
        flags |= HEVC_TOOL_SMOOTHING;
    }
    if pps.dependent_slices() {
        flags |= HEVC_TOOL_DEPENDENT_SLICES;
    }
    if pps.output_flag_present() {
        flags |= HEVC_TOOL_OUTPUT_FLAG;
    }
    flags |= u32::from(pps.num_extra_slice_header_bits) << HEVC_TOOL_EXTRA_BITS_SHIFT;
    if pps.sign_hiding() {
        flags |= HEVC_TOOL_SIGN_HIDING;
    }
    if pps.cabac_init_present() {
        flags |= HEVC_TOOL_CABAC_INIT;
    }
    flags
}

/// Packed picture property flags.
fn pic_flags(picture: &Picture, pps: &Pps) -> u32 {
    let mut flags = 0u32;
    if pps.constrained_intra() {
        flags |= HEVC_PIC_CONSTRAINED_INTRA;
    }
    if pps.transform_skip() {
        flags |= HEVC_PIC_TRANSFORM_SKIP;
    }
    if pps.cu_qp_delta() {
        flags |= HEVC_PIC_CU_QP_DELTA;
    }
    if pps.chroma_offsets_present() {
        flags |= HEVC_PIC_CHROMA_OFFSETS;
    }
    if pps.weighted_pred() {
        flags |= HEVC_PIC_WEIGHTED_PRED;
    }
    if pps.weighted_bipred() {
        flags |= HEVC_PIC_WEIGHTED_BIPRED;
    }
    if pps.transquant_bypass() {
        flags |= HEVC_PIC_TRANSQUANT_BYPASS;
    }
    if pps.tiles_enabled() {
        flags |= HEVC_PIC_TILES;
        if pps.loop_across_tiles() {
            flags |= HEVC_PIC_LOOP_ACROSS_TILES;
        }
    }
    if pps.entropy_sync() {
        flags |= HEVC_PIC_SYNC;
    }
    if pps.uniform_spacing() {
        flags |= HEVC_PIC_UNIFORM_SPACING;
    }
    if pps.loop_across_slices() {
        flags |= HEVC_PIC_LOOP_ACROSS_SLICES;
    }
    if pps.deblocking_override() {
        flags |= HEVC_PIC_DEBLOCK_OVERRIDE;
    }
    if pps.deblocking_disabled() {
        flags |= HEVC_PIC_DEBLOCK_DISABLED;
    }
    if pps.lists_modification_present() {
        flags |= HEVC_PIC_LISTS_MOD;
    }
    if pps.slice_ext_present() {
        flags |= HEVC_PIC_SLICE_EXT;
    }
    if picture.irap {
        flags |= HEVC_PIC_IRAP | HEVC_PIC_INTRA;
    }
    if picture.idr {
        flags |= HEVC_PIC_IDR;
    }
    flags
}

/// Tile grid into the picture parameters.
fn fill_tiles(params: &mut PicParamsHevc, pps: &Pps) {
    if !pps.tiles_enabled() {
        return;
    }
    params.num_tile_columns_minus1 = pps.num_tile_columns - 1;
    params.num_tile_rows_minus1 = pps.num_tile_rows - 1;
    if pps.uniform_spacing() {
        return;
    }
    for (index, width) in pps.column_width.iter().enumerate().take(19) {
        params.column_width_minus1[index] = width - 1;
    }
    for (index, height) in pps.row_height.iter().enumerate().take(21) {
        params.row_height_minus1[index] = height - 1;
    }
}

/// Reference surfaces plus POCs compacted from the snapshot.
fn fill_ref_lists(params: &mut PicParamsHevc, refs: &[RefState]) -> Result<(), D3d11Error> {
    for (index, state) in refs.iter().enumerate().take(REF_LIST_LEN) {
        params.ref_pic_list[index] = PicEntryHevc::new(slot_u8(state.slot)?, state.long_term);
        params.poc_list[index] = state.poc;
    }
    Ok(())
}

/// Reference set indices into the parameter reference list.
fn fill_ref_sets(
    params: &mut PicParamsHevc,
    rps: &ShortTermRps,
    lt: &[(i32, bool)],
    poc: i32,
    refs: &[RefState],
) -> Result<(), D3d11Error> {
    let mut before = 0usize;
    for (index, delta) in rps.negative[..usize::from(rps.negative_count)]
        .iter()
        .enumerate()
    {
        if !rps.negative_used[index] || before >= params.ref_set_st_curr_before.len() {
            continue;
        }
        let member = delta.checked_add(poc).ok_or({
            D3d11Error::Parse(ParseReason::Range {
                syntax: "delta_poc",
            })
        })?;
        params.ref_set_st_curr_before[before] = pp_index(refs, member)?;
        before += 1;
    }
    let mut after = 0usize;
    for (index, delta) in rps.positive[..usize::from(rps.positive_count)]
        .iter()
        .enumerate()
    {
        if !rps.positive_used[index] || after >= params.ref_set_st_curr_after.len() {
            continue;
        }
        let member = delta.checked_add(poc).ok_or({
            D3d11Error::Parse(ParseReason::Range {
                syntax: "delta_poc",
            })
        })?;
        params.ref_set_st_curr_after[after] = pp_index(refs, member)?;
        after += 1;
    }
    let mut curr = 0usize;
    for (member, used) in lt {
        if !used || curr >= params.ref_set_lt_curr.len() {
            continue;
        }
        params.ref_set_lt_curr[curr] = pp_index(refs, *member)?;
        curr += 1;
    }
    Ok(())
}

/// Parameter position behind stored `poc`.
fn pp_index(refs: &[RefState], poc: i32) -> Result<u8, D3d11Error> {
    refs.iter()
        .position(|state| state.poc == poc)
        .and_then(|index| u8::try_from(index).ok())
        .ok_or(D3d11Error::BrokenReference(
            BrokenReferenceReason::ReferencePocAbsent { poc },
        ))
}

/// Slot number as a byte.
fn slot_u8(slot: usize) -> Result<u8, D3d11Error> {
    u8::try_from(slot).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "slot" }))
}

/// Driver submission for one decoded picture.
#[derive(Debug)]
pub(crate) struct SubmittedHevc {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsHevc,
    /// Quantization matrix buffer, absent without scaling lists.
    pub(crate) qmatrix: Option<QmatrixHevc>,
    /// Slice control buffer.
    pub(crate) slices: Vec<SliceShortHevc>,
    /// Concatenated slices with start codes, padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface slot.
    pub(crate) slot: u8,
}

/// Submission plus display-order outputs.
#[derive(Debug)]
pub(crate) struct HevcDecoded {
    /// Driver submission.
    pub(crate) submission: SubmittedHevc,
    /// Display-order outputs.
    pub(crate) outputs: Vec<Output>,
    /// Current picture output behind post-submit copies.
    pub(crate) current_index: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::{lt_poc, HevcDpb};
    use crate::dpb::Output;
    use crate::dxva::{HEVC_PIC_IDR, HEVC_PIC_INTRA, HEVC_PIC_IRAP};
    use crate::h265::{
        ConstraintFlags, LtSpec, PendingSlice, Picture, Pps, ProfileTierLevel, RpsBits,
        ScalingLists, ShortTermRps, SliceHeader, SliceType, SourceFlags, Sps, SPS_SAO,
        SPS_TEMPORAL_MVP,
    };

    /// Sequence set with POC width 16 and reorder depth two.
    fn sps() -> Sps {
        Sps {
            id: 0,
            flags: SPS_SAO | SPS_TEMPORAL_MVP,
            max_sub_layers: 1,
            level_idc: 120,
            width: 320,
            height: 240,
            crop: None,
            log2_max_poc_lsb_minus4: 0,
            max_dec_pic_buffering_minus1: 4,
            max_num_reorder_pics: 2,
            log2_min_cb_size_minus3: 0,
            log2_diff_max_min_cb_size: 3,
            log2_min_tb_size_minus2: 0,
            log2_diff_max_min_tb_size: 3,
            max_transform_depth_inter: 1,
            max_transform_depth_intra: 1,
            scaling: ScalingLists::default(),
            pcm_depth_luma: 0,
            pcm_depth_chroma: 0,
            pcm_log2_min_minus3: 0,
            pcm_log2_diff: 0,
            rps: Vec::new(),
            lt_poc_lsb: Vec::new(),
            lt_used_by_curr: Vec::new(),
            vps_id: 0,
            ptl: ProfileTierLevel {
                tier: false,
                profile_idc: 1,
                compat: 0x4000_0000,
                source: SourceFlags {
                    progressive: true,
                    interlaced: false,
                },
                constraints: ConstraintFlags {
                    non_packed: true,
                    frame_only: true,
                },
                level_idc: 120,
                sub_layers: Vec::new(),
            },
            chroma_format: 1,
            separate_colour_plane: false,
            bit_depth_luma: 0,
            bit_depth_chroma: 0,
            ordering_buffering: vec![4],
            ordering_reorder: vec![2],
            ordering_latency: vec![0],
            vui: None,
            sps_extension_present: false,
            sps_extension_flags: [false; 4],
        }
    }

    /// Picture set with defaults.
    fn pps() -> Pps {
        Pps {
            id: 0,
            sps_id: 0,
            flags: 0,
            num_extra_slice_header_bits: 0,
            num_ref_l0: 0,
            num_ref_l1: 0,
            pic_init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            num_tile_columns: 1,
            num_tile_rows: 1,
            column_width: Vec::new(),
            row_height: Vec::new(),
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            scaling: ScalingLists::default(),
            log2_parallel_merge_minus2: 0,
            deblock_control_present: false,
            pps_extension_present: false,
            pps_extension_flags: [false; 4],
        }
    }

    /// Reference set from delta and used pairs.
    fn rps(negative: &[(i32, bool)], positive: &[(i32, bool)]) -> ShortTermRps {
        let mut set = ShortTermRps::default();
        for (delta, used) in negative {
            let at = usize::from(set.negative_count);
            set.negative[at] = *delta;
            set.negative_used[at] = *used;
            set.negative_count += 1;
        }
        for (delta, used) in positive {
            let at = usize::from(set.positive_count);
            set.positive[at] = *delta;
            set.positive_used[at] = *used;
            set.positive_count += 1;
        }
        set
    }

    /// Explicit set provenance.
    fn bits() -> RpsBits {
        RpsBits {
            explicit: true,
            bits: 12,
            ref_num_delta_pocs: 0,
            table_idx: None,
        }
    }

    /// Single-slice picture.
    fn picture(
        nal_type: u8,
        slice_type: SliceType,
        poc_lsb: Option<u32>,
        set: Option<(ShortTermRps, RpsBits)>,
    ) -> Picture {
        let (rps, rps_bits) = set.map_or((None, None), |(rps, bits)| (Some(rps), Some(bits)));
        Picture {
            slices: vec![PendingSlice {
                header: SliceHeader {
                    first: true,
                    address: 0,
                    slice_type,
                    pic_output: true,
                    no_output_prior: false,
                    poc_lsb,
                    rps,
                    rps_bits,
                    lt: Vec::new(),
                    entry_count: 0,
                    extension_length: 0,
                },
                nal: vec![0x26, 0x01, 0x88],
                nal_type,
                temporal_id: 0,
                pps_id: 0,
            }],
            idr: (19..=20).contains(&nal_type),
            irap: (16..=23).contains(&nal_type),
            rasl: (8..=9).contains(&nal_type),
        }
    }

    /// IDR picture.
    fn idr() -> Picture {
        picture(20, SliceType::I, None, None)
    }

    /// P picture with `lsb` referencing `delta` back.
    fn p(lsb: u32, delta: i32) -> Picture {
        picture(
            1,
            SliceType::P,
            Some(lsb),
            Some((rps(&[(delta, true)], &[]), bits())),
        )
    }

    /// BLA picture with `lsb` and an empty reference set.
    fn bla(lsb: u32) -> Picture {
        picture(16, SliceType::I, Some(lsb), Some((rps(&[], &[]), bits())))
    }

    /// Output slots in order.
    fn slots(outputs: &[Output]) -> Vec<u8> {
        outputs.iter().map(Output::slot).collect()
    }

    #[test]
    fn idr_occupies_slot_zero() {
        let mut dpb = HevcDpb::new(&sps());
        let decoded = dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        assert!(decoded.outputs.is_empty());
        assert_eq!(decoded.submission.slot, 0);
        let params = &decoded.submission.params;
        assert_eq!(params.curr_poc, 0);
        assert_eq!(params.curr_pic.entry, 0);
        assert!(params.ref_pic_list.iter().all(|entry| entry.entry == 0xFF));
        assert_eq!(params.feedback, 1);
        assert_eq!(
            params.pic_flags & (HEVC_PIC_IRAP | HEVC_PIC_IDR | HEVC_PIC_INTRA),
            HEVC_PIC_IRAP | HEVC_PIC_IDR | HEVC_PIC_INTRA
        );
        assert_eq!(params.width_in_min_cbs, 40);
        assert_eq!(params.height_in_min_cbs, 30);
    }

    #[test]
    fn no_reorder_emits_per_call() {
        let mut sequence = sps();
        sequence.max_num_reorder_pics = 0;
        let mut dpb = HevcDpb::new(&sequence);
        let decoded = dpb.decode(&idr(), &sequence, &pps()).expect("idr");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        assert_eq!(decoded.current_index, Some(0));
        let decoded = dpb.decode(&p(2, -2), &sequence, &pps()).expect("p");
        assert_eq!(slots(&decoded.outputs), vec![1]);
        assert_eq!(decoded.current_index, Some(0));
    }

    #[test]
    fn pinned_slot_skips_alloc() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.pins().pin(0);
        let decoded = dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        assert_eq!(decoded.submission.slot, 1);
    }

    #[test]
    fn all_pinned_errors_no_free_slot() {
        let mut dpb = HevcDpb::new(&sps());
        for slot in 0u8..16 {
            dpb.pins().pin(slot);
        }
        let error = dpb.decode(&idr(), &sps(), &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::NoFreeSlot));
        assert_eq!(error.to_string(), "no free surface slot");
    }

    #[test]
    fn p_references_idr() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(2, -2), &sps(), &pps()).expect("p");
        assert!(
            decoded.outputs.is_empty(),
            "reorder depth buffers the picture"
        );
        assert_eq!(decoded.submission.slot, 1);
        let params = &decoded.submission.params;
        assert_eq!(params.curr_poc, 2);
        assert_eq!(params.curr_pic.entry, 1);
        assert_eq!(params.ref_pic_list[0].entry, 0);
        assert_eq!(params.poc_list[0], 0);
        assert_eq!(params.ref_set_st_curr_before[0], 0);
        assert_eq!(params.ref_set_st_curr_before[1], 0xFF);
        assert_eq!(params.num_bits_for_rps_in_slice, 12);
        assert_eq!(params.feedback, 2);
        let decoded = dpb.decode(&p(4, -2), &sps(), &pps()).expect("p2");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        assert_eq!(decoded.submission.slot, 2);
    }

    #[test]
    fn b_reorders_by_limit() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        assert!(decoded.outputs.is_empty());
        let b = picture(
            1,
            SliceType::B,
            Some(2),
            Some((rps(&[(-2, true)], &[(2, true)]), bits())),
        );
        let decoded = dpb.decode(&b, &sps(), &pps()).expect("b");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        let params = &decoded.submission.params;
        assert_eq!(params.ref_set_st_curr_before[0], 0);
        assert_eq!(params.ref_set_st_curr_after[0], 1);
        assert_eq!(slots(&dpb.drain()), vec![2, 1]);
    }

    #[test]
    fn cra_keeps_following_refs() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        let cra = picture(
            21,
            SliceType::I,
            Some(8),
            Some((rps(&[(-4, false)], &[]), bits())),
        );
        let decoded = dpb.decode(&cra, &sps(), &pps()).expect("cra");
        assert_eq!(slots(&decoded.outputs), vec![0, 1]);
        let rasl = picture(
            8,
            SliceType::P,
            Some(6),
            Some((rps(&[(-2, true)], &[]), bits())),
        );
        let decoded = dpb.decode(&rasl, &sps(), &pps()).expect("rasl decodes");
        assert!(
            decoded.outputs.is_empty(),
            "reorder depth buffers the picture"
        );
        assert_eq!(decoded.submission.params.ref_set_st_curr_before[0], 0);
        assert_eq!(slots(&dpb.drain()), vec![2, 0]);
    }

    #[test]
    fn bla_resets_poc_to_lsb() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        let decoded = dpb.decode(&bla(2), &sps(), &pps()).expect("bla");
        assert_eq!(slots(&decoded.outputs), vec![0, 1]);
        assert_eq!(decoded.submission.params.curr_poc, 2);
    }

    #[test]
    fn no_output_prior_drops_silently() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        let mut restart = idr();
        restart.slices[0].header.no_output_prior = true;
        let decoded = dpb.decode(&restart, &sps(), &pps()).expect("restart");
        assert!(decoded.outputs.is_empty(), "priors dropped without output");
        assert_eq!(slots(&dpb.drain()), vec![0]);
    }

    #[test]
    fn missing_reference_breaks() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let error = dpb.decode(&p(4, -2), &sps(), &pps()).unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d11Error::BrokenReference(_)
        ));
    }

    #[test]
    fn lt_poc_follows_msb_cycles() {
        let plain = LtSpec {
            poc_lsb: 4,
            used: true,
            msb_present: false,
            msb_cycle: 0,
        };
        assert_eq!(lt_poc(&plain, 20, 16).expect("near"), 20);
        let wrapped = LtSpec {
            poc_lsb: 14,
            used: true,
            msb_present: false,
            msb_cycle: 0,
        };
        assert_eq!(lt_poc(&wrapped, 2, 16).expect("wrapped"), -2);
        let cycled = LtSpec {
            poc_lsb: 4,
            used: true,
            msb_present: true,
            msb_cycle: 1,
        };
        assert_eq!(lt_poc(&cycled, 20, 16).expect("cycled"), 4);
    }

    #[test]
    fn submission_carries_bitstream() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(2, -2), &sps(), &pps()).expect("p");
        let submission = &decoded.submission;
        assert_eq!(submission.bitstream.len(), 128);
        assert_eq!(submission.bitstream[..6], [0, 0, 1, 0x26, 0x01, 0x88]);
        assert!(submission.bitstream[6..].iter().all(|byte| *byte == 0));
        assert!(submission.qmatrix.is_none());
        assert_eq!(submission.slices.len(), 1);
        let slice = &submission.slices[0];
        assert_eq!(slice.location, 0);
        assert_eq!(slice.bytes, 128);
        assert_eq!(slice.chopping, 0);
    }

    #[test]
    fn scaling_lists_ride_along() {
        let mut sequence = sps();
        sequence.flags |= crate::h265::SPS_SCALING;
        let mut dpb = HevcDpb::new(&sequence);
        let decoded = dpb.decode(&idr(), &sequence, &pps()).expect("idr");
        let matrix = decoded.submission.qmatrix.expect("matrix");
        assert_eq!(matrix.lists_0, [[16; 16]; 6]);
        assert_eq!(matrix.lists_1[0][0], 16);
        assert_eq!(matrix.lists_1[0][63], 115);
        assert_eq!(matrix.lists_1[5][63], 91);
        assert_eq!(matrix.dc_size3, [16; 2]);
    }

    #[test]
    fn dpb_full_without_output() {
        let mut sequence = sps();
        sequence.max_dec_pic_buffering_minus1 = 0;
        let mut dpb = HevcDpb::new(&sequence);
        dpb.decode(&idr(), &sequence, &pps()).expect("idr");
        let error = dpb.decode(&p(2, -2), &sequence, &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::DpbFull));
    }

    #[test]
    fn drain_emits_poc_order() {
        let mut dpb = HevcDpb::new(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        assert_eq!(slots(&dpb.drain()), vec![0, 1]);
        assert!(dpb.drain().is_empty(), "drain runs once");
    }
}
