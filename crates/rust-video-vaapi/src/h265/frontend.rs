//! H.265 access-unit assembly, reference sets, and DPB orchestration.
//!
//! Portable: parses Annex B, groups slices per first-slice flags,
//! derives reference sets per 8.3.2, builds reference lists per 8.3.4,
//! and marks the DPB per 8.3.3 and C.5.2. A VA-API backend submits
//! finished pictures.

use std::borrow::Cow;
use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;

use crate::error::VaApiError;
use crate::frontend::SurfaceSlot;
use crate::h265::dpb::Dpb;
use crate::h265::parser::{
    Nalu, NaluHeader, NaluType, Parser, Pps, ShortTermRefPicSet, Slice, SliceHeader, Sps,
};
use crate::h265::picture::{PictureData, Reference};

use bytes::Bytes;

/// One coded slice: parsed header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSlice {
    /// Parsed slice header.
    header: SliceHeader,
    /// NAL unit type of the backing NAL.
    nalu_type: NaluType,
    /// Temporal id of the backing NAL.
    temporal_id: u8,
    /// NAL bytes past the start code, header bytes included.
    data: Bytes,
}

impl PendingSlice {
    /// Parsed slice header.
    #[must_use]
    pub fn header(&self) -> &SliceHeader {
        &self.header
    }

    /// NAL bytes past the start code, header bytes included.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Slices of one coded picture, not yet ordered against the DPB.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPicture {
    /// Active sequence set.
    sps: Rc<Sps>,
    /// Picture set the slices name.
    pps: Rc<Pps>,
    /// Slices in bitstream order.
    slices: Vec<PendingSlice>,
}

/// Reference lists of one slice: list 0 and list 1 as DPB indices.
type RefLists = (Vec<Option<usize>>, Vec<Option<usize>>);

/// Reference sets of one picture as DPB indices, per 8.3.2.
#[derive(Clone, Debug, Default)]
pub struct Rps {
    /// Short-term before, in RPS order; `None` names a missing reference.
    st_curr_before: Vec<Option<usize>>,
    /// Short-term after, in RPS order; `None` names a missing reference.
    st_curr_after: Vec<Option<usize>>,
    /// Long-term current, in RPS order; `None` names a missing reference.
    lt_curr: Vec<Option<usize>>,
}

impl Rps {
    /// Reference sets over explicit DPB indices.
    #[cfg(test)]
    pub(crate) fn from_lists(
        st_curr_before: Vec<Option<usize>>,
        st_curr_after: Vec<Option<usize>>,
        lt_curr: Vec<Option<usize>>,
    ) -> Self {
        Self {
            st_curr_before,
            st_curr_after,
            lt_curr,
        }
    }

    /// Short-term before, in RPS order.
    #[must_use]
    pub fn st_curr_before(&self) -> &[Option<usize>] {
        &self.st_curr_before
    }

    /// Short-term after, in RPS order.
    #[must_use]
    pub fn st_curr_after(&self) -> &[Option<usize>] {
        &self.st_curr_after
    }

    /// Long-term current, in RPS order.
    #[must_use]
    pub fn lt_curr(&self) -> &[Option<usize>] {
        &self.lt_curr
    }

    /// Reference lists for `header` per 8-8 through 8-11.
    ///
    /// I slices take empty lists; P slices take list 0 only.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a modification entry points outside the
    /// temporary list.
    pub fn lists(&self, header: &SliceHeader) -> Result<RefLists, VaApiError> {
        if header.type_.is_i() {
            return Ok((Vec::new(), Vec::new()));
        }
        let temp0 = self.temp_list0(header);
        let list0 = Self::modified(
            &temp0,
            header.ref_pic_list_modification.modification_flag_l0,
            &header.ref_pic_list_modification.list_entry_l0,
            header.num_ref_idx_l0_active_minus1,
        )?;
        if !header.type_.is_b() {
            return Ok((list0, Vec::new()));
        }
        let temp1 = self.temp_list1(header);
        let list1 = Self::modified(
            &temp1,
            header.ref_pic_list_modification.modification_flag_l1,
            &header.ref_pic_list_modification.list_entry_l1,
            header.num_ref_idx_l1_active_minus1,
        )?;
        Ok((list0, list1))
    }

    /// Temporary list 0 per 8-8.
    fn temp_list0(&self, header: &SliceHeader) -> Vec<Option<usize>> {
        let mut temp = Vec::new();
        let end = Self::temp_len(
            header.num_ref_idx_l0_active_minus1,
            header.num_pic_total_curr,
        );
        while temp.len() < end {
            let before = temp.len();
            temp.extend(self.st_curr_before.iter().copied().take(end - temp.len()));
            temp.extend(self.st_curr_after.iter().copied().take(end - temp.len()));
            temp.extend(self.lt_curr.iter().copied().take(end - temp.len()));
            if temp.len() == before {
                break;
            }
        }
        temp
    }

