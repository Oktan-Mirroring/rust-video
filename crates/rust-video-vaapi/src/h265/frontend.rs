//! H.265 access-unit assembly, reference sets, and DPB orchestration.
//!
//! Portable: parses Annex B, groups slices per first-slice flags,
//! derives reference sets per 8.3.2, builds reference lists per 8.3.4,
//! and marks the DPB per 8.3.3 and C.5.2. A VA-API backend submits
//! finished pictures.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::error::VaApiError;
use crate::h264::frontend::SurfaceSlot;
use crate::h265::dpb::Dpb;
use crate::h265::picture::{PictureData, Reference};
use crate::h265::{
    is_idr, is_irap, is_rasl, parse_pps, parse_pps_ids, parse_slice_prefix, parse_slice_rest,
    parse_slice_tail, parse_sps, parse_vps, rbsp, split_nals, BitReader, Gate, LtSpec, Pps,
    ShortTermRps, SliceHeader, SliceTail, Sps,
};

use bytes::Bytes;
use rust_video_caps::reason::{
    BrokenReferenceReason as CapsBroken, ParseReason as CapsParse,
    UnsupportedReason as CapsUnsupported,
};

/// One coded slice: parsed header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingSlice {
    /// Parsed slice header.
    header: SliceHeader,
    /// Parsed slice tail.
    tail: SliceTail,
    /// NAL unit type of the backing NAL.
    nalu_type: u8,
    /// Temporal id of the backing NAL.
    temporal_id: u8,
    /// NAL bytes past the start code, header bytes included.
    data: Bytes,
    /// Emulation prevention bytes inside the slice header.
    epb: u32,
}

impl PendingSlice {
    /// Parsed slice header.
    #[must_use]
    pub fn header(&self) -> &SliceHeader {
        &self.header
    }

    /// Parsed slice tail.
    #[must_use]
    pub fn tail(&self) -> &SliceTail {
        &self.tail
    }

    /// NAL bytes past the start code, header bytes included.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Slice header size in bits, NAL header included.
    #[must_use]
    pub fn bit_size(&self) -> usize {
        self.tail.bit_size
    }

    /// Emulation prevention bytes inside the slice header.
    #[must_use]
    pub fn epb(&self) -> u32 {
        self.epb
    }

