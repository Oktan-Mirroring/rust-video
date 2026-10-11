//! H.264 access-unit assembly, picture-order counts, and DPB orchestration.
//!
//! Portable: parses Annex B, orders pictures per 7.4.1.2.4, derives order
//! counts per 8.2.1, builds and modifies reference lists per 8.2.4, and
//! marks the DPB per 8.2.5. A VA-API backend submits finished pictures.

use std::io::Cursor;
use std::rc::Rc;

use crate::error::VaApiError;

use cros_codecs::codec::h264::dpb::{Dpb, ReferencePicLists};
use cros_codecs::codec::h264::parser::{
    MaxLongTermFrameIdx, Nalu, NaluType, Parser, Pps, SliceHeader, Sps,
};
use cros_codecs::codec::h264::picture::{Field, IsIdr, PictureData, Reference};

/// DPB surface slot.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SurfaceSlot {
    /// Pool index.
    index: u32,
}

impl SurfaceSlot {
    /// Slot over pool index `index`.
    #[must_use]
    pub fn new(index: u32) -> Self {
        Self { index }
    }

    /// Pool index.
    #[must_use]
    pub fn index(self) -> u32 {
        self.index
    }
}

/// One coded slice: parsed header plus its NAL bytes.
#[derive(Clone, Debug)]
pub struct PendingSlice {
    /// Parsed slice header.
    header: SliceHeader,
    /// NAL bytes past the start code, header byte included.
    data: Vec<u8>,
}

impl PendingSlice {
    /// Parsed slice header.
    #[must_use]
    pub fn header(&self) -> &SliceHeader {
        &self.header
    }

    /// NAL bytes past the start code, header byte included.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Slices of one coded picture, not yet ordered against the DPB.
pub struct PendingPicture {
    /// Active sequence set at the first slice.
    sps: Rc<Sps>,
    /// Picture set the first slice names.
    pps: Rc<Pps>,
    /// NAL reference flag of the first slice.
    nal_ref_idc: u8,
    /// True for IDR pictures.
    is_idr: bool,
    /// Slices in bitstream order.
    slices: Vec<PendingSlice>,
}

/// Coded picture with order counts and initial reference lists.
pub struct Picture {
    /// Picture state: counts, numbers, marking.
    pic: PictureData,
    /// Active sequence set.
    sps: Rc<Sps>,
    /// Active picture set.
    pps: Rc<Pps>,
    /// Slices in bitstream order.
    slices: Vec<PendingSlice>,
    /// Initial P/B reference lists as DPB indices.
    initial: ReferencePicLists,
}

impl Picture {
    /// Picture state: counts, numbers, marking.
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