    /// Temporary list 1 per 8-10.
    fn temp_list1(&self, header: &SliceHeader) -> Vec<Option<usize>> {
        let mut temp = Vec::new();
        let end = Self::temp_len(
            header.num_ref_idx_l1_active_minus1,
            header.num_pic_total_curr,
        );
        while temp.len() < end {
            let before = temp.len();
            temp.extend(self.st_curr_after.iter().copied().take(end - temp.len()));
            temp.extend(self.st_curr_before.iter().copied().take(end - temp.len()));
            temp.extend(self.lt_curr.iter().copied().take(end - temp.len()));
            if temp.len() == before {
                break;
            }
        }
        temp
    }

    /// Temporary list length: active entries or the current count.
    fn temp_len(active_minus1: u8, total_curr: u32) -> usize {
        let active = usize::from(active_minus1) + 1;
        active.max(usize::try_from(total_curr).unwrap_or(usize::MAX))
    }

    /// Active entries of `temp`, reordered per modification entries.
    fn modified(
        temp: &[Option<usize>],
        modified: bool,
        entries: &[u32],
        active_minus1: u8,
    ) -> Result<Vec<Option<usize>>, VaApiError> {
        let end = usize::from(active_minus1) + 1;
        let mut list = Vec::with_capacity(end);
        for index in 0..end {
            if !modified {
                list.push(temp.get(index).copied().flatten());
                continue;
            }
            let at = usize::try_from(entries.get(index).copied().unwrap_or(u32::MAX))
                .unwrap_or(usize::MAX);
            let entry = temp.get(at).copied().ok_or_else(|| {
                VaApiError::BrokenReference(format!("list entry {at} outside temp list"))
            })?;
            list.push(entry);
        }
        Ok(list)
    }
}

/// Coded picture with order counts and reference sets.
pub struct Picture {
    /// Picture state: counts, output flags, marking.
    pic: PictureData,
    /// Active sequence set.
    sps: Rc<Sps>,
    /// Active picture set.
    pps: Rc<Pps>,
    /// Slices in bitstream order.
    slices: Vec<PendingSlice>,
    /// Reference sets as DPB indices.
    rps: Rps,
}

impl Picture {
    /// Picture state: counts, output flags, marking.
    #[must_use]
    pub fn pic(&self) -> &PictureData {
        &self.pic
    }

    /// Active sequence set.
    #[must_use]
    pub fn sps(&self) -> &Rc<Sps> {
        &self.sps
    }

    /// Active picture set.
    #[must_use]
    pub fn pps(&self) -> &Rc<Pps> {
        &self.pps
    }

    /// Slices in bitstream order.
    #[must_use]
    pub fn slices(&self) -> &[PendingSlice] {
        &self.slices
    }

    /// Reference sets as DPB indices.
    #[must_use]
    pub fn rps(&self) -> &Rps {
        &self.rps
    }
}

/// Pictures and drained slots from one ingest call.
pub struct Ingest {
    /// Pictures finished by the new bytes, in decode order.
    pictures: Vec<PendingPicture>,
    /// Slots drained by a mid-stream sequence change.
    drained: Vec<SurfaceSlot>,
    /// True when the sequence changed mid-stream.
    reconfigured: bool,
    /// Coded size the drained slots were decoded at.
    drained_size: Option<(u32, u32)>,
    /// Visible rectangle the drained slots were decoded at.
    drained_crop: Option<(u32, u32, u32, u32)>,
}

impl Ingest {
    /// Pictures finished by the new bytes, in decode order.
    pub fn pictures(&mut self) -> Vec<PendingPicture> {
        std::mem::take(&mut self.pictures)
    }

    /// Slots drained by a mid-stream sequence change.
    pub fn drained(&mut self) -> Vec<SurfaceSlot> {
        std::mem::take(&mut self.drained)
    }

    /// True when the sequence changed mid-stream.
    #[must_use]
    pub fn reconfigured(&self) -> bool {
        self.reconfigured
    }

    /// Coded size the drained slots were decoded at.
    #[must_use]
    pub fn drained_size(&self) -> Option<(u32, u32)> {
        self.drained_size
    }

    /// Visible rectangle the drained slots were decoded at.
    #[must_use]
    pub fn drained_crop(&self) -> Option<(u32, u32, u32, u32)> {
        self.drained_crop
    }
}

/// H.265 frontend: Annex B in, ordered pictures out.
pub struct Frontend {
    /// Bitstream parser with the active parameter sets.
    parser: Parser,
    /// Active sequence set.
    sps: Option<Rc<Sps>>,
    /// Decoded-picture buffer over surface slots.
    dpb: Dpb<SurfaceSlot>,
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
    /// Sets of the open picture.
    pending_sets: Option<(Rc<Sps>, Rc<Pps>)>,
    /// Last independent slice header, for dependent slices.
    last_independent: Option<SliceHeader>,
    /// Previous temporal-zero picture, for order counts.
    prev_tid0: Option<PictureData>,
    /// True before the first picture of the stream.
    first_in_bitstream: bool,
    /// True after an end-of-sequence NAL.
    first_after_eos: bool,
    /// `NoRaslOutputFlag` of the last IRAP picture.
    irap_no_rasl: bool,
    /// Order-count lsb range of the active sequence.
    max_poc_lsb: i32,
    /// Picture sets waiting on their sequence set.
    pending_pps: Vec<Nalu<'static>>,
}