    /// Assembled slice for mapping tests.
    #[cfg(test)]
    pub(crate) fn assembled(
        header: SliceHeader,
        tail: SliceTail,
        data_len: usize,
        epb: u32,
    ) -> Self {
        Self {
            header,
            tail,
            nalu_type: 1,
            temporal_id: 0,
            data: bytes::Bytes::from(vec![0xAA; data_len]),
            epb,
        }
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
#[cfg(test)]
type RefLists = (Vec<Option<usize>>, Vec<Option<usize>>);

/// Pre-bump reference handles retained across DPB pruning.
#[derive(Default)]
struct KeptRps {
    /// Short-term before handles in RPS order.
    st_curr_before: Vec<Option<Rc<RefCell<PictureData>>>>,
    /// Short-term after handles in RPS order.
    st_curr_after: Vec<Option<Rc<RefCell<PictureData>>>>,
    /// Long-term current handles in RPS order.
    lt_curr: Vec<Option<Rc<RefCell<PictureData>>>>,
}

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
    #[cfg(test)]
    pub fn lists(&self, header: &SliceHeader, tail: &SliceTail) -> Result<RefLists, VaApiError> {
        let mut list0 = Vec::new();
        let mut list1 = Vec::new();
        let mut temp = Vec::new();
        self.lists_into(header, tail, &mut list0, &mut list1, &mut temp)?;
        Ok((list0, list1))
    }

    /// Reference lists for `header`, rebuilt in `list0`/`list1`.
    ///
    /// `temp` stages one temporary list at a time.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a modification entry points outside the
    /// temporary list.
    pub fn lists_into(
        &self,
        header: &SliceHeader,
        tail: &SliceTail,
        list0: &mut Vec<Option<usize>>,
        list1: &mut Vec<Option<usize>>,
        temp: &mut Vec<Option<usize>>,
    ) -> Result<(), VaApiError> {
        if header.slice_type.is_intra() {
            list0.clear();
            list1.clear();
            return Ok(());
        }
        let total_curr = Self::total_curr(header, tail);
        self.temp_list0_into(tail.num_ref_l0, total_curr, temp);
        Self::modified_into(
            temp,
            tail.mod_flag_l0,
            &tail.mod_entries_l0,
            tail.num_ref_l0,
            list0,
        )?;
        if !header.slice_type.is_b() {
            list1.clear();
            return Ok(());
        }
        self.temp_list1_into(tail.num_ref_l1, total_curr, temp);
        Self::modified_into(
            temp,
            tail.mod_flag_l1,
            &tail.mod_entries_l1,
            tail.num_ref_l1,
            list1,
        )
    }

    /// Current-picture reference count: used short-term plus used long-term.
    fn total_curr(header: &SliceHeader, tail: &SliceTail) -> u32 {
        let short = header
            .rps
            .as_ref()
            .map_or(0, |set| set.used_negative() + set.used_positive());
        let long = tail.lt.iter().filter(|pic| pic.used).count();
        u32::try_from(short + long).unwrap_or(u32::MAX)
    }

    /// Temporary list 0 per 8-8, rebuilt in `out`.
    fn temp_list0_into(&self, active_minus1: u8, total_curr: u32, out: &mut Vec<Option<usize>>) {
        out.clear();
        let end = Self::temp_len(active_minus1, total_curr);
        while out.len() < end {
            let before = out.len();
            out.extend(self.st_curr_before.iter().copied().take(end - out.len()));
            out.extend(self.st_curr_after.iter().copied().take(end - out.len()));
            out.extend(self.lt_curr.iter().copied().take(end - out.len()));
            if out.len() == before {
                break;
            }
        }
    }

    /// Temporary list 1 per 8-10, rebuilt in `out`.
    fn temp_list1_into(&self, active_minus1: u8, total_curr: u32, out: &mut Vec<Option<usize>>) {
        out.clear();
        let end = Self::temp_len(active_minus1, total_curr);
        while out.len() < end {
            let before = out.len();
            out.extend(self.st_curr_after.iter().copied().take(end - out.len()));
            out.extend(self.st_curr_before.iter().copied().take(end - out.len()));
            out.extend(self.lt_curr.iter().copied().take(end - out.len()));
            if out.len() == before {
                break;
            }
        }
    }

    /// Temporary list length: active entries or the current count.
    fn temp_len(active_minus1: u8, total_curr: u32) -> usize {
        let active = usize::from(active_minus1) + 1;
        active.max(usize::try_from(total_curr).unwrap_or(usize::MAX))
    }

    /// Active entries of `temp`, reordered per modification entries, rebuilt in `out`.
    fn modified_into(
        temp: &[Option<usize>],
        modified: bool,
        entries: &[u32],
        active_minus1: u8,
        out: &mut Vec<Option<usize>>,
    ) -> Result<(), VaApiError> {
        out.clear();
        let end = usize::from(active_minus1) + 1;
        out.reserve(end);
        for index in 0..end {
            if !modified {
                out.push(temp.get(index).copied().flatten());
                continue;
            }
            let at = usize::try_from(entries.get(index).copied().unwrap_or(u32::MAX))
                .unwrap_or(usize::MAX);
            let entry = temp
                .get(at)
                .copied()
                .ok_or(VaApiError::BrokenReference(CapsBroken::MarkingTargetAbsent))?;
            out.push(entry);
        }
        Ok(())
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
    /// Stored sequence sets by id.
    sps_store: BTreeMap<u8, Rc<Sps>>,
    /// Stored picture sets by id.
    pps_store: BTreeMap<u8, Rc<Pps>>,
    /// Active sequence set.
    sps: Option<Rc<Sps>>,
    /// Decoded-picture buffer over surface slots.
    dpb: Dpb<SurfaceSlot>,
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
    /// Sets of the open picture.
    pending_sets: Option<(Rc<Sps>, Rc<Pps>)>,
    /// Previous temporal-zero POC, for order counts.
    prev_tid0: Option<i32>,
    /// True before the first picture of the stream.
    first_in_bitstream: bool,
    /// True after an end-of-sequence NAL.
    first_after_eos: bool,
    /// `NoRaslOutputFlag` of the last IRAP picture.
    irap_no_rasl: bool,
    /// Order-count lsb range of the active sequence.
    max_poc_lsb: i32,
    /// Picture sets waiting on their sequence set.
    pending_pps: Vec<Vec<u8>>,
}

impl Frontend {
    /// Empty frontend, no sequence active.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sps_store: BTreeMap::new(),
            pps_store: BTreeMap::new(),
            sps: None,
            dpb: Dpb::default(),
            pending: Vec::new(),
            pending_sets: None,
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
        self.sps.as_ref().map(|sps| (sps.width, sps.height))
    }

    /// Visible rectangle as `(x, y, width, height)`, if a sequence is active.
    #[must_use]
    pub fn visible_rect(&self) -> Option<(u32, u32, u32, u32)> {
        self.sps.as_ref().map(|sps| Self::visible_rect_of(sps))
    }

    /// Active sequence set.
    #[must_use]
    pub fn sps(&self) -> Option<&Rc<Sps>> {
        self.sps.as_ref()
    }

    /// DPB frame capacity for the active sequence.
    #[must_use]
    pub fn max_dpb_frames(&self) -> Option<usize> {
        self.sps
            .as_ref()
            .map(|sps| Self::max_dpb_size_of(sps).min(16))
    }

    /// Picture set `id`, if stored.
    #[cfg(test)]
    pub(crate) fn pps(&self, id: u8) -> Option<&Rc<Pps>> {
        self.pps_store.get(&id)
    }

    /// Visible rectangle of `sps` as `(x, y, width, height)`.
    fn visible_rect_of(sps: &Sps) -> (u32, u32, u32, u32) {
        let Some(crop) = sps.crop else {
            return (0, 0, sps.width, sps.height);
        };
        (
            crop.left,
            crop.top,
            sps.width - crop.left - crop.right,
            sps.height - crop.top - crop.bottom,
        )
    }

    /// DPB picture capacity for `sps`, H.265 A.4.2 equation A-2.
    fn max_dpb_size_of(sps: &Sps) -> usize {
        // Table A.8 luma-sample ceilings by level.
        let max_luma_ps: u32 = match sps.level_idc {
            30 => 36_864,
            60 => 122_880,
            63 => 245_760,
            90 => 552_960,
            93 => 983_040,
            120 | 123 => 2_228_224,
            150 | 153 | 156 => 8_912_896,
            _ => 35_651_584,
        };
        let max_dpb_pic_buf: u32 = if (1..=5).contains(&sps.ptl.profile_idc) {
            6
        } else {
            7
        };
        let samples = sps.width.saturating_mul(sps.height);
        let max = if samples <= max_luma_ps >> 2 {
            std::cmp::min(4 * max_dpb_pic_buf, 16)
        } else if samples <= max_luma_ps >> 1 {
            std::cmp::min(2 * max_dpb_pic_buf, 16)
        } else if samples <= (3 * max_luma_ps) >> 2 {
            std::cmp::min(4 * max_dpb_pic_buf / 3, 16)
        } else {
            max_dpb_pic_buf
        };
        usize::try_from(max).unwrap_or(usize::MAX)
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
        for nal in split_nals(bytes.as_ref()) {
            let Some(header) = nal.get(0..2) else {
                continue;
            };
            let body = &nal[2..];
            match crate::h265::nal_unit_type(header[0]) {
                32 => {
                    let rbsp = rbsp(body);
                    parse_vps::<VaApiError>(&rbsp)?;
                }
                33 => {
                    if self.adopt_sps(body, &mut out)? {
                        out.reconfigured = true;
                    }
                }
                34 => {
                    if self.adopt_pps(body).is_err() {
                        self.pending_pps.push(nal.to_vec());
                    }
                }
                0..=9 | 16..=21 => {
                    self.ingest_slice(nal, body, header, bytes, &mut out.pictures)?;
                }
                36 => {
                    self.first_after_eos = true;
                    if let Some(picture) = self.take_pending() {
                        out.pictures.push(picture);
                    }
                }
                37 => {
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

    /// Adopt RBSP `body` as the active sequence set.
    ///
    /// A changed set finishes the open picture, drains the DPB, and
    /// resets decode state; the return reports the change.
    fn adopt_sps(&mut self, body: &[u8], out: &mut Ingest) -> Result<bool, VaApiError> {
        let rbsp = rbsp(body);
        let parsed = parse_sps::<VaApiError, Gate>(&rbsp)?;
        let id = parsed.id;
        if self.sps_store.keys().len() >= 16 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "sps_count",
            }));
        }
        let sps = Rc::new(parsed);
        self.sps_store.insert(id, Rc::clone(&sps));
        self.max_poc_lsb = 1 << (u32::from(sps.log2_max_poc_lsb_minus4) + 4);
        for pps in std::mem::take(&mut self.pending_pps) {
            let Some(body) = pps.get(2..) else {
                continue;
            };
            if self.adopt_pps(body).is_err() {
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
        self.dpb
            .set_max_num_pics(Self::max_dpb_size_of(&sps).min(16));
        self.prev_tid0 = None;
        self.sps = Some(sps);
        Ok(true)
    }

    /// Store the picture set in RBSP `body`.
    fn adopt_pps(&mut self, body: &[u8]) -> Result<(), VaApiError> {
        let rbsp = rbsp(body);
        let (pps_id, sps_id) = parse_pps_ids::<VaApiError>(&rbsp)?;
        if pps_id > 63 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "pps_id" }));
        }
        if sps_id > 15 {
            return Err(VaApiError::Parse(CapsParse::Range { syntax: "sps_id" }));
        }
        let sps = self
            .sps_store
            .get(&sps_id)
            .ok_or(VaApiError::Parse(CapsParse::PpsCitesUnknownSps))?;
        let parsed = parse_pps::<VaApiError, Gate>(&rbsp, Some(sps))?;
        let id = parsed.id;
        if self.pps_store.keys().len() >= 64 {
            return Err(VaApiError::Parse(CapsParse::Range {
                syntax: "pps_count",
            }));
        }
        self.pps_store.insert(id, Rc::new(parsed));
        Ok(())
    }

    /// Append one slice NAL to the open picture.
    fn ingest_slice(
        &mut self,
        nal: &[u8],
        body: &[u8],
        header: &[u8],
        bytes: &Bytes,
        pictures: &mut Vec<PendingPicture>,
    ) -> Result<(), VaApiError> {
        let layer_id = (u16::from(header[0] & 1) << 5) | (u16::from(header[1]) >> 3);
        if layer_id != 0 {
            return Err(VaApiError::Unsupported(CapsUnsupported::Multilayer));
        }
        let nalu_type = crate::h265::nal_unit_type(header[0]);
        let temporal_id = crate::h265::temporal_id(header[1]);
        let rbsp = rbsp(body);
        let mut reader = BitReader::new(&rbsp);
        let prefix = parse_slice_prefix::<VaApiError>(&mut reader, nalu_type)?;
        let pps_id = prefix.2;
        let pps = self
            .pps_store
            .get(&pps_id)
            .cloned()
            .ok_or(VaApiError::Parse(CapsParse::SliceCitesUnknownPps))?;
        let sps = self
            .sps_store
            .get(&pps.sps_id)
            .cloned()
            .ok_or(VaApiError::Parse(CapsParse::PpsCitesUnknownSps))?;
        let parsed = parse_slice_rest::<VaApiError>(&mut reader, prefix, nalu_type, &sps, &pps)?;
        let tail = parse_slice_tail::<VaApiError>(&rbsp, nalu_type, &sps, &pps)?;
        if self.sps.as_ref() != Some(&sps) {
            return Err(VaApiError::Parse(CapsParse::SliceCitesUnknownSps));
        }
        if !self.pending.is_empty() && parsed.first {
            if let Some(picture) = self.take_pending() {
                pictures.push(picture);
            }
        }
        if self.pending.is_empty() {
            self.pending_sets = Some((sps, pps));
        }
        let mut payload = nal;
        while payload.last().is_some_and(|byte| *byte == 0) {
            payload = &payload[..payload.len() - 1];
        }
        let base = bytes.as_ptr() as usize;
        let offset = payload.as_ptr() as usize - base;
        debug_assert!(offset + payload.len() <= bytes.len());
        let data = bytes.slice(offset..offset + payload.len());
        let epb = Self::count_epb(body, tail.bit_size.saturating_sub(16));
        self.pending.push(PendingSlice {
            header: parsed,
            tail,
            nalu_type,
            temporal_id,
            data,
            epb,
        });
        Ok(())
    }

    /// Emulation prevention bytes in `body` below `rbsp_bits` of header.
    fn count_epb(body: &[u8], rbsp_bits: usize) -> u32 {
        let mut epb = 0u32;
        let mut zeros = 0;
        let mut bits = 0;
        for byte in body {
            if bits >= rbsp_bits {
                break;
            }
            if zeros == 2 && *byte == 3 {
                epb += 1;
                zeros = 0;
                continue;
            }
            bits += 8;
            zeros = if *byte == 0 { zeros + 1 } else { 0 };
        }
        epb
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
            .ok_or(VaApiError::Parse(CapsParse::PictureWithoutSlices))?;
        Self::check_supported(&pending.sps, &pending.pps)?;
        let pic = PictureData::new_from_slice(
            &first.header,
            first.nalu_type,
            first.temporal_id,
            self.first_in_bitstream,
            self.first_after_eos,
            self.prev_tid0,
            &pending.sps,
        );
        self.first_in_bitstream = false;
        self.first_after_eos = false;
        if is_irap(pic.nalu_type) {
            self.irap_no_rasl = pic.output.no_rasl_output_flag;
        } else if is_rasl(pic.nalu_type) && self.irap_no_rasl {
            return Ok(None);
        }
        let kept = self.mark_rps(&first.header, &first.tail, &pic);
        let bumped = self.update_before_decoding(&pic, &pending.sps);
        let rps = self.locate_rps(&kept);
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
            self.prev_tid0 = Some(picture.pic.pic_order_cnt_val);
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

    /// Mark DPB reference states for `header` per 8.3.3, returning active handles.
    fn mark_rps(&mut self, header: &SliceHeader, tail: &SliceTail, pic: &PictureData) -> KeptRps {
        if is_irap(pic.nalu_type) && pic.output.no_rasl_output_flag {
            self.dpb.mark_all_as_unused_for_ref();
        }
        if is_idr(pic.nalu_type) {
            return KeptRps::default();
        }
        let Some(set) = header.rps.as_ref() else {
            return KeptRps::default();
        };
        let (st_before, st_after, st_foll) = short_term_pocs(set, pic.pic_order_cnt_val);
        let (lt_curr, lt_curr_masked, lt_foll, lt_foll_masked) =
            long_term_pocs(&tail.lt, pic.pic_order_cnt_val, self.max_poc_lsb);
        let st_curr_before = self.resolve_short(&st_before);
        let st_curr_after = self.resolve_short(&st_after);
        let lt_curr_pics = self.resolve_long(&lt_curr, &lt_curr_masked);
        let foll_short = self.resolve_short(&st_foll);
        let foll_long = self.resolve_long(&lt_foll, &lt_foll_masked);
        for pic_rc in lt_curr_pics.iter().chain(foll_long.iter()).flatten() {
            pic_rc.borrow_mut().set_reference(Reference::LongTerm);
        }
        let mut all_kept = Vec::new();
        all_kept.extend(st_curr_before.iter().flatten().cloned());
        all_kept.extend(st_curr_after.iter().flatten().cloned());
        all_kept.extend(lt_curr_pics.iter().flatten().cloned());
        all_kept.extend(foll_short.into_iter().flatten());
        all_kept.extend(foll_long.into_iter().flatten());
        for entry in self.dpb.entries() {
            if !all_kept.iter().any(|kept| Rc::ptr_eq(&entry.0, kept)) {
                entry.0.borrow_mut().set_reference(Reference::None);
            }
        }
        KeptRps {
            st_curr_before,
            st_curr_after,
            lt_curr: lt_curr_pics,
        }
    }

    /// Post-bump DPB indices for `kept`.
    fn locate_rps(&self, kept: &KeptRps) -> Rps {
        Rps {
            st_curr_before: self.locate_list(&kept.st_curr_before),
            st_curr_after: self.locate_list(&kept.st_curr_after),
            lt_curr: self.locate_list(&kept.lt_curr),
        }
    }

    /// Post-bump DPB positions for `list`.
    fn locate_list(&self, list: &[Option<Rc<RefCell<PictureData>>>]) -> Vec<Option<usize>> {
        list.iter()
            .map(|entry| {
                entry.as_ref().and_then(|target| {
                    self.dpb
                        .entries()
                        .iter()
                        .position(|stored| Rc::ptr_eq(&stored.0, target))
                })
            })
            .collect()
    }

    /// DPB picture handles of short-term `pocs`, `None` where absent.
    fn resolve_short(&self, pocs: &[i32]) -> Vec<Option<Rc<RefCell<PictureData>>>> {
        pocs.iter()
            .map(|poc| {
                self.dpb
                    .find_short_term_ref_by_poc(*poc)
                    .map(|entry| entry.0)
            })
            .collect()
    }

    /// DPB picture handles of long-term `pocs`, masked where `masks` holds.
    fn resolve_long(&self, pocs: &[i32], masks: &[bool]) -> Vec<Option<Rc<RefCell<PictureData>>>> {
        pocs.iter()
            .zip(masks.iter())
            .map(|(poc, masked)| {
                let entry = if *masked {
                    self.dpb.find_ref_by_poc_masked(*poc, self.max_poc_lsb - 1)
                } else {
                    self.dpb.find_ref_by_poc(*poc)
                };
                entry.map(|entry| entry.0)
            })
            .collect()
    }

    /// Bump or clear the DPB ahead of decoding `pic`.
    fn update_before_decoding(&mut self, pic: &PictureData, sps: &Sps) -> Vec<SurfaceSlot> {
        let mut outputs = Vec::new();
        if is_irap(pic.nalu_type) && pic.output.no_rasl_output_flag {
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
        let profile = sps.ptl.profile_idc;
        // Compatibility flags pack into bits 31..0 for profiles 0..32.
        let compat = sps.ptl.compat & (1u32 << 30) != 0 || sps.ptl.compat & (1u32 << 28) != 0;
        if profile != 1 && profile != 3 && !compat {
            return Err(VaApiError::Unsupported(CapsUnsupported::ProfileOutsideMain));
        }
        if sps.chroma_format != 1 {
            return Err(VaApiError::Unsupported(CapsUnsupported::ChromaOutside420));
        }
        if sps.separate_colour_plane {
            return Err(VaApiError::Unsupported(CapsUnsupported::ChromaOutside420));
        }
        if sps.bit_depth_luma != 0 {
            return Err(VaApiError::Unsupported(CapsUnsupported::LumaDepthAbove8Bit));
        }
        if sps.bit_depth_chroma != 0 {
            return Err(VaApiError::Unsupported(
                CapsUnsupported::ChromaDepthAbove8Bit,
            ));
        }
        if sps.sps_extension_flags[0] || pps.pps_extension_flags[0] {
            return Err(VaApiError::Unsupported(CapsUnsupported::SpsExtension));
        }
        if sps.sps_extension_flags[3] || pps.pps_extension_flags[3] {
            return Err(VaApiError::Unsupported(CapsUnsupported::SpsExtension));
        }
        if sps.vui.as_ref().is_some_and(|vui| vui.field_seq) {
            return Err(VaApiError::Unsupported(CapsUnsupported::Interlaced));
        }
        Ok(())
    }
}

impl Default for Frontend {
    fn default() -> Self {
        Self::new()
    }
}

/// Short-term POCs of `set` against `poc`: before, after, following.
fn short_term_pocs(set: &ShortTermRps, poc: i32) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut foll = Vec::new();
    for (index, used) in set
        .negative_used
        .iter()
        .enumerate()
        .take(usize::from(set.negative_count))
    {
        let Some(delta) = set.negative.get(index) else {
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
        .positive_used
        .iter()
        .enumerate()
        .take(usize::from(set.positive_count))
    {
        let Some(delta) = set.positive.get(index) else {
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

/// Long-term POCs of `lt` against `poc`: current, masked, following, masked.
fn long_term_pocs(
    lt: &[LtSpec],
    poc: i32,
    max_lsb: i32,
) -> (Vec<i32>, Vec<bool>, Vec<i32>, Vec<bool>) {
    let mut curr = Vec::new();
    let mut curr_masked = Vec::new();
    let mut foll = Vec::new();
    let mut foll_masked = Vec::new();
    for pic in lt {
        let entry = if pic.msb_present {
            let max = u32::try_from(max_lsb).unwrap_or(u32::MAX);
            pic.poc_msb_present(poc, max).unwrap_or(i32::MAX)
        } else {
            i32::try_from(pic.poc_lsb).unwrap_or(i32::MAX)
        };
        if pic.used {
            curr.push(entry);
            curr_masked.push(!pic.msb_present);
        } else {
            foll.push(entry);
            foll_masked.push(!pic.msb_present);
        }
    }
    (curr, curr_masked, foll, foll_masked)
}

#[cfg(test)]
mod tests {
    use rust_video_h265::HevcGate;

    use super::{long_term_pocs, short_term_pocs, Frontend};
    use crate::error::VaApiError;
    use crate::fixtures::{idr_payload, nal, p_payload, pps_payload, sps_payload};
    use crate::h265::{
        parse_pps, parse_slice_tail, parse_sps, rbsp, Gate, LtSpec, ShortTermRps, SliceHeader,
        SliceTail, SliceType,
    };

    use bytes::Bytes;
    use rust_video_harness::Bits;

    /// Frontend that ingested the sequence and picture sets.
    fn sequenced() -> Frontend {
        let mut frontend = Frontend::new();
        let mut stream = nal(33, &sps_payload());
        stream.extend(nal(34, &pps_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("sets parse");
        assert_eq!(ingest.pictures(), []);
        assert_eq!(frontend.coded_size(), Some((64, 64)));
        frontend
    }

    #[test]
    fn ingest_groups_slices_into_pictures() {
        let mut frontend = sequenced();
        let mut stream = nal(19, &idr_payload());
        stream.extend(nal(1, &p_payload(2, 1)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let pictures = ingest.pictures();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].slices.len(), 1);
        assert!(pictures[0].slices[0].header.slice_type.is_intra());
        let pending = frontend.take_pending().expect("open p picture");
        assert!(matches!(pending.slices[0].header.slice_type, SliceType::P));
        assert!(frontend.take_pending().is_none());
    }

    #[test]
    fn begin_orders_pocs_and_resolves_references() {
        let mut frontend = sequenced();
        let mut stream = nal(19, &idr_payload());
        stream.extend(nal(1, &p_payload(2, 1)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let idr = ingest.pictures().pop().expect("idr picture");
        let (picture, bumped) = frontend.begin(idr).expect("idr begins").expect("idr kept");
        assert_eq!(bumped, []);
        assert_eq!(picture.pic().pic_order_cnt_val, 0);
        assert_eq!(picture.rps().st_curr_before(), []);
        let outputs = frontend
            .commit(picture, crate::h264::frontend::SurfaceSlot::new(0))
            .expect("idr commits");
        assert_eq!(outputs, []);
        let pending = frontend.take_pending().expect("open p picture");
        let (picture, bumped) = frontend.begin(pending).expect("p begins").expect("p kept");
        assert_eq!(bumped, []);
        assert_eq!(picture.pic().pic_order_cnt_val, 2);
        assert_eq!(picture.rps().st_curr_before(), &[Some(0)]);
        let (list0, list1) = picture
            .rps()
            .lists(picture.slices()[0].header(), picture.slices()[0].tail())
            .expect("p lists");
        assert_eq!(list0, vec![Some(0)]);
        assert_eq!(list1, []);
        let outputs = frontend
            .commit(picture, crate::h264::frontend::SurfaceSlot::new(1))
            .expect("p commits");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].index(), 0);
    }

    #[test]
    fn begin_drops_rasl_after_irap() {
        let mut frontend = sequenced();
        let mut stream = nal(19, &idr_payload());
        stream.extend(nal(9, &p_payload(1, 0)));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("slices parse");
        let idr = ingest.pictures().pop().expect("idr picture");
        let (picture, _) = frontend.begin(idr).expect("idr begins").expect("idr kept");
        frontend
            .commit(picture, crate::h264::frontend::SurfaceSlot::new(0))
            .expect("idr commits");
        let pending = frontend.take_pending().expect("open rasl picture");
        assert!(frontend.begin(pending).expect("rasl begins").is_none());
    }

    #[test]
    fn picture_set_before_sequence_set_recovers() {
        let mut frontend = Frontend::new();
        let mut stream = nal(34, &pps_payload());
        stream.extend(nal(33, &sps_payload()));
        stream.extend(nal(19, &idr_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("early pps recovers");
        assert_eq!(ingest.pictures().len(), 0);
        assert!(frontend.take_pending().is_some());
    }

    #[test]
    fn layered_slice_declines_typed() {
        let mut frontend = sequenced();
        let mut slice = vec![0, 0, 0, 1, 1u8 << 1, 0x09];
        slice.extend(p_payload(2, 1));
        let bytes = Bytes::from(slice);
        let Err(VaApiError::Unsupported(reason)) = frontend.ingest(&bytes) else {
            panic!("layered slice declines");
        };
        assert!(reason.to_string().contains("multilayer"));
    }

    #[test]
    fn dependent_slice_without_independent_fails() {
        let mut frontend = Frontend::new();
        let mut stream = nal(33, &sps_payload());
        let mut pps = Bits::new();
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
        stream.extend(nal(34, &pps.finish()));
        let mut slice = Bits::new();
        slice.bit(false);
        slice.ue(0);
        slice.bit(true);
        slice.se(0);
        stream.extend(nal(1, &slice.finish()));
        let bytes = Bytes::from(stream);
        let Err(VaApiError::Unsupported(reason)) = frontend.ingest(&bytes) else {
            panic!("orphan dependent slice fails");
        };
        assert!(reason.to_string().contains("dependent"));
    }

    #[test]
    fn garbage_sequence_set_fails() {
        let mut frontend = Frontend::new();
        let stream = nal(33, &[0x00]);
        let bytes = Bytes::from(stream);
        assert!(matches!(frontend.ingest(&bytes), Err(VaApiError::Parse(_))));
    }

    #[test]
    fn sequence_change_drains_and_reconfigures() {
        let mut frontend = sequenced();
        let stream = nal(19, &idr_payload());
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("idr parses");
        assert_eq!(ingest.pictures(), []);
        let idr = frontend.take_pending().expect("open idr");
        let (picture, _) = frontend.begin(idr).expect("begins").expect("kept");
        frontend
            .commit(picture, crate::h264::frontend::SurfaceSlot::new(3))
            .expect("commits");
        let mut stream = nal(33, &sps_payload());
        stream.extend(nal(34, &pps_payload()));
        let bytes = Bytes::from(stream);
        let mut ingest = frontend.ingest(&bytes).expect("same sets parse");
        assert!(!ingest.reconfigured());
        assert_eq!(ingest.drained(), []);
        let mut writer = Bits::new();
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
        let stream = nal(33, &writer.finish());
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
        let pps = frontend.pps(0).expect("parsed set").clone();
        let mut bad = (*sps).clone();
        bad.ptl.profile_idc = 4;
        bad.ptl.compat = 0;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut compat = (*sps).clone();
        compat.ptl.profile_idc = 2;
        compat.ptl.compat |= 1u32 << 30;
        assert!(Frontend::check_supported(&compat, &pps).is_ok());
        compat.bit_depth_luma = 2;
        assert!(matches!(
            Frontend::check_supported(&compat, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.chroma_format = 0;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.bit_depth_luma = 2;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*sps).clone();
        bad.sps_extension_flags[3] = true;
        assert!(matches!(
            Frontend::check_supported(&bad, &pps),
            Err(VaApiError::Unsupported(_))
        ));
        let mut bad = (*pps).clone();
        bad.pps_extension_flags[0] = true;
        assert!(matches!(
            Frontend::check_supported(&sps, &bad),
            Err(VaApiError::Unsupported(_))
        ));
        assert!(Frontend::check_supported(&sps, &pps).is_ok());
    }

    /// Slice header with three used short-term pictures.
    fn list_header(slice_type: SliceType) -> SliceHeader {
        let mut set = ShortTermRps {
            negative_count: 2,
            positive_count: 1,
            ..ShortTermRps::default()
        };
        set.negative_used[0] = true;
        set.negative_used[1] = true;
        set.positive_used[0] = true;
        SliceHeader {
            first: true,
            address: 0,
            slice_type,
            pic_output: true,
            no_output_prior: false,
            poc_lsb: Some(0),
            rps: Some(set),
            rps_bits: None,
            lt: Vec::new(),
            entry_count: 0,
            extension_length: 0,
        }
    }

    #[test]
    fn short_term_pocs_split_current_and_following() {
        let mut set = ShortTermRps {
            negative_count: 2,
            positive_count: 1,
            ..ShortTermRps::default()
        };
        set.negative[0] = -1;
        set.negative[1] = -3;
        set.positive[0] = 2;
        set.negative_used[0] = true;
        set.negative_used[1] = false;
        set.positive_used[0] = true;
        let (before, after, foll) = short_term_pocs(&set, 10);
        assert_eq!(before, vec![9]);
        assert_eq!(after, vec![12]);
        assert_eq!(foll, vec![7]);
    }

    #[test]
    fn long_term_poc_applies_msb_cycle() {
        let lt = vec![
            LtSpec {
                poc_lsb: 4,
                used: true,
                msb_present: true,
                msb_cycle: 1,
            },
            LtSpec {
                poc_lsb: 9,
                used: true,
                msb_present: false,
                msb_cycle: 0,
            },
        ];
        let (curr, masked, foll, _) = long_term_pocs(&lt, 260, 256);
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
        let mut header = list_header(SliceType::P);
        let mut tail = SliceTail {
            num_ref_l0: 2,
            ..SliceTail::default()
        };
        let (list0, list1) = rps.lists(&header, &tail).expect("p lists");
        assert_eq!(list0, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(list1, []);
        header.slice_type = SliceType::B;
        tail.num_ref_l1 = 2;
        let (list0, list1) = rps.lists(&header, &tail).expect("b lists");
        assert_eq!(list0, vec![Some(0), Some(1), Some(2)]);
        assert_eq!(list1, vec![Some(1), Some(0), Some(2)]);
        header.slice_type = SliceType::I;
        let (list0, list1) = rps.lists(&header, &tail).expect("i lists");
        assert_eq!(list0, []);
        assert_eq!(list1, []);
    }

    #[test]
    fn malformed_video_set_fails_ingest() {
        let mut frontend = Frontend::new();
        let bytes = Bytes::from(nal(32, &[0xFF, 0xFF]));
        assert!(matches!(frontend.ingest(&bytes), Err(VaApiError::Parse(_))));
    }

    #[test]
    fn slice_carries_bit_size_and_epb() {
        let mut frontend = sequenced();
        let bytes = Bytes::from(nal(19, &idr_payload()));
        frontend.ingest(&bytes).expect("idr parses");
        let pending = frontend.take_pending().expect("open picture");
        assert!(pending.slices[0].bit_size() > 16);
        assert_eq!(pending.slices[0].epb(), 0);
    }

    #[test]
    fn rps_lists_apply_modification() {
        use super::Rps;

        let rps = Rps {
            st_curr_before: vec![Some(0), Some(1)],
            st_curr_after: vec![],
            lt_curr: vec![Some(2)],
        };
        let header = list_header(SliceType::P);
        let mut tail = SliceTail {
            num_ref_l0: 1,
            mod_flag_l0: true,
            mod_entries_l0: vec![2, 0],
            ..SliceTail::default()
        };
        let (list0, _) = rps.lists(&header, &tail).expect("modified lists");
        assert_eq!(list0, vec![Some(2), Some(0)]);
        tail.mod_entries_l0 = vec![9, 0];
        assert!(matches!(
            rps.lists(&header, &tail),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    /// Parsed fixture sequence and picture sets.
    fn sets() -> (rust_video_h265::Sps, rust_video_h265::Pps) {
        let seq_raw = sps_payload();
        let pic_raw = pps_payload();
        let seq = parse_sps::<VaApiError, Gate>(&rbsp(&seq_raw)).expect("sps");
        let pic = parse_pps::<VaApiError, Gate>(&rbsp(&pic_raw), Some(&seq)).expect("pps");
        (seq, pic)
    }

    #[test]
    fn tail_carries_p_slice_submit_fields() {
        let (seq, pic) = sets();
        let raw = p_payload(2, 1);
        let slice_rbsp = rbsp(&raw);
        let tail = parse_slice_tail::<VaApiError>(&slice_rbsp, 1, &seq, &pic).expect("tail");
        assert_eq!((tail.num_ref_l0, tail.num_ref_l1), (0, 0));
        assert!(!tail.mod_flag_l0 && !tail.mod_flag_l1);
        assert!(tail.mod_entries_l0.is_empty() && tail.mod_entries_l1.is_empty());
        assert!(!tail.temporal_mvp);
        assert!(!tail.sao_luma && !tail.sao_chroma);
        assert!(!tail.mvd_l1_zero && !tail.cabac_init);
        assert!(tail.collocated_from_l0);
        assert_eq!(tail.collocated_ref_idx, 0);
        assert_eq!(tail.weights.luma_denom, 0);
        assert!(tail.weights.l0.is_empty() && tail.weights.l1.is_empty());
        assert_eq!(tail.merge_cand_minus5, 0);
        assert_eq!(
            (tail.qp_delta, tail.cb_qp_offset, tail.cr_qp_offset),
            (0, 0, 0)
        );
        assert!(!tail.deblocking_disabled);
        assert_eq!((tail.beta_offset, tail.tc_offset), (0, 0));
        assert!(!pic.loop_across_slices());
        assert!(!tail.loop_filter_across);
        assert_eq!(tail.lt, []);
        assert!(tail.bit_size > 16);
    }

    #[test]
    fn tail_defaults_on_idr_slices() {
        let (seq, pic) = sets();
        let raw = idr_payload();
        let slice_rbsp = rbsp(&raw);
        let tail = parse_slice_tail::<VaApiError>(&slice_rbsp, 19, &seq, &pic).expect("tail");
        assert_eq!((tail.num_ref_l0, tail.num_ref_l1), (0, 0));
        assert_eq!(tail.merge_cand_minus5, 0);
        assert_eq!(tail.qp_delta, 0);
        assert_eq!(tail.colour_plane, 0);
        assert_eq!(tail.lt, []);
        assert!(tail.bit_size > 16);
    }

    #[test]
    fn gate_bounds_picture_qp_and_counts() {
        let (seq, _) = sets();
        let pic_raw = pps_payload();
        let base = parse_pps::<VaApiError, Gate>(&rbsp(&pic_raw), Some(&seq)).expect("pps");
        let mut pic = base.clone();
        pic.pic_init_qp_minus26 = 26;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&seq)),
            Err(VaApiError::Parse(_))
        ));
        let mut pic = base.clone();
        pic.cb_qp_offset = 13;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&seq)),
            Err(VaApiError::Parse(_))
        ));
        let mut pic = base;
        pic.num_ref_l0 = 15;
        assert!(matches!(
            Gate::check_pps(&pic, Some(&seq)),
            Err(VaApiError::Parse(_))
        ));
    }
}
