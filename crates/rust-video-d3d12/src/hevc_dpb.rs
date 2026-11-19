//! HEVC decoded picture buffer with DXVA submission payloads.

use std::sync::Arc;

use crate::decode::{start_coded_bitstream, HevcSubmission};
use crate::dpb::{outputs_sorted, slot_u8, Output};
use crate::dxva::{
    PicEntryHevc, PicParamsHevc, QmatrixHevc, SliceShortHevc, HEVC_CHROMA_420, HEVC_IDR_PIC,
    HEVC_INTRA_PIC, HEVC_IRAP_PIC, HEVC_POC_LSB_SHIFT, HEVC_PROP_CHROMA_QP_OFFSETS,
    HEVC_PROP_CONSTRAINED_INTRA, HEVC_PROP_CU_QP_DELTA, HEVC_PROP_DEBLOCK_DISABLED,
    HEVC_PROP_DEBLOCK_OVERRIDE, HEVC_PROP_ENTROPY_SYNC, HEVC_PROP_LISTS_MOD, HEVC_PROP_LOOP_SLICES,
    HEVC_PROP_LOOP_TILES, HEVC_PROP_SLICE_EXT, HEVC_PROP_TILES, HEVC_PROP_TRANSFORM_SKIP,
    HEVC_PROP_TRANSQUANT_BYPASS, HEVC_PROP_UNIFORM_SPACING, HEVC_PROP_WEIGHTED_BIPRED,
    HEVC_PROP_WEIGHTED_PRED, HEVC_TOOL_AMP, HEVC_TOOL_CABAC_INIT, HEVC_TOOL_DEPENDENT_SLICES,
    HEVC_TOOL_EXTRA_BITS_SHIFT, HEVC_TOOL_OUTPUT_FLAG, HEVC_TOOL_SAO, HEVC_TOOL_SIGN_HIDING,
    HEVC_TOOL_STRONG_INTRA, HEVC_TOOL_TEMPORAL_MVP,
};
use crate::error::{BrokenReferenceReason, D3d12Error, ParseReason, UnsupportedReason};
use crate::hevc::{Picture, Pps, ShortTermRps, Sps};
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
    }
}

/// Reference picture behind parameter construction.
#[derive(Clone, Copy)]
struct RefState {
    /// DPB slot.
    slot: usize,
    /// Picture order count.
    poc: i32,
}