impl Frontend {
    /// Empty frontend, no sequence active.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::default(),
            sps: None,
            dpb: Dpb::default(),
            pending: Vec::new(),
            pending_sets: None,
            last_independent: None,
            prev_tid0: None,
            first_in_bitstream: true,
            first_after_eos: true,
            irap_no_rasl: false,
            max_poc_lsb: 0,
            pending_pps: Vec::new(),
        }
    }

    /// Decoded-picture buffer.
    #[must_use]
    pub fn dpb(&self) -> &Dpb<SurfaceSlot> {
        &self.dpb
    }

    /// Coded picture size in luma samples, if a sequence is active.
    #[must_use]
    pub fn coded_size(&self) -> Option<(u32, u32)> {
        self.sps
            .as_ref()
            .map(|sps| (u32::from(sps.width()), u32::from(sps.height())))
    }

    /// Visible rectangle as `(x, y, width, height)`, if a sequence is active.
    #[must_use]
    pub fn visible_rect(&self) -> Option<(u32, u32, u32, u32)> {
        self.sps.as_ref().map(|sps| {
            let rect = sps.visible_rectangle();
            (
                rect.min.x,
                rect.min.y,
                rect.max.x - rect.min.x,
                rect.max.y - rect.min.y,
            )
        })
    }

    /// Active sequence set.
    #[must_use]
    pub fn sps(&self) -> Option<&Rc<Sps>> {
        self.sps.as_ref()
    }

    /// DPB frame capacity for the active sequence.
    #[must_use]
    pub fn max_dpb_frames(&self) -> Option<usize> {
        self.sps.as_ref().map(|sps| sps.max_dpb_size().min(16))
    }

    /// True when `index` backs a live DPB entry.
    #[must_use]
    pub fn slot_in_use(&self, index: u32) -> bool {
        self.dpb
            .entries()
            .iter()
            .any(|entry| entry.1.index() == index)
    }

    /// Pictures finished by Annex B `bytes`.
    ///
    /// Slice payloads sub-slice `bytes` without copying.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a NAL fails to parse, names an unknown
    /// parameter set, or layers the stream.
    pub fn ingest(&mut self, bytes: &Bytes) -> Result<Ingest, VaApiError> {
        let mut out = Ingest {
            pictures: Vec::new(),
            drained: Vec::new(),
            reconfigured: false,
            drained_size: None,
            drained_crop: None,
        };
        let mut cursor = Cursor::new(bytes.as_ref());
        loop {
            let Ok(nalu) = Nalu::next(&mut cursor) else {
                if Self::start_code_ahead(&cursor) {
                    return Err(VaApiError::Parse("broken nal unit".to_owned()));
                }
                break;
            };
            match nalu.header.type_ {
                NaluType::VpsNut => {
                    self.parser.parse_vps(&nalu).map_err(VaApiError::Parse)?;
                }
                NaluType::SpsNut => {
                    if self.adopt_sps(&nalu, &mut out)? {
                        out.reconfigured = true;
                    }
                }
                NaluType::PpsNut => {
                    if self.parser.parse_pps(&nalu).is_err() {
                        self.pending_pps.push(nalu.into_owned());
                    }
                }
                NaluType::BlaWLp
                | NaluType::BlaWRadl
                | NaluType::BlaNLp
                | NaluType::IdrWRadl
                | NaluType::IdrNLp
                | NaluType::TrailN
                | NaluType::TrailR
                | NaluType::TsaN
                | NaluType::TsaR
                | NaluType::StsaN
                | NaluType::StsaR
                | NaluType::RadlN
                | NaluType::RadlR
                | NaluType::RaslN
                | NaluType::RaslR
                | NaluType::CraNut => {
                    self.ingest_slice(nalu, bytes, &mut out.pictures)?;
                }
                NaluType::EosNut => {
                    self.first_after_eos = true;
                    if let Some(picture) = self.take_pending() {
                        out.pictures.push(picture);
                    }
                }
                NaluType::EobNut => {
                    self.first_in_bitstream = true;
                    if let Some(picture) = self.take_pending() {
                        out.pictures.push(picture);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Open picture, if any slices arrived.
    pub fn take_pending(&mut self) -> Option<PendingPicture> {
        if self.pending.is_empty() {
            return None;
        }
        let slices = std::mem::take(&mut self.pending);
        let (sps, pps) = self.pending_sets.take()?;
        Some(PendingPicture { sps, pps, slices })
    }

    /// Slots still held by the DPB, in output order.
    pub fn drain(&mut self) -> Vec<SurfaceSlot> {
        self.dpb.drain().into_iter().map(|entry| entry.1).collect()
    }

    /// Adopt `nalu` as the active sequence set.
    ///
    /// A changed set finishes the open picture, drains the DPB, and
    /// resets decode state; the return reports the change.
    fn adopt_sps(&mut self, nalu: &Nalu, out: &mut Ingest) -> Result<bool, VaApiError> {
        let id = self
            .parser
            .parse_sps(nalu)
            .map_err(VaApiError::Parse)?
            .seq_parameter_set_id;
        let sps = self
            .parser
            .get_sps(id)
            .cloned()
            .ok_or_else(|| VaApiError::Parse("sequence set vanished".to_owned()))?;
        self.max_poc_lsb = 1 << (u32::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4);
        for pps in std::mem::take(&mut self.pending_pps) {
            if self.parser.parse_pps(&pps).is_err() {
                self.pending_pps.push(pps);
            }
        }
        if self.sps.as_ref().is_some_and(|active| active == &sps) {
            return Ok(false);
        }
        if let Some(picture) = self.take_pending() {
            out.pictures.push(picture);
        }
        out.drained_size = self.coded_size();
        out.drained_crop = self.visible_rect();
        out.drained.extend(self.drain());
        self.dpb = Dpb::default();
        self.dpb.set_max_num_pics(sps.max_dpb_size().min(16));
        self.prev_tid0 = None;
        self.sps = Some(sps);
        Ok(true)
    }

    /// Append one slice NAL to the open picture.
    fn ingest_slice(
        &mut self,
        nalu: Nalu<'_>,
        bytes: &Bytes,
        pictures: &mut Vec<PendingPicture>,
    ) -> Result<(), VaApiError> {
        if nalu.header.nuh_layer_id != 0 {
            return Err(VaApiError::Unsupported("multilayer".to_owned()));
        }
        let nalu_type = nalu.header.type_;
        let temporal_id = nalu.header.nuh_temporal_id();
        let mut slice = self
            .parser
            .parse_slice_header(nalu)
            .map_err(VaApiError::Parse)?;
        if slice.header.position.dependent_slice_segment_flag {
            let independent = self.last_independent.clone().ok_or_else(|| {
                VaApiError::Parse("dependent slice without independent".to_owned())
            })?;
            slice
                .replace_header(independent)
                .map_err(VaApiError::Parse)?;
        } else {
            self.last_independent = Some(slice.header.clone());
        }
        let pps = self
            .parser
            .get_pps(slice.header.pic_parameter_set_id)
            .cloned()
            .ok_or_else(|| VaApiError::Parse("slice names unknown picture set".to_owned()))?;
        let sps = pps.sps.clone();
        if self.sps.as_ref() != Some(&sps) {
            return Err(VaApiError::Parse(
                "slice names inactive sequence set".to_owned(),
            ));
        }
        if !self.pending.is_empty() && slice.header.position.first_slice_segment_in_pic_flag {
            if let Some(picture) = self.take_pending() {
                pictures.push(picture);
            }
        }
        if self.pending.is_empty() {
            self.pending_sets = Some((sps, pps));
        }
        let payload = slice.nalu.as_ref();
        let base = bytes.as_ptr() as usize;
        let offset = payload.as_ptr() as usize - base;
        debug_assert!(offset + payload.len() <= bytes.len());
        let data = bytes.slice(offset..offset + payload.len());
        self.pending.push(PendingSlice {
            header: slice.header,
            nalu_type,
            temporal_id,
            data,
        });
        Ok(())
    }

    /// Order `pending` against the DPB: counts, sets, marking, bumps.
    ///
    /// Returns the begun picture plus pre-decode outputs, or `None`
    /// for a RASL picture the stream drops.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the picture leaves Main 8-bit 4:2:0 or the
    /// DPB cannot supply the picture order.
    pub fn begin(
        &mut self,
        pending: PendingPicture,
    ) -> Result<Option<(Picture, Vec<SurfaceSlot>)>, VaApiError> {
        let first = pending
            .slices
            .first()
            .ok_or_else(|| VaApiError::Parse("picture without slices".to_owned()))?;
        Self::check_supported(&pending.sps, &pending.pps)?;
        // `new_from_slice` reads only the NAL header below.
        let probe = Slice {
            header: first.header.clone(),
            nalu: Nalu {
                header: NaluHeader {
                    type_: first.nalu_type,
                    nuh_layer_id: 0,
                    nuh_temporal_id_plus1: first.temporal_id.saturating_add(1),
                },
                data: Cow::Borrowed(&[]),
                size: 0,
                offset: 0,
            },
        };
        let pic = PictureData::new_from_slice(
            &probe,
            self.first_in_bitstream,
            self.first_after_eos,
            self.prev_tid0.as_ref(),
            self.max_poc_lsb,
        );
        self.first_in_bitstream = false;
        self.first_after_eos = false;
        if pic.nalu_type.is_irap() {
            self.irap_no_rasl = pic.output.no_rasl_output_flag;
        } else if pic.nalu_type.is_rasl() && self.irap_no_rasl {
            return Ok(None);
        }
        let rps = self.decode_rps(&first.header, &pending.sps, &pic);
        let bumped = self.update_before_decoding(&pic, &pending.sps);
        Ok(Some((
            Picture {
                pic,
                sps: pending.sps,
                pps: pending.pps,
                slices: pending.slices,
                rps,
            },
            bumped,
        )))
    }

    /// Store `picture` on `slot`, returning post-decode outputs.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the DPB has no free slot for the picture.
    pub fn commit(
        &mut self,
        picture: Picture,
        slot: SurfaceSlot,
    ) -> Result<Vec<SurfaceSlot>, VaApiError> {
        if picture.pic.state.valid_for_prev_tid0_pic {
            self.prev_tid0 = Some(picture.pic.clone());
        }
        self.dpb
            .store_picture(Rc::new(RefCell::new(picture.pic)), slot)
            .map_err(|_| VaApiError::DpbFull)?;
        let mut outputs = Vec::new();
        while self.dpb.needs_additional_bumping(&picture.sps) {
            let Some(entry) = self.dpb.bump(false) else {
                break;
            };
            outputs.push(entry.1);
        }
        Ok(outputs)
    }

    /// Reference sets for `header`, marking the DPB per 8.3.3.
    fn decode_rps(&mut self, header: &SliceHeader, sps: &Sps, pic: &PictureData) -> Rps {
        if pic.nalu_type.is_irap() && pic.output.no_rasl_output_flag {
            self.dpb.mark_all_as_unused_for_ref();
        }
        let mut rps = Rps::default();
        if pic.nalu_type.is_idr() {
            return rps;
        }
        let set = Self::short_term_set(header, sps);
        let (st_before, st_after, st_foll) = short_term_pocs(set, pic.pic_order_cnt_val);
        let (lt_curr, lt_curr_masked, lt_foll, lt_foll_masked) =
            long_term_pocs(header, pic.pic_order_cnt_val, self.max_poc_lsb);
        rps.st_curr_before = self.resolve_short(&st_before);
        rps.st_curr_after = self.resolve_short(&st_after);
        rps.lt_curr = self.resolve_long(&lt_curr, &lt_curr_masked);
        let foll_short = self.resolve_short(&st_foll);
        let foll_long = self.resolve_long(&lt_foll, &lt_foll_masked);
        for index in rps.lt_curr.iter().chain(foll_long.iter()).flatten() {
            if let Some(entry) = self.dpb.entries().get(*index) {
                entry.0.borrow_mut().set_reference(Reference::LongTerm);
            }
        }
        let mut kept = Vec::new();
        kept.extend(rps.st_curr_before.iter().flatten());
        kept.extend(rps.st_curr_after.iter().flatten());
        kept.extend(rps.lt_curr.iter().flatten());
        kept.extend(foll_short.iter().flatten());
        kept.extend(foll_long.iter().flatten());
        for (index, entry) in self.dpb.entries().iter().enumerate() {
            if !kept.contains(&index) {
                entry.0.borrow_mut().set_reference(Reference::None);
            }
        }
        rps
    }

    /// Short-term set the header selects, inline or from the SPS.
    fn short_term_set<'a>(header: &'a SliceHeader, sps: &'a Sps) -> &'a ShortTermRefPicSet {
        if header.curr_rps_idx == sps.num_short_term_ref_pic_sets {
            &header.short_term_ref_pic_set
        } else {
            &sps.short_term_ref_pic_set[usize::from(header.curr_rps_idx)]
        }
    }

    /// DPB indices of short-term `pocs`, `None` where absent.
    fn resolve_short(&self, pocs: &[i32]) -> Vec<Option<usize>> {
        pocs.iter()
            .map(|poc| {
                self.dpb.find_short_term_ref_by_poc(*poc).and_then(|entry| {
                    self.dpb
                        .entries()
                        .iter()
                        .position(|stored| Rc::ptr_eq(&stored.0, &entry.0))
                })
            })
            .collect()
    }

    /// DPB indices of long-term `pocs`, masked where `masks` holds.
    fn resolve_long(&self, pocs: &[i32], masks: &[bool]) -> Vec<Option<usize>> {
        pocs.iter()
            .zip(masks.iter())
            .map(|(poc, masked)| {
                let entry = if *masked {
                    self.dpb.find_ref_by_poc_masked(*poc, self.max_poc_lsb - 1)
                } else {
                    self.dpb.find_ref_by_poc(*poc)
                };
                entry.and_then(|entry| {
                    self.dpb
                        .entries()
                        .iter()
                        .position(|stored| Rc::ptr_eq(&stored.0, &entry.0))
                })
            })
            .collect()
    }

    /// Bump or clear the DPB ahead of decoding `pic`.
    fn update_before_decoding(&mut self, pic: &PictureData, sps: &Sps) -> Vec<SurfaceSlot> {
        let mut outputs = Vec::new();
        if pic.nalu_type.is_irap() && pic.output.no_rasl_output_flag {
            if pic.output.no_output_of_prior_pics_flag {
                self.dpb.clear();
            } else {
                outputs.extend(self.drain());
            }
            return outputs;
        }
        self.dpb.remove_unused();
        while self.dpb.needs_bumping(sps) {
            let Some(entry) = self.dpb.bump(false) else {
                break;
            };
            outputs.push(entry.1);
        }
        outputs
    }

    /// Reject streams outside Main 8-bit 4:2:0 without extensions.
    fn check_supported(sps: &Sps, pps: &Pps) -> Result<(), VaApiError> {
        let profile = sps.profile_tier_level.general_profile_idc;
        if profile != 1 && profile != 3 {
            return Err(VaApiError::Unsupported(format!("h265 profile {profile}")));
        }
        if sps.chroma_format_idc != 1 {
            return Err(VaApiError::Unsupported(format!(
                "chroma {}",
                sps.chroma_format_idc
            )));
        }
        if sps.format.separate_colour_plane_flag {
            return Err(VaApiError::Unsupported("separate colour planes".to_owned()));
        }
        if sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
            return Err(VaApiError::Unsupported("high bit depth".to_owned()));
        }
        if sps.extensions.range_extension_flag || pps.extensions.range_extension_flag {
            return Err(VaApiError::Unsupported("range extension".to_owned()));
        }
        if sps.extensions.scc_extension_flag || pps.extensions.scc_extension_flag {
            return Err(VaApiError::Unsupported("screen content coding".to_owned()));
        }
        if sps.features.vui_parameters_present_flag && sps.vui_parameters.structure.field_seq_flag {
            return Err(VaApiError::Unsupported("interlaced".to_owned()));
        }
        Ok(())
    }

    /// True when a start code remains in `cursor`.
    fn start_code_ahead(cursor: &Cursor<&[u8]>) -> bool {
        let position = usize::try_from(cursor.position()).unwrap_or(usize::MAX);
        cursor
            .get_ref()
            .get(position..)
            .is_some_and(|tail| tail.windows(3).any(|window| window == [0x00, 0x00, 0x01]))
    }
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

/// Short-term POCs of `set` against `poc`: before, after, following.
fn short_term_pocs(set: &ShortTermRefPicSet, poc: i32) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut foll = Vec::new();
    for (index, used) in set
        .used_by_curr_pic_s0
        .iter()
        .enumerate()
        .take(usize::from(set.num_negative_pics))
    {
        let Some(delta) = set.delta_poc_s0.get(index) else {
            break;
        };
        let entry = poc.saturating_add(*delta);
        if *used {
            before.push(entry);
        } else {
            foll.push(entry);
        }
    }
    for (index, used) in set
        .used_by_curr_pic_s1
        .iter()
        .enumerate()
        .take(usize::from(set.num_positive_pics))
    {
        let Some(delta) = set.delta_poc_s1.get(index) else {
            break;
        };
        let entry = poc.saturating_add(*delta);
        if *used {
            after.push(entry);
        } else {
            foll.push(entry);
        }
    }
    (before, after, foll)
}

/// Long-term POCs of `header` against `poc`: current, masked, following, masked.
fn long_term_pocs(
    header: &SliceHeader,
    poc: i32,
    max_lsb: i32,
) -> (Vec<i32>, Vec<bool>, Vec<i32>, Vec<bool>) {
    let mut curr = Vec::new();
    let mut curr_masked = Vec::new();
    let mut foll = Vec::new();
    let mut foll_masked = Vec::new();
    let count = usize::from(
        header
            .num_long_term_sps
            .saturating_add(header.num_long_term_pics),
    );
    for index in 0..count {
        let (Some(lsb), Some(used), Some(msb), Some(cycle)) = (
            header.poc_lsb_lt.get(index),
            header.used_by_curr_pic_lt.get(index),
            header.delta_poc_msb_present_flag.get(index),
            header.delta_poc_msb_cycle_lt.get(index),
        ) else {
            break;
        };
        let mut entry = i64::from(*lsb);
        if *msb {
            entry += i64::from(poc);
            entry -= i64::from(*cycle) * i64::from(max_lsb);
            entry -= i64::from(poc & (max_lsb - 1));
        }
        let entry = i32::try_from(entry).unwrap_or(i32::MAX);
        if *used {
            curr.push(entry);
            curr_masked.push(!msb);
        } else {
            foll.push(entry);
            foll_masked.push(!msb);
        }
    }
    (curr, curr_masked, foll, foll_masked)
}

#[cfg(test)]
mod tests {
    use super::{long_term_pocs, short_term_pocs, Frontend};
    use crate::error::VaApiError;
    use crate::h265::fixture::{idr_payload, nal, p_payload, pps_payload, sps_payload, Writer};
    use crate::h265::parser::{
        NaluType, RefPicListModification, ShortTermRefPicSet, SliceHeader, SliceType,
    };

    use bytes::Bytes;

    /// Frontend that ingested the sequence and picture sets.
    fn sequenced() -> Frontend {
        let mut frontend = Frontend::new();
        let mut stream = nal(NaluType::SpsNut, &sps_payload());
        stream.extend(nal(NaluType::PpsNut, &pps_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("sets parse");
        assert_eq!(ingest.pictures(), []);
        assert_eq!(frontend.coded_size(), Some((64, 64)));
        frontend
    }

    #[test]
    fn ingest_groups_slices_into_pictures() {
        let mut frontend = sequenced();
        let mut stream = nal(NaluType::IdrWRadl, &idr_payload());
        stream.extend(nal(NaluType::TrailR, &p_payload(2, 1)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let pictures = ingest.pictures();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].slices.len(), 1);
        assert!(pictures[0].slices[0].header.type_.is_i());
        let pending = frontend.take_pending().expect("open p picture");
        assert!(pending.slices[0].header.type_.is_p());
        assert!(frontend.take_pending().is_none());
    }

    #[test]
    fn begin_orders_pocs_and_resolves_references() {
        let mut frontend = sequenced();
        let mut stream = nal(NaluType::IdrWRadl, &idr_payload());
        stream.extend(nal(NaluType::TrailR, &p_payload(2, 1)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let idr = ingest.pictures().pop().expect("idr picture");
        let (picture, bumped) = frontend.begin(idr).expect("idr begins").expect("idr kept");
        assert_eq!(bumped, []);
        assert_eq!(picture.pic().pic_order_cnt_val, 0);
        assert_eq!(picture.rps().st_curr_before(), []);
        let outputs = frontend
            .commit(picture, crate::frontend::SurfaceSlot::new(0))
            .expect("idr commits");
        assert_eq!(outputs, []);
        let pending = frontend.take_pending().expect("open p picture");
        let (picture, bumped) = frontend.begin(pending).expect("p begins").expect("p kept");
        assert_eq!(bumped, []);
        assert_eq!(picture.pic().pic_order_cnt_val, 2);
        assert_eq!(picture.rps().st_curr_before(), &[Some(0)]);
        let (list0, list1) = picture
            .rps()
            .lists(picture.slices()[0].header())
            .expect("p lists");
        assert_eq!(list0, vec![Some(0)]);
        assert_eq!(list1, []);
        let outputs = frontend
            .commit(picture, crate::frontend::SurfaceSlot::new(1))
            .expect("p commits");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].index(), 0);
    }

    #[test]
    fn begin_drops_rasl_after_irap() {
        let mut frontend = sequenced();
        let mut stream = nal(NaluType::IdrWRadl, &idr_payload());
        stream.extend(nal(NaluType::RaslR, &p_payload(1, 0)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let idr = ingest.pictures().pop().expect("idr picture");
        let (picture, _) = frontend.begin(idr).expect("idr begins").expect("idr kept");
        frontend
            .commit(picture, crate::frontend::SurfaceSlot::new(0))
            .expect("idr commits");
        let pending = frontend.take_pending().expect("open rasl picture");
        assert!(frontend.begin(pending).expect("rasl begins").is_none());
    }

    #[test]
    fn picture_set_before_sequence_set_recovers() {
        let mut frontend = Frontend::new();
        let mut stream = nal(NaluType::PpsNut, &pps_payload());
        stream.extend(nal(NaluType::SpsNut, &sps_payload()));
        stream.extend(nal(NaluType::IdrWRadl, &idr_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("early pps recovers");
        assert_eq!(ingest.pictures().len(), 0);
        assert!(frontend.take_pending().is_some());
    }

    #[test]
    fn layered_slice_declines_typed() {
        let mut frontend = sequenced();
        let mut slice = vec![0, 0, 0, 1, (NaluType::TrailR as u8) << 1, 0x09];
        slice.extend(p_payload(2, 1));
        let bytes = Bytes::from(slice);
        let Err(VaApiError::Unsupported(message)) = frontend.ingest(&bytes) else {
            panic!("layered slice declines");
        };
        assert!(message.contains("multilayer"));
    }

    #[test]
    fn dependent_slice_without_independent_fails() {
        let mut frontend = Frontend::new();
        let mut stream = nal(NaluType::SpsNut, &sps_payload());
        let mut pps = Writer::new();
        pps.ue(0);
        pps.ue(0);
        pps.bit(true);
        pps.bit(false);
        pps.bits(0, 3);
        pps.bit(false);
        pps.bit(false);
        pps.ue(0);
        pps.ue(0);
        pps.se(0);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.se(0);
        pps.se(0);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.bit(false);
        pps.ue(0);
        pps.bit(false);
        pps.bit(false);
        stream.extend(nal(NaluType::PpsNut, &pps.bytes()));
        let mut slice = Writer::new();
        slice.bit(false);
        slice.ue(0);
        slice.bit(true);
        slice.se(0);
        stream.extend(nal(NaluType::TrailR, &slice.bytes()));
        let bytes = Bytes::from(stream);
        let Err(VaApiError::Parse(message)) = frontend.ingest(&bytes) else {
            panic!("orphan dependent slice fails");
        };
        assert!(message.contains("dependent"));
    }

    #[test]
    fn garbage_sequence_set_fails() {
        let mut frontend = Frontend::new();
        let stream = nal(NaluType::SpsNut, &[0x00]);
        let bytes = Bytes::from(stream);
        assert!(matches!(frontend.ingest(&bytes), Err(VaApiError::Parse(_))));
    }

    #[test]
    fn sequence_change_drains_and_reconfigures() {
        let mut frontend = sequenced();
        let stream = nal(NaluType::IdrWRadl, &idr_payload());
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("idr parses");
        assert_eq!(ingest.pictures(), []);
        let idr = frontend.take_pending().expect("open idr");
        let (picture, _) = frontend.begin(idr).expect("begins").expect("kept");
        frontend
            .commit(picture, crate::frontend::SurfaceSlot::new(3))
            .expect("commits");
        let mut stream = nal(NaluType::SpsNut, &sps_payload());
        stream.extend(nal(NaluType::PpsNut, &pps_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("same sets parse");
        assert!(!ingest.reconfigured());
        assert_eq!(ingest.drained(), []);
        let mut writer = Writer::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.bits(0, 2);
        writer.bit(false);
        writer.bits(1, 5);
        for index in 0..32 {
            writer.bit(index == 1);
        }
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bits(0, 31);
        writer.bits(0, 12);
        writer.bit(false);
        writer.bits(120, 8);
        writer.ue(1);
        writer.ue(1);
        writer.ue(128);
        writer.ue(128);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(4);
        writer.bit(true);
        writer.ue(1);
        writer.ue(1);
        writer.ue(0);
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        let stream = nal(NaluType::SpsNut, &writer.bytes());
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("new set parses");
        assert!(ingest.reconfigured());
        assert_eq!(ingest.drained_size(), Some((64, 64)));
        let drained = ingest.drained();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].index(), 3);
        assert_eq!(frontend.coded_size(), Some((128, 128)));
    }

    #[test]
    fn check_supported_declines_foreign_streams() {
        let frontend = sequenced();
        let sps = frontend.sps().expect("active set").clone();
        let pps = frontend.parser.get_pps(0).expect("parsed set").clone();
        let mut bad = (*sps).clone();
        bad.profile_tier_level.general_profile_idc = 2;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.chroma_format_idc = 0;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.bit_depth_luma_minus8 = 2;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.extensions.scc_extension_flag = true;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*pps).clone();
        bad.extensions.range_extension_flag = true;
        assert!(matches!(
            Frontend::check_supported(&sps, &bad),
            Err(VaApiError::Unsupported(_))
        ));
        assert!(Frontend::check_supported(&sps, &pps).is_ok());
    }

    #[test]
    fn short_term_pocs_split_current_and_following() {
        let mut set = ShortTermRefPicSet {
            num_negative_pics: 2,
            num_positive_pics: 1,
            ..ShortTermRefPicSet::default()
        };
        set.delta_poc_s0[0] = -1;
        set.delta_poc_s0[1] = -3;
        set.delta_poc_s1[0] = 2;
        set.used_by_curr_pic_s0[0] = true;
        set.used_by_curr_pic_s0[1] = false;
        set.used_by_curr_pic_s1[0] = true;
        let (before, after, foll) = short_term_pocs(&set, 10);
        assert_eq!(before, vec![9]);
        assert_eq!(after, vec![12]);
        assert_eq!(foll, vec![7]);
    }

    #[test]
    fn long_term_poc_applies_msb_cycle() {
        let mut header = SliceHeader {
            num_long_term_pics: 2,
            ..SliceHeader::default()
        };
        header.poc_lsb_lt[0] = 4;
        header.poc_lsb_lt[1] = 9;
        header.used_by_curr_pic_lt[0] = true;
        header.used_by_curr_pic_lt[1] = true;
        header.delta_poc_msb_present_flag[0] = true;
        header.delta_poc_msb_cycle_lt[0] = 1;
        let (curr, masked, foll, _) = long_term_pocs(&header, 260, 256);
        assert_eq!(curr, vec![4, 9]);
        assert_eq!(masked, vec![false, true]);
        assert_eq!(foll, []);
    }

    #[test]
    fn rps_lists_follow_temporal_order() {
        use super::Rps;

        let rps = Rps {
            st_curr_before: vec![Some(0)],
            st_curr_after: vec![Some(1)],
            lt_curr: vec![Some(2)],
        };
        let mut header = SliceHeader {
            type_: SliceType::P,
            num_ref_idx_l0_active_minus1: 2,
            num_pic_total_curr: 3,
            ..SliceHeader::default()
        };
        let (list0, list1) = rps.lists(&header).expect("p lists");
        assert_eq!(list0, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(list1, []);
        header.type_ = SliceType::B;
        header.num_ref_idx_l1_active_minus1 = 2;
        let (list0, list1) = rps.lists(&header).expect("b lists");
        assert_eq!(list0, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(list1, vec![Some(1), Some(0), Some(2)]);
        header.type_ = SliceType::I;
        let (list0, list1) = rps.lists(&header).expect("i lists");
        assert_eq!(list0, []);
        assert_eq!(list1, []);
    }

    #[test]
    fn rps_lists_apply_modification() {
        use super::Rps;

        let rps = Rps {
            st_curr_before: vec![Some(0), Some(1)],
            st_curr_after: vec![],
            lt_curr: vec![Some(2)],
        };
        let mut header = SliceHeader {
            type_: SliceType::P,
            num_ref_idx_l0_active_minus1: 1,
            num_pic_total_curr: 3,
            ref_pic_list_modification: RefPicListModification {
                modification_flag_l0: true,
                list_entry_l0: vec![2, 0],
                ..RefPicListModification::default()
            },
            ..SliceHeader::default()
        };
        let (list0, _) = rps.lists(&header).expect("modified lists");
        assert_eq!(list0, vec![Some(2), Some(0)]);
        header.ref_pic_list_modification.list_entry_l0 = vec![9, 0];
        assert!(matches!(
            rps.lists(&header),
            Err(VaApiError::BrokenReference(_))
        ));
    }
}