    /// Initial P/B reference lists as DPB indices.
    #[must_use]
    pub fn initial(&self) -> &ReferencePicLists {
        &self.initial
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

/// H.264 frontend: Annex B in, ordered pictures out.
pub struct Frontend {
    /// Bitstream parser with the active parameter sets.
    parser: Parser,
    /// Active sequence set.
    sps: Option<Rc<Sps>>,
    /// Decoded-picture buffer over surface slots.
    dpb: Dpb<SurfaceSlot>,
    /// Picture-order-count state.
    poc: PocTracker,
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
    /// First-VCL key of the open picture.
    pending_key: Option<SliceKey>,
    /// Sets of the open picture.
    pending_sets: Option<(Rc<Sps>, Rc<Pps>)>,
    /// NAL flags of the open picture.
    pending_flags: Option<(u8, bool)>,
    /// Long-term index cap.
    max_long_term_frame_idx: MaxLongTermFrameIdx,
    /// Frame number of the previous reference picture.
    prev_ref_frame_num: Option<u32>,
    /// Timestamp counter.
    timestamp: u64,
}

impl Frontend {
    /// Empty frontend, no sequence active.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::default(),
            sps: None,
            dpb: Dpb::default(),
            poc: PocTracker::default(),
            pending: Vec::new(),
            pending_key: None,
            pending_sets: None,
            pending_flags: None,
            max_long_term_frame_idx: MaxLongTermFrameIdx::NoLongTermFrameIndices,
            prev_ref_frame_num: None,
            timestamp: 0,
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
        self.sps.as_ref().map(|sps| (sps.width(), sps.height()))
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
        self.sps.as_ref().map(|sps| sps.max_dpb_frames())
    }

    /// True when `index` backs a live DPB entry.
    #[must_use]
    pub fn slot_in_use(&self, index: u32) -> bool {
        self.dpb.entries().iter().any(|entry| {
            entry.reference.is_some_and(|slot| slot.index == index)
                || entry.decoded_frame.is_some_and(|slot| slot.index == index)
        })
    }

    /// Pictures finished by Annex B `bytes`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a NAL fails to parse or names an unknown
    /// parameter set.
    pub fn ingest(&mut self, bytes: &[u8]) -> Result<Ingest, VaApiError> {
        let mut out = Ingest {
            pictures: Vec::new(),
            drained: Vec::new(),
            reconfigured: false,
            drained_size: None,
            drained_crop: None,
        };
        let mut cursor = Cursor::new(bytes);
        loop {
            let Ok(nalu) = Nalu::next(&mut cursor) else {
                if Self::start_code_ahead(&cursor) {
                    return Err(VaApiError::Parse("broken nal unit".to_owned()));
                }
                break;
            };
            match nalu.header.type_ {
                NaluType::Sps => {
                    if self.adopt_sps(&nalu, &mut out)? {
                        out.reconfigured = true;
                    }
                }
                NaluType::Pps => {
                    self.parser.parse_pps(&nalu).map_err(VaApiError::Parse)?;
                }
                NaluType::Slice | NaluType::SliceIdr => {
                    self.ingest_slice(nalu, &mut out.pictures)?;
                }
                NaluType::SliceDpa | NaluType::SliceDpb | NaluType::SliceDpc => {
                    return Err(VaApiError::Unsupported("slice partitions".to_owned()));
                }
                NaluType::SliceAux
                | NaluType::SliceExt
                | NaluType::SliceDepth
                | NaluType::PrefixUnit
                | NaluType::SubsetSps => {
                    return Err(VaApiError::Unsupported("scalable/multiview nal".to_owned()));
                }
                NaluType::SeqEnd | NaluType::StreamEnd => {
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
        self.pending_key = None;
        let (sps, pps) = self.pending_sets.take()?;
        let (nal_ref_idc, is_idr) = self.pending_flags.take()?;
        Some(PendingPicture {
            sps,
            pps,
            nal_ref_idc,
            is_idr,
            slices,
        })
    }

    /// Slots still held by the DPB, in output order.
    pub fn drain(&mut self) -> Vec<SurfaceSlot> {
        self.dpb.drain().into_iter().flatten().collect()
    }

    /// Adopt `nalu` as the active sequence set.
    ///
    /// A changed set finishes the open picture, drains the DPB, and
    /// resets decode state; the return reports the change.
    fn adopt_sps(&mut self, nalu: &Nalu, out: &mut Ingest) -> Result<bool, VaApiError> {
        let sps = self
            .parser
            .parse_sps(nalu)
            .map_err(VaApiError::Parse)?
            .clone();
        if self.sps.as_ref().is_some_and(|active| **active == *sps) {
            return Ok(false);
        }
        if let Some(picture) = self.take_pending() {
            out.pictures.push(picture);
        }
        out.drained_size = self.coded_size();
        out.drained_crop = self.visible_rect();
        out.drained.extend(self.drain());
        self.dpb = Dpb::default();
        self.poc = PocTracker::default();
        self.max_long_term_frame_idx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
        self.prev_ref_frame_num = None;
        self.dpb
            .set_limits(sps.max_dpb_frames(), sps.max_num_order_frames() as usize);
        self.sps = Some(sps);
        Ok(true)
    }

    /// Append one slice NAL to the open picture.
    fn ingest_slice(
        &mut self,
        nalu: Nalu<'_>,
        pictures: &mut Vec<PendingPicture>,
    ) -> Result<(), VaApiError> {
        let ref_idc = nalu.header.ref_idc;
        let is_idr = nalu.header.idr_pic_flag;
        let slice = self
            .parser
            .parse_slice_header(nalu)
            .map_err(VaApiError::Parse)?;
        if slice.header.redundant_pic_cnt > 0 {
            return Ok(());
        }
        let sps = self
            .sps
            .clone()
            .ok_or_else(|| VaApiError::Parse("slice before sequence set".to_owned()))?;
        let pps = self
            .parser
            .get_pps(slice.header.pic_parameter_set_id)
            .cloned()
            .ok_or_else(|| VaApiError::Parse("slice names unknown picture set".to_owned()))?;
        let key = SliceKey::new(&slice.header, &sps, ref_idc, is_idr);
        if self
            .pending_key
            .as_ref()
            .is_some_and(|open| open.opens_new_picture(&key))
        {
            if let Some(picture) = self.take_pending() {
                pictures.push(picture);
            }
        }
        if self.pending.is_empty() {
            self.pending_key = Some(key);
            self.pending_sets = Some((sps, pps));
            self.pending_flags = Some((ref_idc, is_idr));
        }
        self.pending.push(PendingSlice {
            header: slice.header,
            data: slice.nalu.as_ref().to_vec(),
        });
        Ok(())
    }

    /// Order `pending` against the DPB: counts, gaps, initial lists.
    ///
    /// Returns the begun picture plus slots bumped by gap insertion.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the picture is interlaced, uses FMO, leaves
    /// 8-bit 4:2:0, opens a frame-number gap the stream forbids, or
    /// carries an unknown order-count type.
    pub fn begin(
        &mut self,
        pending: PendingPicture,
    ) -> Result<(Picture, Vec<SurfaceSlot>), VaApiError> {
        let first = pending
            .slices
            .first()
            .ok_or_else(|| VaApiError::Parse("picture without slices".to_owned()))?;
        Self::check_progressive(&pending.sps, &pending.pps, &first.header)?;
        let mut pic = PictureData::default();
        pic.pic_order_cnt_type = pending.sps.pic_order_cnt_type;
        pic.pic_num = i32::from(first.header.frame_num);
        pic.frame_num = u32::from(first.header.frame_num);
        pic.nal_ref_idc = pending.nal_ref_idc;
        pic.is_idr = if pending.is_idr {
            IsIdr::Yes {
                idr_pic_id: first.header.idr_pic_id,
            }
        } else {
            IsIdr::No
        };
        pic.ref_pic_marking = first.header.dec_ref_pic_marking.clone();
        pic.timestamp = self.stamp();
        pic.set_reference(
            if pending.nal_ref_idc == 0 {
                Reference::None
            } else {
                Reference::ShortTerm
            },
            true,
        );
        match pending.sps.pic_order_cnt_type {
            0 => {
                pic.pic_order_cnt_lsb = i32::from(first.header.pic_order_cnt_lsb);
                pic.delta_pic_order_cnt_bottom = first.header.delta_pic_order_cnt_bottom;
            }
            1 => {
                pic.delta_pic_order_cnt0 = first.header.delta_pic_order_cnt[0];
                pic.delta_pic_order_cnt1 = first.header.delta_pic_order_cnt[1];
            }
            _ => {}
        }
        self.dpb.set_limits(
            pending.sps.max_dpb_frames(),
            pending.sps.max_num_order_frames() as usize,
        );
        let bumped = self.bridge_gap(&pending.sps, &pic)?;
        self.poc.derive(&mut pic, &pending.sps)?;
        let max_frame_num = pending.sps.max_frame_num();
        self.dpb.update_pic_nums(pic.frame_num, max_frame_num, &pic);
        let initial = self.dpb.build_ref_pic_lists(&pic);
        Ok((
            Picture {
                pic,
                sps: pending.sps,
                pps: pending.pps,
                slices: pending.slices,
                initial,
            },
            bumped,
        ))
    }

    /// Reference lists for one slice: initial lists plus 8.2.4.3 modification.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a modification command names a picture the
    /// DPB cannot supply, or uses multiview commands.
    pub fn modify(
        &self,
        header: &SliceHeader,
        initial: &[usize],
        second: bool,
        curr_pic_num: i32,
        max_pic_num: i32,
    ) -> Result<Vec<usize>, VaApiError> {
        let (flag, active, mods) = if second {
            (
                header.ref_pic_list_modification_flag_l1,
                header.num_ref_idx_l1_active_minus1,
                &header.ref_pic_list_modification_l1,
            )
        } else {
            (
                header.ref_pic_list_modification_flag_l0,
                header.num_ref_idx_l0_active_minus1,
                &header.ref_pic_list_modification_l0,
            )
        };
        let mut list: Vec<usize> = initial.iter().copied().take(active as usize + 1).collect();
        if !flag {
            return Ok(list);
        }
        let mut pred = curr_pic_num;
        let mut ref_idx = 0usize;
        for modification in mods {
            match modification.modification_of_pic_nums_idc {
                0 | 1 => {
                    let step = i64::from(modification.abs_diff_pic_num_minus1) + 1;
                    let nowrap = if modification.modification_of_pic_nums_idc == 0 {
                        let raw = i64::from(pred) - step;
                        if raw < 0 {
                            raw + i64::from(max_pic_num)
                        } else {
                            raw
                        }
                    } else {
                        let raw = i64::from(pred) + step;
                        if raw >= i64::from(max_pic_num) {
                            raw - i64::from(max_pic_num)
                        } else {
                            raw
                        }
                    };
                    pred = i32::try_from(nowrap)
                        .map_err(|_| VaApiError::BrokenReference("picnum overflow".to_owned()))?;
                    let num = if nowrap > i64::from(curr_pic_num) {
                        pred.wrapping_sub(max_pic_num)
                    } else {
                        pred
                    };
                    let position = self
                        .dpb
                        .entries()
                        .iter()
                        .position(|entry| {
                            let picture = entry.pic.borrow();
                            matches!(picture.reference(), Reference::ShortTerm)
                                && picture.pic_num == num
                                && !picture.nonexisting
                        })
                        .ok_or_else(|| {
                            VaApiError::BrokenReference(format!("short-term {num} absent"))
                        })?;
                    Self::shift_insert(&mut list, ref_idx, position, active, |index| {
                        self.dpb.entries()[index]
                            .pic
                            .borrow()
                            .pic_num_f(max_pic_num)
                            != num
                    })?;
                    ref_idx += 1;
                }
                2 => {
                    let num = modification.long_term_pic_num;
                    let position = self
                        .dpb
                        .entries()
                        .iter()
                        .position(|entry| {
                            let picture = entry.pic.borrow();
                            matches!(picture.reference(), Reference::LongTerm)
                                && picture.long_term_pic_num == num
                        })
                        .ok_or_else(|| {
                            VaApiError::BrokenReference(format!("long-term {num} absent"))
                        })?;
                    let cap = self.max_long_term_frame_idx;
                    Self::shift_insert(&mut list, ref_idx, position, active, |index| {
                        self.dpb.entries()[index]
                            .pic
                            .borrow()
                            .long_term_pic_num_f(cap)
                            != num
                    })?;
                    ref_idx += 1;
                }
                3 => break,
                idc => {
                    return Err(VaApiError::Unsupported(format!("list modification {idc}")));
                }
            }
        }
        list.truncate(active as usize + 1);
        Ok(list)
    }

    /// Mark, store, and bump `picture` on `slot`, returning output slots.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a memory-management command is unknown or
    /// names a missing picture, or the DPB has no free slot.
    pub fn commit(
        &mut self,
        picture: Picture,
        slot: SurfaceSlot,
    ) -> Result<Vec<SurfaceSlot>, VaApiError> {
        let mut pic = picture.pic;
        let mut outputs = Vec::new();
        let is_idr = matches!(pic.is_idr, IsIdr::Yes { .. });
        if is_idr {
            if pic.ref_pic_marking.no_output_of_prior_pics_flag {
                self.dpb.clear();
            } else {
                outputs.extend(self.drain());
            }
            self.dpb.mark_all_as_unused_for_ref();
            if pic.ref_pic_marking.long_term_reference_flag {
                pic.set_reference(Reference::LongTerm, true);
                pic.long_term_frame_idx = 0;
                self.max_long_term_frame_idx = MaxLongTermFrameIdx::Idx(0);
            } else {
                pic.set_reference(Reference::ShortTerm, true);
                self.max_long_term_frame_idx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
            }
        } else if pic.nal_ref_idc != 0 {
            if pic.ref_pic_marking.adaptive_ref_pic_marking_mode_flag {
                let markings = pic.ref_pic_marking.clone();
                for marking in &markings.inner {
                    match marking.memory_management_control_operation {
                        0 => break,
                        1 => self
                            .dpb
                            .mmco_op_1(&pic, marking)
                            .map_err(|error| Self::broken(&error))?,
                        2 => self
                            .dpb
                            .mmco_op_2(&pic, marking)
                            .map_err(|error| Self::broken(&error))?,
                        3 => self
                            .dpb
                            .mmco_op_3(&pic, marking)
                            .map_err(|error| Self::broken(&error))?,
                        4 => {
                            self.max_long_term_frame_idx = self.dpb.mmco_op_4(marking);
                        }
                        5 => {
                            self.max_long_term_frame_idx = self.dpb.mmco_op_5(&mut pic);
                        }
                        6 => self.dpb.mmco_op_6(&mut pic, marking),
                        operation => {
                            return Err(VaApiError::BrokenReference(format!(
                                "unknown mmco {operation}"
                            )));
                        }
                    }
                }
            } else {
                self.dpb.sliding_window_marking(&mut pic, &picture.sps);
            }
            if !matches!(pic.reference(), Reference::LongTerm) {
                pic.set_reference(Reference::ShortTerm, true);
            }
        }
        if pic.is_ref() {
            self.poc.note_ref(&pic);
        }
        self.poc.note_pic(&pic);
        if pic.has_mmco_5 {
            outputs.extend(self.drain());
        }
        outputs.extend(self.dpb.bump_as_needed(&pic).into_iter().flatten());
        let is_ref = pic.is_ref();
        let frame_num = pic.frame_num;
        if is_ref || self.dpb.has_empty_frame_buffer() {
            self.dpb
                .store_picture(pic.into_rc(), Some(slot))
                .map_err(|_| VaApiError::DpbFull)?;
        } else {
            outputs.push(slot);
        }
        if is_ref {
            self.prev_ref_frame_num = Some(frame_num);
        }
        Ok(outputs)
    }

    /// Non-existing pictures bridging a frame-number gap, per 8.2.5.2.
    fn bridge_gap(&mut self, sps: &Sps, pic: &PictureData) -> Result<Vec<SurfaceSlot>, VaApiError> {
        let mut bumped = Vec::new();
        if matches!(pic.is_idr, IsIdr::Yes { .. }) || self.dpb.is_empty() {
            return Ok(bumped);
        }
        let Some(previous) = self.prev_ref_frame_num else {
            return Ok(bumped);
        };
        let max_frame_num = sps.max_frame_num();
        let expected = (previous + 1) % max_frame_num;
        if pic.frame_num == previous || pic.frame_num == expected {
            return Ok(bumped);
        }
        if !sps.gaps_in_frame_num_value_allowed_flag {
            return Err(VaApiError::Unsupported(format!(
                "frame_num gap {} -> {}",
                previous, pic.frame_num
            )));
        }
        let mut unused = expected;
        while unused != pic.frame_num {
            let mut gap = PictureData::new_non_existing(unused, self.stamp());
            if sps.pic_order_cnt_type != 0 {
                self.poc.derive(&mut gap, sps)?;
            }
            self.dpb.update_pic_nums(unused, max_frame_num, &gap);
            self.dpb.sliding_window_marking(&mut gap, sps);
            bumped.extend(self.dpb.bump_as_needed(&gap).into_iter().flatten());
            self.dpb
                .store_picture(gap.into_rc(), None)
                .map_err(|_| VaApiError::DpbFull)?;
            let noted = self.dpb.entries().last().map(|stored| {
                let picture = stored.pic.borrow();
                (
                    picture.frame_num,
                    picture.frame_num_offset,
                    picture.has_mmco_5,
                    picture.pic_order_cnt_msb,
                    picture.pic_order_cnt_lsb,
                    picture.top_field_order_cnt,
                    picture.field,
                )
            });
            if let Some(noted) = noted {
                self.poc.note_stored(noted);
            }
            unused = (unused + 1) % max_frame_num;
        }
        Ok(bumped)
    }

    /// Next picture timestamp.
    fn stamp(&mut self) -> u64 {
        let stamp = self.timestamp;
        self.timestamp = self.timestamp.wrapping_add(1);
        stamp
    }

    /// Reject streams outside progressive 8-bit 4:2:0 without FMO.
    fn check_progressive(sps: &Sps, pps: &Pps, header: &SliceHeader) -> Result<(), VaApiError> {
        if header.field_pic_flag {
            return Err(VaApiError::Unsupported("interlaced".to_owned()));
        }
        if pps.num_slice_groups_minus1 > 0 {
            return Err(VaApiError::Unsupported("fmo".to_owned()));
        }
        if sps.separate_colour_plane_flag {
            return Err(VaApiError::Unsupported("separate colour planes".to_owned()));
        }
        if sps.chroma_format_idc != 1 {
            return Err(VaApiError::Unsupported(format!(
                "chroma {}",
                sps.chroma_format_idc
            )));
        }
        if sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
            return Err(VaApiError::Unsupported("high bit depth".to_owned()));
        }
        Ok(())
    }

    /// Shift-insert per 8-37/8-38, keeping entries that `keep` accepts.
    fn shift_insert(
        list: &mut Vec<usize>,
        at: usize,
        position: usize,
        active: u8,
        keep: impl Fn(usize) -> bool,
    ) -> Result<(), VaApiError> {
        if at > list.len() {
            return Err(VaApiError::BrokenReference(
                "ref index past list".to_owned(),
            ));
        }
        list.insert(at, position);
        let next = at + 1;
        let mut write = next;
        let end = active as usize + 1;
        let mut read = next;
        while read <= end && read < list.len() {
            if keep(list[read]) {
                list[write] = list[read];
                write += 1;
            }
            read += 1;
        }
        list.truncate(write.min(end));
        Ok(())
    }

    /// Marking failure as a broken reference list.
    fn broken(error: &cros_codecs::codec::h264::dpb::MmcoError) -> VaApiError {
        VaApiError::BrokenReference(error.to_string())
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

/// First-VCL key per 7.4.1.2.4.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SliceKey {
    /// Slice frame number.
    frame_num: u16,
    /// Picture set id.
    pps_id: u8,
    /// Coded field.
    field: Field,
    /// Whether the NAL reference flag is zero.
    nal_ref_zero: bool,
    /// Order-count lsb.
    pic_order_cnt_lsb: u16,
    /// Bottom-field order delta.
    delta_pic_order_cnt_bottom: i32,
    /// Order-count deltas.
    delta_pic_order_cnt: [i32; 2],
    /// IDR flag.
    idr: bool,
    /// IDR id.
    idr_pic_id: u16,
    /// Order-count type.
    pic_order_cnt_type: u8,
}

impl SliceKey {
    /// Key for `header` under `sps`.
    fn new(header: &SliceHeader, sps: &Sps, nal_ref_idc: u8, idr: bool) -> Self {
        Self {
            frame_num: header.frame_num,
            pps_id: header.pic_parameter_set_id,
            field: header.field(),
            nal_ref_zero: nal_ref_idc == 0,
            pic_order_cnt_lsb: header.pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom: header.delta_pic_order_cnt_bottom,
            delta_pic_order_cnt: header.delta_pic_order_cnt,
            idr,
            idr_pic_id: header.idr_pic_id,
            pic_order_cnt_type: sps.pic_order_cnt_type,
        }
    }

    /// True when `other` opens a new picture after `self`.
    fn opens_new_picture(&self, other: &Self) -> bool {
        self.frame_num != other.frame_num
            || self.pps_id != other.pps_id
            || self.field != other.field
            || self.nal_ref_zero != other.nal_ref_zero
            || (self.pic_order_cnt_type == 0
                && other.pic_order_cnt_type == 0
                && (self.pic_order_cnt_lsb != other.pic_order_cnt_lsb
                    || self.delta_pic_order_cnt_bottom != other.delta_pic_order_cnt_bottom))
            || (self.pic_order_cnt_type == 1
                && other.pic_order_cnt_type == 1
                && self.delta_pic_order_cnt != other.delta_pic_order_cnt)
            || self.idr != other.idr
            || (self.idr && other.idr && self.idr_pic_id != other.idr_pic_id)
    }
}

/// Previous reference picture counts per 8.2.1.
#[derive(Clone, Debug, Default)]
struct ReferenceCounts {
    /// Order-count msb.
    msb: i32,
    /// Order-count lsb.
    lsb: i32,
    /// Top order count.
    top: i32,
    /// Whether the picture is a bottom field.
    bottom: bool,
    /// Whether the picture carries MMCO 5.
    mmco5: bool,
}

/// Previous picture numbers per 8.2.1.
#[derive(Clone, Debug, Default)]
struct PreviousNumbers {
    /// Frame number.
    frame_num: u32,
    /// Frame-number offset.
    frame_num_offset: u32,
    /// Whether the picture carries MMCO 5.
    mmco5: bool,
}

/// Picture-order-count state per 8.2.1.
#[derive(Clone, Debug, Default)]
struct PocTracker {
    /// Previous reference picture counts, if one was stored.
    reference: Option<ReferenceCounts>,
    /// Previous picture numbers, if one was stored.
    previous: Option<PreviousNumbers>,
}

impl PocTracker {
    /// Order counts for `pic` under `sps`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on an unknown order-count type or an order count
    /// outside the `i32` range.
    fn derive(&mut self, pic: &mut PictureData, sps: &Sps) -> Result<(), VaApiError> {
        let is_idr = matches!(pic.is_idr, IsIdr::Yes { .. });
        match pic.pic_order_cnt_type {
            0 => self.derive_type0(pic, sps, is_idr),
            1 => self.derive_type1(pic, sps, is_idr)?,
            2 => self.derive_type2(pic, sps, is_idr)?,
            other => {
                return Err(VaApiError::Unsupported(format!(
                    "pic_order_cnt_type {other}"
                )));
            }
        }
        pic.pic_order_cnt = pic.top_field_order_cnt.min(pic.bottom_field_order_cnt);
        Ok(())
    }

    /// Type-0 counts per 8.2.1.1.
    fn derive_type0(&self, pic: &mut PictureData, sps: &Sps, is_idr: bool) {
        let base = self.type0_base(is_idr);
        let max_lsb = 1 << (u32::from(sps.log2_max_pic_order_cnt_lsb_minus4) + 4);
        let lsb = pic.pic_order_cnt_lsb;
        pic.pic_order_cnt_msb = if lsb < base.1 && base.1 - lsb >= max_lsb / 2 {
            base.0 + max_lsb
        } else if lsb > base.1 && lsb - base.1 > max_lsb / 2 {
            base.0 - max_lsb
        } else {
            base.0
        };
        pic.top_field_order_cnt = pic.pic_order_cnt_msb + lsb;
        pic.bottom_field_order_cnt = pic.top_field_order_cnt + pic.delta_pic_order_cnt_bottom;
    }

    /// Type-1 counts per 8.2.1.2.
    fn derive_type1(
        &self,
        pic: &mut PictureData,
        sps: &Sps,
        is_idr: bool,
    ) -> Result<(), VaApiError> {
        pic.frame_num_offset = self.frame_num_offset(pic, sps, is_idr);
        let cycle = u64::from(sps.num_ref_frames_in_pic_order_cnt_cycle);
        let mut absolute = if cycle == 0 {
            0
        } else {
            u64::from(pic.frame_num_offset) + u64::from(pic.frame_num)
        };
        if pic.nal_ref_idc == 0 && absolute > 0 {
            absolute -= 1;
        }
        let mut expected = 0i64;
        if absolute > 0 {
            if cycle == 0 {
                return Err(VaApiError::Parse("empty poc cycle".to_owned()));
            }
            let in_cycle = usize::try_from((absolute - 1) % cycle).unwrap_or(0);
            let cycles = i64::try_from((absolute - 1) / cycle)
                .map_err(|_| VaApiError::Unsupported("poc overflow".to_owned()))?;
            expected = cycles * i64::from(sps.expected_delta_per_pic_order_cnt_cycle);
            for offset in sps.offset_for_ref_frame.iter().take(in_cycle + 1) {
                expected += i64::from(*offset);
            }
        }
        if pic.nal_ref_idc == 0 {
            expected += i64::from(sps.offset_for_non_ref_pic);
        }
        pic.top_field_order_cnt = i32::try_from(expected + i64::from(pic.delta_pic_order_cnt0))
            .map_err(|_| VaApiError::Unsupported("poc overflow".to_owned()))?;
        pic.bottom_field_order_cnt = i32::try_from(
            i64::from(pic.top_field_order_cnt)
                + i64::from(sps.offset_for_top_to_bottom_field)
                + i64::from(pic.delta_pic_order_cnt1),
        )
        .map_err(|_| VaApiError::Unsupported("poc overflow".to_owned()))?;
        Ok(())
    }

    /// Type-2 counts per 8.2.1.3.
    fn derive_type2(
        &self,
        pic: &mut PictureData,
        sps: &Sps,
        is_idr: bool,
    ) -> Result<(), VaApiError> {
        pic.frame_num_offset = self.frame_num_offset(pic, sps, is_idr);
        let temporary = if is_idr {
            0
        } else {
            let base = i64::from(pic.frame_num_offset) + i64::from(pic.frame_num);
            2 * base - i64::from(pic.nal_ref_idc == 0)
        };
        let count = i32::try_from(temporary)
            .map_err(|_| VaApiError::Unsupported("poc overflow".to_owned()))?;
        pic.top_field_order_cnt = count;
        pic.bottom_field_order_cnt = count;
        Ok(())
    }

    /// Previous `(msb, lsb)` pair for type-0 derivation.
    fn type0_base(&self, is_idr: bool) -> (i32, i32) {
        let Some(reference) = self.reference.as_ref() else {
            return (0, 0);
        };
        if is_idr {
            (0, 0)
        } else if reference.mmco5 {
            if reference.bottom {
                (0, 0)
            } else {
                (0, reference.top)
            }
        } else {
            (reference.msb, reference.lsb)
        }
    }

    /// Frame-number offset per 8-6/8-11.
    fn frame_num_offset(&self, pic: &PictureData, sps: &Sps, is_idr: bool) -> u32 {
        let Some(previous) = self.previous.as_ref() else {
            return 0;
        };
        if is_idr {
            return 0;
        }
        let base = if previous.mmco5 {
            0
        } else {
            previous.frame_num_offset
        };
        if previous.frame_num > pic.frame_num {
            base + sps.max_frame_num()
        } else {
            base
        }
    }

    /// Record `pic` as the previous picture.
    fn note_pic(&mut self, pic: &PictureData) {
        self.previous = Some(PreviousNumbers {
            frame_num: pic.frame_num,
            frame_num_offset: pic.frame_num_offset,
            mmco5: pic.has_mmco_5,
        });
    }

    /// Record `pic` as the previous reference picture.
    fn note_ref(&mut self, pic: &PictureData) {
        self.reference = Some(ReferenceCounts {
            msb: pic.pic_order_cnt_msb,
            lsb: pic.pic_order_cnt_lsb,
            top: pic.top_field_order_cnt,
            bottom: matches!(pic.field, Field::Bottom),
            mmco5: pic.has_mmco_5,
        });
    }

    /// Record a stored gap picture as previous picture and reference.
    fn note_stored(&mut self, noted: (u32, u32, bool, i32, i32, i32, Field)) {
        let (frame_num, frame_num_offset, has_mmco_5, msb, lsb, top, field) = noted;
        self.previous = Some(PreviousNumbers {
            frame_num,
            frame_num_offset,
            mmco5: has_mmco_5,
        });
        self.reference = Some(ReferenceCounts {
            msb,
            lsb,
            top,
            bottom: matches!(field, Field::Bottom),
            mmco5: has_mmco_5,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{Frontend, Picture, PocTracker, SliceKey, SurfaceSlot};
    use crate::error::VaApiError;

    use cros_codecs::codec::h264::dpb::ReferencePicLists;
    use cros_codecs::codec::h264::parser::{
        Level, PpsBuilder, RefPicListModification, RefPicMarking, SliceHeader, Sps,
    };
    use cros_codecs::codec::h264::picture::{IsIdr, PictureData, Reference};

    /// Sequence set for 1080p, order-count type `poc_type`.
    fn sps(poc_type: u8) -> Sps {
        Sps {
            profile_idc: 100,
            level_idc: Level::L4,
            chroma_format_idc: 1,
            pic_width_in_mbs_minus1: 119,
            pic_height_in_map_units_minus1: 67,
            frame_mbs_only_flag: true,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: poc_type,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            max_num_ref_frames: 4,
            ..Sps::default()
        }
    }

    /// Picture with `frame_num`, reference flag, and order inputs.
    fn picture(frame_num: u32, nal_ref_idc: u8) -> PictureData {
        let mut pic = PictureData::default();
        pic.pic_order_cnt_type = 0;
        pic.frame_num = frame_num;
        pic.pic_num = i32::try_from(frame_num).expect("small frame number");
        pic.frame_num_wrap = i32::try_from(frame_num).expect("small frame number");
        pic.nal_ref_idc = nal_ref_idc;
        pic.set_reference(
            if nal_ref_idc == 0 {
                Reference::None
            } else {
                Reference::ShortTerm
            },
            true,
        );
        pic
    }

    /// Committable picture over `sps` with `frame_num` and IDR flag.
    fn committable(sps: &Rc<Sps>, frame_num: u32, idr: bool) -> Picture {
        let mut pic = picture(frame_num, 1);
        pic.pic_order_cnt_type = sps.pic_order_cnt_type;
        if idr {
            pic.is_idr = IsIdr::Yes { idr_pic_id: 0 };
            pic.frame_num = 0;
        }
        pic.ref_pic_marking = RefPicMarking::default();
        Picture {
            pic,
            sps: sps.clone(),
            pps: PpsBuilder::new(sps.clone()).build(),
            slices: Vec::new(),
            initial: ReferencePicLists::default(),
        }
    }

    #[test]
    fn poc_type0_counts_lsb_and_delta() {
        let sps = sps(0);
        let mut tracker = PocTracker::default();
        let mut idr = picture(0, 1);
        idr.is_idr = IsIdr::Yes { idr_pic_id: 0 };
        idr.pic_order_cnt_lsb = 0;
        tracker.derive(&mut idr, &sps).expect("idr counts");
        assert_eq!(
            (idr.top_field_order_cnt, idr.bottom_field_order_cnt),
            (0, 0)
        );
        assert_eq!(idr.pic_order_cnt, 0);
        tracker.note_pic(&idr);
        tracker.note_ref(&idr);

        let mut next = picture(1, 1);
        next.pic_order_cnt_lsb = 4;
        next.delta_pic_order_cnt_bottom = 2;
        tracker.derive(&mut next, &sps).expect("p counts");
        assert_eq!(
            (next.top_field_order_cnt, next.bottom_field_order_cnt),
            (4, 6)
        );
        assert_eq!(next.pic_order_cnt, 4);
    }

    #[test]
    fn poc_type0_wraps_msb() {
        let mut base = sps(0);
        base.log2_max_pic_order_cnt_lsb_minus4 = 1;
        let sps = base;
        let mut tracker = PocTracker::default();

        let mut idr = picture(0, 1);
        idr.is_idr = IsIdr::Yes { idr_pic_id: 0 };
        tracker.derive(&mut idr, &sps).expect("idr counts");
        assert_eq!(idr.top_field_order_cnt, 0);
        tracker.note_pic(&idr);
        tracker.note_ref(&idr);

        for (frame_num, lsb, top) in [(1, 16, 16), (2, 28, 28), (3, 2, 34)] {
            let mut pic = picture(frame_num, 1);
            pic.pic_order_cnt_lsb = lsb;
            tracker.derive(&mut pic, &sps).expect("lsb counts");
            assert_eq!(pic.top_field_order_cnt, top, "lsb {lsb}");
            if frame_num == 3 {
                assert_eq!(pic.pic_order_cnt_msb, 32);
            }
            tracker.note_pic(&pic);
            tracker.note_ref(&pic);
        }
    }

    #[test]
    fn poc_type1_sums_cycle_offsets_to_position() {
        let mut sps = sps(1);
        sps.num_ref_frames_in_pic_order_cnt_cycle = 2;
        sps.offset_for_ref_frame[0] = 2;
        sps.offset_for_ref_frame[1] = 2;
        sps.expected_delta_per_pic_order_cnt_cycle = 4;
        let mut tracker = PocTracker::default();

        let mut idr = picture(0, 1);
        idr.pic_order_cnt_type = 1;
        idr.is_idr = IsIdr::Yes { idr_pic_id: 0 };
        tracker.derive(&mut idr, &sps).expect("idr counts");
        assert_eq!(idr.top_field_order_cnt, 0);
        tracker.note_pic(&idr);
        tracker.note_ref(&idr);

        for (frame_num, expected) in [(1, 2), (2, 4), (3, 6)] {
            let mut pic = picture(frame_num, 1);
            pic.pic_order_cnt_type = 1;
            tracker.derive(&mut pic, &sps).expect("cycle counts");
            assert_eq!(pic.top_field_order_cnt, expected, "frame {frame_num}");
            tracker.note_pic(&pic);
            tracker.note_ref(&pic);
        }
    }

    #[test]
    fn poc_type2_counts_double_frame_num() {
        let sps = sps(2);
        let mut tracker = PocTracker::default();
        let mut idr = picture(0, 1);
        idr.pic_order_cnt_type = 2;
        idr.is_idr = IsIdr::Yes { idr_pic_id: 0 };
        tracker.derive(&mut idr, &sps).expect("idr counts");
        assert_eq!(idr.top_field_order_cnt, 0);
        tracker.note_pic(&idr);
        tracker.note_ref(&idr);

        let mut reference = picture(1, 1);
        reference.pic_order_cnt_type = 2;
        tracker.derive(&mut reference, &sps).expect("ref counts");
        assert_eq!(reference.top_field_order_cnt, 2);
        tracker.note_pic(&reference);
        tracker.note_ref(&reference);

        let mut non_ref = picture(2, 0);
        non_ref.pic_order_cnt_type = 2;
        tracker.derive(&mut non_ref, &sps).expect("non-ref counts");
        assert_eq!(non_ref.top_field_order_cnt, 3);
    }

    #[test]
    fn poc_rejects_unknown_type() {
        let sps = sps(0);
        let mut tracker = PocTracker::default();
        let mut pic = picture(0, 1);
        pic.pic_order_cnt_type = 3;
        assert!(matches!(
            tracker.derive(&mut pic, &sps),
            Err(VaApiError::Unsupported(_))
        ));
    }

    #[test]
    fn slice_key_splits_on_clause_fields() {
        let sps = sps(0);
        let header = SliceHeader::default();
        let open = SliceKey::new(&header, &sps, 1, false);
        assert!(!open.opens_new_picture(&open));

        let mut changed = header.clone();
        changed.frame_num = 1;
        assert!(open.opens_new_picture(&SliceKey::new(&changed, &sps, 1, false)));

        let mut changed = header.clone();
        changed.pic_order_cnt_lsb = 2;
        assert!(open.opens_new_picture(&SliceKey::new(&changed, &sps, 1, false)));

        assert!(open.opens_new_picture(&SliceKey::new(&header, &sps, 0, false)));
        assert!(open.opens_new_picture(&SliceKey::new(&header, &sps, 1, true)));
    }

    #[test]
    fn shift_insert_compacts_and_truncates() {
        let mut list = vec![10usize, 20, 30];
        Frontend::shift_insert(&mut list, 0, 99, 2, |_| true).expect("insert");
        assert_eq!(list, vec![99, 10, 20]);

        let mut list = vec![10usize, 20, 30];
        Frontend::shift_insert(&mut list, 1, 99, 2, |index| index != 20).expect("insert");
        assert_eq!(list, vec![10, 99, 30]);

        let mut list = vec![10usize];
        assert!(Frontend::shift_insert(&mut list, 5, 99, 2, |_| true).is_err());
    }

    /// Frontend with two short-term refs of picnums 5 and 3.
    fn frontend_with_refs() -> (Frontend, Rc<Sps>) {
        let sps = Rc::new(sps(0));
        let mut frontend = Frontend::new();
        frontend.dpb.set_limits(16, 16);
        for (frame_num, slot) in [(5, 0), (3, 1)] {
            let mut pic = picture(frame_num, 1);
            let poc = i32::try_from(frame_num).expect("small frame number") * 2;
            pic.top_field_order_cnt = poc;
            pic.bottom_field_order_cnt = poc;
            pic.pic_order_cnt = poc;
            frontend
                .dpb
                .store_picture(pic.into_rc(), Some(SurfaceSlot::new(slot)))
                .expect("dpb room");
        }
        (frontend, sps)
    }

    #[test]
    fn modify_reorders_short_term_to_front() {
        let (frontend, _) = frontend_with_refs();
        let initial = vec![0usize, 1];
        let header = SliceHeader {
            ref_pic_list_modification_flag_l0: true,
            num_ref_idx_l0_active_minus1: 1,
            ref_pic_list_modification_l0: vec![
                RefPicListModification {
                    modification_of_pic_nums_idc: 0,
                    abs_diff_pic_num_minus1: 3,
                    ..Default::default()
                },
                RefPicListModification {
                    modification_of_pic_nums_idc: 3,
                    ..Default::default()
                },
            ],
            ..SliceHeader::default()
        };
        let list = frontend
            .modify(&header, &initial, false, 7, 16)
            .expect("reorder");
        assert_eq!(list, vec![1, 0]);
    }

    #[test]
    fn modify_reports_absent_reference() {
        let (frontend, _) = frontend_with_refs();
        let header = SliceHeader {
            ref_pic_list_modification_flag_l0: true,
            ref_pic_list_modification_l0: vec![RefPicListModification {
                modification_of_pic_nums_idc: 0,
                abs_diff_pic_num_minus1: 0,
                ..Default::default()
            }],
            ..SliceHeader::default()
        };
        assert!(matches!(
            frontend.modify(&header, &[0, 1], false, 7, 16),
            Err(VaApiError::BrokenReference(_))
        ));
    }

    #[test]
    fn commit_slides_window_past_one_ref() {
        let mut sps = sps(0);
        sps.max_num_ref_frames = 1;
        let sps = Rc::new(sps);
        let mut frontend = Frontend::new();
        frontend.dpb.set_limits(sps.max_dpb_frames(), 16);

        let idr = committable(&sps, 0, true);
        assert!(frontend
            .commit(idr, SurfaceSlot::new(0))
            .expect("idr")
            .is_empty());
        assert_eq!(frontend.dpb.len(), 1);

        let next = committable(&sps, 1, false);
        assert!(frontend
            .commit(next, SurfaceSlot::new(1))
            .expect("p")
            .is_empty());
        let refs: Vec<_> = frontend
            .dpb
            .short_term_refs_iter()
            .map(|entry| entry.pic.borrow().frame_num)
            .collect();
        assert_eq!(refs, vec![1]);
    }

    #[test]
    fn commit_emits_bumping_in_output_order() {
        let mut sps = sps(0);
        sps.max_num_ref_frames = 2;
        let sps = Rc::new(sps);
        let mut frontend = Frontend::new();
        frontend.dpb.set_limits(3, 1);

        for (slot, frame_num, poc, idr) in [(0, 0, 0, true), (1, 1, 2, false), (2, 2, 4, false)] {
            let mut committable = committable(&sps, frame_num, idr);
            let num = i32::try_from(frame_num).expect("small frame number");
            committable.pic.frame_num_wrap = num;
            committable.pic.pic_num = num;
            committable.pic.pic_order_cnt = poc;
            committable.pic.top_field_order_cnt = poc;
            committable.pic.bottom_field_order_cnt = poc;
            let bumped = frontend
                .commit(committable, SurfaceSlot::new(slot))
                .expect("room");
            assert!(bumped.is_empty());
        }

        let mut delayed = committable(&sps, 3, false);
        delayed.pic.frame_num_wrap = 3;
        delayed.pic.pic_num = 3;
        delayed.pic.pic_order_cnt = 6;
        delayed.pic.top_field_order_cnt = 6;
        delayed.pic.bottom_field_order_cnt = 6;
        let bumped = frontend.commit(delayed, SurfaceSlot::new(3)).expect("p");
        assert_eq!(bumped, vec![SurfaceSlot::new(0)]);
    }
}