/// Decoded picture buffer for one sequence.
pub(crate) struct HevcDpb {
    /// Picture slots.
    slots: [Option<Occupant>; SLOTS],
    /// Live-surface pin counts over the decoder pool.
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
    /// Empty buffer for `sps` over the `pins` pool table.
    pub(crate) fn new(sps: &Sps, pins: Arc<SlotPins>) -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            pins,
            capacity: sps.max_dec_pic_buffering_minus1 + 1,
            max_reorder: sps.max_num_reorder_pics,
            max_poc_lsb: sps.max_poc_lsb(),
            poc: PocTracker::new(),
            has_prev_tid0: false,
            feedback: 1,
        }
    }

    /// Submission plus display-order outputs for `picture`.
    pub(crate) fn decode(
        &mut self,
        picture: &Picture,
        sps: &Sps,
        pps: &Pps,
    ) -> Result<HevcDecoded, D3d12Error> {
        if picture.slices.is_empty() {
            return Err(D3d12Error::Parse(ParseReason::PictureWithoutSlices));
        }
        if matches!(self.alloc(), Err(D3d12Error::NoFreeSlot)) {
            return Err(D3d12Error::NoFreeSlot);
        }
        let poc = self.picture_poc(picture)?;
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
                    .ok_or(D3d12Error::Parse(ParseReason::PictureWithoutReferenceSet))?;
                self.mark(rps, poc)?;
            }
            self.make_room(&mut emits)?;
        }
        let refs = self.ref_snapshot();
        let slot = self.alloc()?;
        self.slots[slot] = Some(Occupant {
            poc,
            reference: ref_pic(picture),
            displayed: !picture.slices[0].header.pic_output,
        });
        self.close_picture(picture, poc);
        self.reorder_bump(&mut emits);
        let submission = self.submit(picture, sps, pps, &refs, slot, poc)?;
        Ok(HevcDecoded {
            submission,
            outputs: outputs_sorted(emits),
        })
    }

    /// Undisplayed pictures in display order.
    pub(crate) fn drain(&mut self) -> Vec<Output> {
        let mut emits: Vec<(i32, usize)> = Vec::with_capacity(SLOTS);
        self.slots.emit_undisplayed(&mut emits);
        self.slots.free_displayed_non_refs();
        outputs_sorted(emits)
    }

    /// Picture order count for `picture`.
    fn picture_poc(&self, picture: &Picture) -> Result<i32, D3d12Error> {
        if picture.idr {
            return Ok(0);
        }
        let lsb = picture
            .poc_lsb()
            .ok_or(D3d12Error::Parse(ParseReason::PictureWithoutPoc))?;
        if is_bla(picture) {
            return i32::try_from(lsb).map_err(|_| {
                D3d12Error::Parse(ParseReason::Range {
                    syntax: "pic_order_cnt",
                })
            });
        }
        let msb = self.poc_msb(i64::from(lsb));
        let poc = msb + i64::from(lsb);
        i32::try_from(poc).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
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
    fn mark(&mut self, rps: &ShortTermRps, poc: i32) -> Result<(), D3d12Error> {
        for slot in self.slots.iter_mut().flatten() {
            if !slot.reference {
                continue;
            }
            if !short_term_member(slot.poc, rps, poc) {
                slot.release();
            }
        }
        for curr in curr_pocs(rps, poc) {
            if !self.slots.iter().flatten().any(|slot| slot.poc == curr) {
                return Err(D3d12Error::BrokenReference(
                    BrokenReferenceReason::ReferencePocAbsent { poc: curr },
                ));
            }
        }
        self.slots.free_displayed_non_refs();
        Ok(())
    }

    /// Room for the next picture, emitting the lowest undisplayed POC first.
    fn make_room(&mut self, emits: &mut Vec<(i32, usize)>) -> Result<(), D3d12Error> {
        self.slots.free_displayed_non_refs();
        while self.slots.stored() >= usize::from(self.capacity) {
            if !self.slots.pop_earliest(emits, None) {
                return Err(D3d12Error::DpbFull);
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

    /// First free slot no live surface pins.
    fn alloc(&self) -> Result<usize, D3d12Error> {
        self.slots
            .alloc_where(|index| u8::try_from(index).is_ok_and(|slot| self.pins.pinned(slot)))
            .map_err(|failure| match failure {
                SlotAllocFailure::PinnedBlocked => D3d12Error::NoFreeSlot,
                SlotAllocFailure::Full => D3d12Error::DpbFull,
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
    ) -> Result<HevcSubmission, D3d12Error> {
        let mut params = Self::fill_params(picture, sps, pps, refs, slot, poc)?;
        let qmatrix = QmatrixHevc {
            lists0: [[16; 16]; 6],
            lists1: [[16; 64]; 6],
            lists2: [[16; 64]; 6],
            lists3: [[16; 64]; 2],
            dc2: [16; 6],
            dc3: [16; 2],
        };
        let nals: Vec<&[u8]> = picture
            .slices
            .iter()
            .map(|slice| slice.nal.as_slice())
            .collect();
        let (spans, bitstream) = start_coded_bitstream(&nals)?;
        let slices = spans
            .into_iter()
            .map(|(location, bytes)| SliceShortHevc {
                location,
                bytes,
                chopping: 0,
            })
            .collect();
        let feedback = self.feedback;
        self.feedback = self.feedback.wrapping_add(1);
        params.status_report_feedback_number = feedback;
        Ok(HevcSubmission {
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
    ) -> Result<PicParamsHevc, D3d12Error> {
        let min_cb = sps.min_cb();
        let width_in_min_cbs = u16::try_from(sps.width().div_ceil(min_cb))
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        let height_in_min_cbs = u16::try_from(sps.height().div_ceil(min_cb))
            .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
        let mut params = PicParamsHevc::empty();
        params.pic_width_in_min_cbs_y = width_in_min_cbs;
        params.pic_height_in_min_cbs_y = height_in_min_cbs;
        params.format_flags =
            HEVC_CHROMA_420 | (u16::from(sps.log2_max_poc_lsb_minus4) << HEVC_POC_LSB_SHIFT);
        params.curr_pic = PicEntryHevc::frame(slot_u8(slot)?);
        params.sps_max_dec_pic_buffering_minus1 = sps.max_dec_pic_buffering_minus1;
        params.log2_min_luma_coding_block_size_minus3 = sps.log2_min_cb_minus3;
        params.log2_diff_max_min_luma_coding_block_size = sps.log2_diff_max_min_cb;
        params.log2_min_transform_block_size_minus2 = sps.log2_min_tb_minus2;
        params.log2_diff_max_min_transform_block_size = sps.log2_diff_max_min_tb;
        params.max_transform_hierarchy_depth_inter = sps.max_depth_inter;
        params.max_transform_hierarchy_depth_intra = sps.max_depth_intra;
        params.num_short_term_ref_pic_sets = u8::try_from(sps.rps.len()).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "rps count",
            })
        })?;
        params.num_ref_idx_l0_default_active_minus1 = pps.num_ref_l0;
        params.num_ref_idx_l1_default_active_minus1 = pps.num_ref_l1;
        params.init_qp_minus26 = pps.init_qp_minus26;
        let header = &picture.slices[0].header;
        if let Some(bits) = header.rps_bits {
            if bits.explicit {
                params.num_delta_pocs_of_ref_rps_idx = bits.ref_num_delta_pocs;
                params.num_bits_for_short_term_rps_in_slice = bits.bits;
            }
        }
        params.tool_flags = tool_flags(sps, pps);
        params.property_flags = pic_flags(picture, pps);
        params.pps_cb_qp_offset = pps.cb_qp_offset;
        params.pps_cr_qp_offset = pps.cr_qp_offset;
        params.diff_cu_qp_delta_depth = pps.diff_cu_qp_delta_depth;
        params.pps_beta_offset_div2 = pps.beta_offset_div2;
        params.pps_tc_offset_div2 = pps.tc_offset_div2;
        params.log2_parallel_merge_level_minus2 = pps.log2_parallel_merge_level_minus2;
        params.curr_pic_order_cnt_val = poc;
        fill_tiles(&mut params, pps);
        fill_ref_lists(&mut params, refs)?;
        if let Some(rps) = header.rps.as_ref() {
            fill_ref_sets(&mut params, rps, poc, refs)?;
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

/// True when stored `poc` sits in any short-term set against `current`.
fn short_term_member(poc: i32, rps: &ShortTermRps, current: i32) -> bool {
    rps.negative[..usize::from(rps.negative_count)]
        .iter()
        .chain(rps.positive[..usize::from(rps.positive_count)].iter())
        .any(|delta| delta.checked_add(current) == Some(poc))
}

/// Current-set POCs behind presence checks.
fn curr_pocs(rps: &ShortTermRps, current: i32) -> Vec<i32> {
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
    pocs
}

/// Packed coding tool flags.
fn tool_flags(sps: &Sps, pps: &Pps) -> u32 {
    let mut flags = 0u32;
    if sps.amp() {
        flags |= HEVC_TOOL_AMP;
    }
    if sps.sao() {
        flags |= HEVC_TOOL_SAO;
    }
    if sps.temporal_mvp() {
        flags |= HEVC_TOOL_TEMPORAL_MVP;
    }
    if sps.strong_intra_smoothing() {
        flags |= HEVC_TOOL_STRONG_INTRA;
    }
    if pps.dependent_slices() {
        flags |= HEVC_TOOL_DEPENDENT_SLICES;
    }
    if pps.output_flag_present() {
        flags |= HEVC_TOOL_OUTPUT_FLAG;
    }
    flags |= u32::from(pps.num_extra_slice_header_bits) << HEVC_TOOL_EXTRA_BITS_SHIFT;
    if pps.sign_data_hiding() {
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
        flags |= HEVC_PROP_CONSTRAINED_INTRA;
    }
    if pps.transform_skip() {
        flags |= HEVC_PROP_TRANSFORM_SKIP;
    }
    if pps.cu_qp_delta() {
        flags |= HEVC_PROP_CU_QP_DELTA;
    }
    if pps.slice_chroma_qp_offsets_present() {
        flags |= HEVC_PROP_CHROMA_QP_OFFSETS;
    }
    if pps.weighted_pred() {
        flags |= HEVC_PROP_WEIGHTED_PRED;
    }
    if pps.weighted_bipred() {
        flags |= HEVC_PROP_WEIGHTED_BIPRED;
    }
    if pps.transquant_bypass() {
        flags |= HEVC_PROP_TRANSQUANT_BYPASS;
    }
    if pps.tiles() {
        flags |= HEVC_PROP_TILES;
        if pps.loop_across_tiles() {
            flags |= HEVC_PROP_LOOP_TILES;
        }
    }
    if pps.entropy_sync() {
        flags |= HEVC_PROP_ENTROPY_SYNC;
    }
    if pps.uniform_spacing() {
        flags |= HEVC_PROP_UNIFORM_SPACING;
    }
    if pps.loop_across_slices() {
        flags |= HEVC_PROP_LOOP_SLICES;
    }
    if pps.deblocking_override() {
        flags |= HEVC_PROP_DEBLOCK_OVERRIDE;
    }
    if pps.deblocking_disabled() {
        flags |= HEVC_PROP_DEBLOCK_DISABLED;
    }
    if pps.lists_modification_present() {
        flags |= HEVC_PROP_LISTS_MOD;
    }
    if pps.slice_ext_present() {
        flags |= HEVC_PROP_SLICE_EXT;
    }
    if picture.irap {
        flags |= HEVC_IRAP_PIC | HEVC_INTRA_PIC;
    }
    if picture.idr {
        flags |= HEVC_IDR_PIC;
    }
    flags
}

/// Tile grid into the picture parameters.
fn fill_tiles(params: &mut PicParamsHevc, pps: &Pps) {
    if !pps.tiles() {
        return;
    }
    params.num_tile_columns_minus1 = pps.tile_columns_minus1;
    params.num_tile_rows_minus1 = pps.tile_rows_minus1;
    if pps.uniform_spacing() {
        return;
    }
    params.column_width_minus1 = pps.column_width_minus1;
    params.row_height_minus1 = pps.row_height_minus1;
}

/// Reference surfaces plus POCs compacted from the snapshot.
fn fill_ref_lists(params: &mut PicParamsHevc, refs: &[RefState]) -> Result<(), D3d12Error> {
    for (index, state) in refs.iter().enumerate().take(REF_LIST_LEN) {
        params.ref_pic_list[index] = PicEntryHevc::frame(slot_u8(state.slot)?);
        params.pic_order_cnt_val_list[index] = state.poc;
    }
    Ok(())
}

/// Reference set indices into the parameter reference list.
fn fill_ref_sets(
    params: &mut PicParamsHevc,
    rps: &ShortTermRps,
    poc: i32,
    refs: &[RefState],
) -> Result<(), D3d12Error> {
    let mut before = 0usize;
    for (index, delta) in rps.negative[..usize::from(rps.negative_count)]
        .iter()
        .enumerate()
    {
        if !rps.negative_used[index] || before >= params.ref_pic_set_st_curr_before.len() {
            continue;
        }
        let member = delta.checked_add(poc).ok_or({
            D3d12Error::Parse(ParseReason::Range {
                syntax: "delta_poc",
            })
        })?;
        params.ref_pic_set_st_curr_before[before] = pp_index(refs, member)?;
        before += 1;
    }
    let mut after = 0usize;
    for (index, delta) in rps.positive[..usize::from(rps.positive_count)]
        .iter()
        .enumerate()
    {
        if !rps.positive_used[index] || after >= params.ref_pic_set_st_curr_after.len() {
            continue;
        }
        let member = delta.checked_add(poc).ok_or({
            D3d12Error::Parse(ParseReason::Range {
                syntax: "delta_poc",
            })
        })?;
        params.ref_pic_set_st_curr_after[after] = pp_index(refs, member)?;
        after += 1;
    }
    Ok(())
}

/// Parameter position behind stored `poc`.
fn pp_index(refs: &[RefState], poc: i32) -> Result<u8, D3d12Error> {
    refs.iter()
        .position(|state| state.poc == poc)
        .and_then(|index| u8::try_from(index).ok())
        .ok_or(D3d12Error::BrokenReference(
            BrokenReferenceReason::ReferencePocAbsent { poc },
        ))
}

/// Submission plus display-order outputs.
#[derive(Debug)]
pub(crate) struct HevcDecoded {
    /// Driver submission.
    pub(crate) submission: HevcSubmission,
    /// Display-order outputs.
    pub(crate) outputs: Vec<Output>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::HevcDpb;
    use crate::dpb::Output;
    use crate::dxva::{HEVC_IDR_PIC, HEVC_INTRA_PIC, HEVC_IRAP_PIC};
    use crate::hevc::{
        PendingSlice, Picture, Pps, RpsBits, ShortTermRps, SliceHeader, SliceType, Sps, FLAG_SAO,
        FLAG_TEMPORAL_MVP,
    };
    use crate::pins::SlotPins;

    /// Sequence set with POC width 16 and reorder depth two.
    fn sps() -> Sps {
        Sps {
            seq_parameter_set_id: 0,
            width: 320,
            height: 240,
            crop: None,
            log2_max_poc_lsb_minus4: 0,
            max_dec_pic_buffering_minus1: 4,
            max_num_reorder_pics: 2,
            rps: Vec::new(),
            log2_min_cb_minus3: 0,
            log2_diff_max_min_cb: 3,
            log2_min_tb_minus2: 0,
            log2_diff_max_min_tb: 3,
            max_depth_inter: 1,
            max_depth_intra: 1,
            flags: FLAG_SAO | FLAG_TEMPORAL_MVP,
        }
    }

    /// Picture set with defaults.
    fn pps() -> Pps {
        Pps {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            flags: 0,
            num_extra_slice_header_bits: 0,
            num_ref_l0: 0,
            num_ref_l1: 0,
            init_qp_minus26: 0,
            diff_cu_qp_delta_depth: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            tile_columns_minus1: 0,
            tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            beta_offset_div2: 0,
            tc_offset_div2: 0,
            log2_parallel_merge_level_minus2: 0,
        }
    }

    /// Buffer over a fresh pin table.
    fn dpb(sps: &Sps) -> HevcDpb {
        HevcDpb::new(sps, Arc::new(SlotPins::new()))
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
                    pps_id: 0,
                    slice_type,
                    address: 0,
                    first: true,
                    pic_output: true,
                    no_output_prior: false,
                    poc_lsb,
                    rps,
                    rps_bits,
                },
                nal: vec![0x26, 0x01, 0x88],
                nal_type,
                temporal_id: 0,
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
        let mut dpb = dpb(&sps());
        let decoded = dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        assert!(decoded.outputs.is_empty());
        assert_eq!(decoded.submission.slot, 0);
        let params = &decoded.submission.params;
        assert_eq!(params.curr_pic_order_cnt_val, 0);
        assert_eq!(params.curr_pic.entry, 0);
        assert!(params.ref_pic_list.iter().all(|entry| entry.entry == 0xFF));
        assert_eq!(params.status_report_feedback_number, 1);
        assert_eq!(
            params.property_flags & (HEVC_IRAP_PIC | HEVC_IDR_PIC | HEVC_INTRA_PIC),
            HEVC_IRAP_PIC | HEVC_IDR_PIC | HEVC_INTRA_PIC
        );
        assert_eq!(params.pic_width_in_min_cbs_y, 40);
        assert_eq!(params.pic_height_in_min_cbs_y, 30);
        assert_eq!(params.num_short_term_ref_pic_sets, 0);
    }

    #[test]
    fn no_reorder_emits_per_call() {
        let mut sequence = sps();
        sequence.max_num_reorder_pics = 0;
        let mut dpb = dpb(&sequence);
        let decoded = dpb.decode(&idr(), &sequence, &pps()).expect("idr");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        let decoded = dpb.decode(&p(2, -2), &sequence, &pps()).expect("p");
        assert_eq!(slots(&decoded.outputs), vec![1]);
    }

    #[test]
    fn pinned_slot_skips_alloc() {
        let pins = Arc::new(SlotPins::new());
        let _held = pins.pin(0);
        let mut dpb = HevcDpb::new(&sps(), pins);
        let decoded = dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        assert_eq!(decoded.submission.slot, 1);
    }

    #[test]
    fn all_pinned_errors_no_free_slot() {
        let pins = Arc::new(SlotPins::new());
        let mut held = Vec::new();
        for slot in 0u8..16 {
            held.push(pins.pin(slot));
        }
        let mut dpb = HevcDpb::new(&sps(), pins);
        let error = dpb.decode(&idr(), &sps(), &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::NoFreeSlot));
        assert_eq!(error.to_string(), "no free surface slot");
    }

    #[test]
    fn p_references_idr() {
        let mut dpb = dpb(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(2, -2), &sps(), &pps()).expect("p");
        assert!(
            decoded.outputs.is_empty(),
            "reorder depth buffers the picture"
        );
        assert_eq!(decoded.submission.slot, 1);
        let params = &decoded.submission.params;
        assert_eq!(params.curr_pic_order_cnt_val, 2);
        assert_eq!(params.curr_pic.entry, 1);
        assert_eq!(params.ref_pic_list[0].entry, 0);
        assert_eq!(params.pic_order_cnt_val_list[0], 0);
        assert_eq!(params.ref_pic_set_st_curr_before[0], 0);
        assert_eq!(params.ref_pic_set_st_curr_before[1], 0xFF);
        assert_eq!(params.num_bits_for_short_term_rps_in_slice, 12);
        assert_eq!(params.status_report_feedback_number, 2);
        let decoded = dpb.decode(&p(4, -2), &sps(), &pps()).expect("p2");
        assert_eq!(slots(&decoded.outputs), vec![0]);
        assert_eq!(decoded.submission.slot, 2);
    }

    #[test]
    fn b_reorders_by_limit() {
        let mut dpb = dpb(&sps());
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
        assert_eq!(params.ref_pic_set_st_curr_before[0], 0);
        assert_eq!(params.ref_pic_set_st_curr_after[0], 1);
        assert_eq!(slots(&dpb.drain()), vec![2, 1]);
    }

    #[test]
    fn cra_keeps_following_refs() {
        let mut dpb = dpb(&sps());
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
        assert_eq!(decoded.submission.params.ref_pic_set_st_curr_before[0], 0);
        assert_eq!(slots(&dpb.drain()), vec![2, 0]);
    }

    #[test]
    fn bla_resets_poc_to_lsb() {
        let mut dpb = dpb(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        let decoded = dpb.decode(&bla(2), &sps(), &pps()).expect("bla");
        assert_eq!(slots(&decoded.outputs), vec![0, 1]);
        assert_eq!(decoded.submission.params.curr_pic_order_cnt_val, 2);
    }

    #[test]
    fn no_output_prior_drops_silently() {
        let mut dpb = dpb(&sps());
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
        let mut dpb = dpb(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let error = dpb.decode(&p(4, -2), &sps(), &pps()).unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d12Error::BrokenReference(_)
        ));
    }

    #[test]
    fn submission_carries_bitstream() {
        let mut dpb = dpb(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        let decoded = dpb.decode(&p(2, -2), &sps(), &pps()).expect("p");
        let submission = &decoded.submission;
        assert_eq!(submission.bitstream.len(), 128);
        assert_eq!(submission.bitstream[..6], [0, 0, 1, 0x26, 0x01, 0x88]);
        assert!(submission.bitstream[6..].iter().all(|byte| *byte == 0));
        assert_eq!(submission.qmatrix.lists0, [[16; 16]; 6]);
        assert_eq!(submission.qmatrix.dc3, [16; 2]);
        assert_eq!(submission.slices.len(), 1);
        let slice = &submission.slices[0];
        assert_eq!(slice.location, 0);
        assert_eq!(slice.bytes, 128);
        assert_eq!(slice.chopping, 0);
    }

    #[test]
    fn dpb_full_without_output() {
        let mut sequence = sps();
        sequence.max_dec_pic_buffering_minus1 = 0;
        let mut dpb = dpb(&sequence);
        dpb.decode(&idr(), &sequence, &pps()).expect("idr");
        let error = dpb.decode(&p(2, -2), &sequence, &pps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::DpbFull));
    }

    #[test]
    fn drain_emits_poc_order() {
        let mut dpb = dpb(&sps());
        dpb.decode(&idr(), &sps(), &pps()).expect("idr");
        dpb.decode(&p(4, -4), &sps(), &pps()).expect("p");
        assert_eq!(slots(&dpb.drain()), vec![0, 1]);
        assert!(dpb.drain().is_empty(), "drain runs once");
    }
}
