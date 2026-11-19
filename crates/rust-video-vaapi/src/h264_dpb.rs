// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::Ref;
use std::cell::RefMut;
use std::fmt;

use log::debug;

use crate::h264::Sps;
use crate::h264::{frame_num_wrap, initial_ref_lists, mmco_short_target, RefCandidate, SliceType};
use crate::h264_picture::IsIdr;
use crate::h264_picture::PictureData;
use crate::h264_picture::RcPictureData;
use crate::h264_picture::Reference;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MaxLongTermFrameIdx {
    #[default]
    NoLongTermFrameIndices,
    Idx(u32),
}

impl MaxLongTermFrameIdx {
    /// Create a value from `max_long_term_frame_idx_plus1`.
    pub fn from_value_plus1(max_long_term_frame_idx_plus1: u32) -> Self {
        match max_long_term_frame_idx_plus1 {
            0 => Self::NoLongTermFrameIndices,
            i @ 1.. => Self::Idx(i - 1),
        }
    }

    /// Convert this value to the representation used by `max_long_term_frame_idx_plus1`.
    pub fn to_value_plus1(self) -> u32 {
        match self {
            Self::NoLongTermFrameIndices => 0,
            Self::Idx(i) => i + 1,
        }
    }
}

impl PartialEq<u32> for MaxLongTermFrameIdx {
    fn eq(&self, other: &u32) -> bool {
        match self {
            MaxLongTermFrameIdx::NoLongTermFrameIndices => false,
            MaxLongTermFrameIdx::Idx(idx) => idx.eq(other),
        }
    }
}

impl PartialOrd<u32> for MaxLongTermFrameIdx {
    fn partial_cmp(&self, other: &u32) -> Option<std::cmp::Ordering> {
        match self {
            MaxLongTermFrameIdx::NoLongTermFrameIndices => Some(std::cmp::Ordering::Less),
            MaxLongTermFrameIdx::Idx(idx) => Some(idx.cmp(other)),
        }
    }
}

/// All the reference picture lists used to decode a picture.
#[derive(Default)]
pub struct ReferencePicLists {
    /// Reference picture list for P slices. Retains the same meaning as in the
    /// specification. Points into the pictures stored in the DPB. Derived once
    /// per picture.
    pub p_list: Vec<usize>,
    /// Reference picture list 0 for B slices. Retains the same meaning as in
    /// the specification. Points into the pictures stored in the DPB. Derived
    /// once per picture.
    pub b_list0: Vec<usize>,
    /// Reference picture list 1 for B slices. Retains the same meaning as in
    /// the specification. Points into the pictures stored in the DPB. Derived
    /// once per picture.
    pub b_list1: Vec<usize>,
}

/// A single entry in the DPB.
#[derive(Clone)]
pub struct DpbEntry<T> {
    /// `PictureData` of the frame in this entry.
    pub pic: RcPictureData,
    /// Reference to the decoded frame, ensuring that it doesn't get reused while in the DPB.
    pub reference: Option<T>,
    /// Decoded frame promise. It will be set when the frame enters the DPB, and taken during the
    /// bump process.
    pub decoded_frame: Option<T>,
    /// Whether the picture is still waiting to be bumped and displayed.
    needed_for_output: bool,
}

impl<T> DpbEntry<T> {
    /// Returns `true` is the entry is eligible to be bumped.
    ///
    /// An entry can be bumped if its `needed_for_output` flag is true. The
    /// tier decodes progressive frames only, so every entry is fully decoded.
    fn is_bumpable(&self) -> bool {
        self.needed_for_output
    }
}

pub struct Dpb<T> {
    /// List of `PictureData` and backend handles to decoded pictures.
    entries: Vec<DpbEntry<T>>,
    /// The maximum number of pictures that can be stored.
    max_num_pics: usize,
    /// Indicates an upper bound for the number of frames buffers, in the
    /// decoded picture buffer (DPB), that are required for storing frames,
    /// complementary field pairs, and non-paired fields before output. It is a
    /// requirement of bitstream conformance that the maximum number of frames,
    /// complementary field pairs, or non-paired fields that precede any frame,
    /// complementary field pair, or non-paired field in the coded video
    /// sequence in decoding order and follow it in output order shall be less
    /// than or equal to `max_num_reorder_frames`.
    max_num_reorder_frames: usize,
}

#[derive(Debug)]
pub enum StorePictureError {
    DpbIsFull,
}

impl fmt::Display for StorePictureError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DPB is full")
    }
}

impl std::error::Error for StorePictureError {}

#[derive(Debug)]
pub enum MmcoError {
    NoShortTermPic,
    ExpectedMarked,
    ExpectedExisting,
}

impl fmt::Display for MmcoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MmcoError::NoShortTermPic => {
                write!(f, "could not find ShortTerm picture to mark in the DPB")
            }
            MmcoError::ExpectedMarked => {
                write!(
                    f,
                    "a ShortTerm picture was expected to be marked for MMCO=3"
                )
            }
            MmcoError::ExpectedExisting => {
                write!(f, "picture cannot be marked as nonexisting for MMCO=3")
            }
        }
    }
}

impl std::error::Error for MmcoError {}

impl<T: Clone> Dpb<T> {
    /// Returns an iterator over the underlying H264 pictures stored in the
    /// DPB.
    fn pictures(&self) -> impl Iterator<Item = Ref<'_, PictureData>> {
        self.entries.iter().map(|h| h.pic.borrow())
    }

    /// Returns a mutable iterator over the underlying H264 pictures stored in
    /// the DPB.
    fn pictures_mut(&mut self) -> impl Iterator<Item = RefMut<'_, PictureData>> {
        self.entries.iter().map(|h| h.pic.borrow_mut())
    }

    /// Returns the length of the DPB.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a reference to the whole DPB entries.
    pub fn entries(&self) -> &Vec<DpbEntry<T>> {
        &self.entries
    }

    /// Set the DPB's limits in terms of maximum number or pictures.
    pub fn set_limits(&mut self, max_num_pics: usize, max_num_reorder_frames: usize) {
        self.max_num_pics = max_num_pics;
        self.max_num_reorder_frames = max_num_reorder_frames;
    }

    // Returns the number of reference frames, counting the first field only if
    // dealing with interlaced content.
    pub fn num_ref_frames(&self) -> usize {
        self.pictures().filter(|p| p.is_ref()).count()
    }

    /// Find the short term reference picture with the lowest `frame_num_wrap`
    /// value.
    pub fn find_short_term_lowest_frame_num_wrap(&self) -> Option<&DpbEntry<T>> {
        let lowest = self
            .entries
            .iter()
            .filter(|h| {
                let p = h.pic.borrow();
                matches!(p.reference(), Reference::ShortTerm)
            })
            .min_by_key(|h| {
                let p = h.pic.borrow();
                p.frame_num_wrap
            });

        lowest
    }

    /// Mark all pictures in the DPB as unused for reference.
    pub fn mark_all_as_unused_for_ref(&mut self) {
        for mut picture in self.pictures_mut() {
            picture.set_reference(Reference::None);
        }
    }

    /// Remove unused pictures from the DPB. A picture is not going to be used
    /// anymore if it's a) not a reference and b) not needed for output
    fn remove_unused(&mut self) {
        self.entries.retain(|entry| {
            let pic = entry.pic.borrow();
            let discard = !pic.is_ref() && !entry.needed_for_output;

            if discard {
                log::debug!("Removing unused picture {pic:#?}");
            }

            !discard
        });
    }

    /// Find a short term reference picture with the given `pic_num` value.
    fn find_short_term_with_pic_num_pos(&self, pic_num: i32) -> Option<usize> {
        let position = self
            .pictures()
            .position(|p| matches!(p.reference(), Reference::ShortTerm) && p.pic_num == pic_num);

        log::debug!("find_short_term_with_pic_num: {pic_num}, found position {position:?}");

        position
    }

    /// Find a short term reference picture with the given `pic_num` value.
    pub fn find_short_term_with_pic_num(&self, pic_num: i32) -> Option<&DpbEntry<T>> {
        let position = self.find_short_term_with_pic_num_pos(pic_num)?;
        Some(&self.entries[position])
    }

    /// Find a long term reference picture with the given `long_term_pic_num`
    /// value.
    fn find_long_term_with_long_term_pic_num_pos(&self, long_term_pic_num: u32) -> Option<usize> {
        let position = self.pictures().position(|p| {
            matches!(p.reference(), Reference::LongTerm) && p.long_term_pic_num == long_term_pic_num
        });

        log::debug!(
            "find_long_term_with_long_term_pic_num: {long_term_pic_num}, found position {position:?}"
        );

        position
    }

    /// Find a long term reference picture with the given `long_term_pic_num`
    /// value.
    pub fn find_long_term_with_long_term_pic_num(
        &self,
        long_term_pic_num: u32,
    ) -> Option<&DpbEntry<T>> {
        let position = self.find_long_term_with_long_term_pic_num_pos(long_term_pic_num)?;
        Some(&self.entries[position])
    }

    /// Store `picture` and its backend handle in the DPB.
    pub fn store_picture(
        &mut self,
        picture: RcPictureData,
        handle: Option<T>,
    ) -> Result<(), StorePictureError> {
        if self.entries.len() >= self.max_num_pics {
            return Err(StorePictureError::DpbIsFull);
        }

        let pic = picture.borrow();

        // C.4.2. Decoding of gaps in frame_num and storage of "non-existing"
        // pictures
        let needed_for_output = !pic.nonexisting;

        debug!(
            "Stored picture POC {:?}, the DPB length is {:?}",
            pic.pic_order_cnt,
            self.entries.len()
        );
        drop(pic);

        self.entries.push(DpbEntry {
            pic: picture,
            reference: handle.clone(),
            decoded_frame: handle,
            needed_for_output,
        });

        Ok(())
    }

    /// Whether the DPB has an empty slot for a new picture.
    pub fn has_empty_frame_buffer(&self) -> bool {
        self.entries.len() < self.max_num_pics
    }

    /// Whether the DPB needs bumping, as described by clauses 1, 4, 5, 6 of
    /// C.4.5.3 "Bumping" process.
    pub fn needs_bumping(&self, to_insert: &PictureData) -> bool {
        // In C.4.5.3 we handle clauses 2 and 3 separately. All other clauses
        // check for an empty frame buffer first. Here we handle:
        //    - There is no empty frame buffer and a empty frame buffer is
        //    needed for storage of an inferred "non-existing" frame.
        //
        //    - There is no empty frame buffer and an empty frame buffer is
        //    needed for storage of a decoded (non-IDR) reference picture.
        //
        //    - There is no empty frame buffer and the current picture is a non-
        //    reference picture that is not the second field of a complementary
        //    non-reference field pair and there are pictures in the DPB that
        //    are marked as "needed for output" that precede the current
        //    non-reference picture in output order.
        //
        // Clauses 2 and 3 are handled by H264Codec::handle_picture and
        // H264Codec::finish_picture, respectively.
        if self.has_empty_frame_buffer() {
            return false;
        }

        if to_insert.nonexisting {
            return true;
        }

        let non_idr_ref = to_insert.is_ref() && matches!(to_insert.is_idr, IsIdr::No);
        if non_idr_ref {
            return true;
        }

        let lowest_poc = match self.find_lowest_poc_for_bumping() {
            Some(handle) => handle.pic.borrow().pic_order_cnt,
            None => return false,
        };

        to_insert.pic_order_cnt > lowest_poc
    }

    /// Find the lowest POC in the DPB that can be bumped.
    fn find_lowest_poc_for_bumping(&self) -> Option<&DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|entry| entry.is_bumpable())
            .min_by_key(|handle| handle.pic.borrow().pic_order_cnt)
    }

    /// Find the lowest POC in the DPB that can be bumped and return a mutable reference.
    fn find_lowest_poc_for_bumping_mut(&mut self) -> Option<&mut DpbEntry<T>> {
        self.entries
            .iter_mut()
            .filter(|entry| entry.is_bumpable())
            .min_by_key(|handle| handle.pic.borrow().pic_order_cnt)
    }

    /// Bump the dpb, returning a picture as per the bumping process described in C.4.5.3.
    /// Note that this picture will still be referenced by its pair, if any.
    fn bump(&mut self) -> Option<T> {
        loop {
            let dpb_entry = self.find_lowest_poc_for_bumping_mut()?;
            let handle = dpb_entry.decoded_frame.take();
            let pic = dpb_entry.pic.borrow();

            debug!("Bumping picture {pic:#?} from the dpb");

            dpb_entry.needed_for_output = false;
            drop(pic);

            if handle.is_some() {
                return handle;
            }
        }
    }

    /// Drains the DPB by continuously invoking the bumping process.
    pub fn drain(&mut self) -> Vec<T> {
        debug!("Draining the DPB.");

        let mut pics = vec![];

        while let Some(pic) = self.bump() {
            pics.push(pic);
        }

        self.clear();

        pics
    }

    /// Clears the DPB, dropping all the pictures.
    pub fn clear(&mut self) {
        debug!("Clearing the DPB");

        let max_num_pics = self.max_num_pics;

        *self = Dpb::default();

        self.max_num_pics = max_num_pics;
    }

    /// Returns an iterator of short term refs.
    pub fn short_term_refs_iter(&self) -> impl Iterator<Item = &DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|&handle| matches!(handle.pic.borrow().reference(), Reference::ShortTerm))
    }

    /// Returns an iterator of long term refs.
    pub fn long_term_refs_iter(&self) -> impl Iterator<Item = &DpbEntry<T>> {
        self.entries
            .iter()
            .filter(|&handle| matches!(handle.pic.borrow().reference(), Reference::LongTerm))
    }

    pub fn update_pic_nums(&mut self, frame_num: u32, max_frame_num: u32) {
        for mut pic in self.pictures_mut() {
            if !pic.is_ref() {
                continue;
            }

            if *pic.reference() == Reference::LongTerm {
                pic.long_term_pic_num = pic.long_term_frame_idx;
            } else {
                pic.frame_num_wrap = frame_num_wrap(pic.frame_num, frame_num, max_frame_num);
                pic.pic_num = pic.frame_num_wrap;
            }
        }
    }

    /// Bumps the DPB if needed. DPB bumping is described on C.4.5.3.
    pub fn bump_as_needed(&mut self, current_pic: &PictureData) -> Vec<T> {
        let mut pics = vec![];
        while self.needs_bumping(current_pic) && self.len() >= self.max_num_reorder_frames {
            match self.bump() {
                Some(pic) => pics.push(pic),
                None => return pics,
            }
            self.remove_unused();
        }

        pics
    }

    /// Bumps every picture still needed for output, in output order.
    ///
    /// The no-reorder fast path: output order matches decode order,
    /// so everything ready emits on the completing commit. Reference
    /// pictures stay stored; only their output handle leaves.
    pub(crate) fn bump_ready(&mut self) -> Vec<T> {
        let mut pics = Vec::new();
        while let Some(pic) = self.bump() {
            pics.push(pic);
            self.remove_unused();
        }
        pics
    }

    // 8.2.5.3
    pub fn sliding_window_marking(&mut self, sps: &Sps) {
        let mut num_ref_pics = self.num_ref_frames();
        let max_num_ref_frames = std::cmp::max(1, sps.max_num_ref_frames as usize);

        if num_ref_pics < max_num_ref_frames {
            return;
        }

        while num_ref_pics >= max_num_ref_frames {
            if let Some(to_unmark) = self.find_short_term_lowest_frame_num_wrap() {
                to_unmark.pic.borrow_mut().set_reference(Reference::None);
                num_ref_pics -= 1;
            } else {
                log::warn!("could not find a ShortTerm picture to unmark in the DPB");
                break;
            }
        }

        self.remove_unused();
    }

    pub fn mmco_op_1(
        &mut self,
        pic: &PictureData,
        difference_of_pic_nums_minus1: u32,
    ) -> Result<(), MmcoError> {
        let target = mmco_short_target(difference_of_pic_nums_minus1, pic.frame_num);
        let Ok(target) = i32::try_from(target) else {
            return Err(MmcoError::NoShortTermPic);
        };

        log::debug!("MMCO op 1 for pic_num_x {target}");
        log::trace!("Dpb state before MMCO=1: {self:#?}");

        let to_mark = self
            .find_short_term_with_pic_num(target)
            .ok_or(MmcoError::NoShortTermPic)?;

        to_mark.pic.borrow_mut().set_reference(Reference::None);

        Ok(())
    }

    pub fn mmco_op_2(&mut self, long_term_pic_num: u32) -> Result<(), MmcoError> {
        log::debug!("MMCO op 2 for long_term_pic_num {long_term_pic_num}");

        log::trace!("Dpb state before MMCO=2: {self:#?}");

        let to_mark = self
            .find_long_term_with_long_term_pic_num(long_term_pic_num)
            .ok_or(MmcoError::NoShortTermPic)?;

        to_mark.pic.borrow_mut().set_reference(Reference::None);

        Ok(())
    }

    pub fn mmco_op_3(
        &mut self,
        pic: &PictureData,
        difference_of_pic_nums_minus1: u32,
        long_term_frame_idx: u32,
    ) -> Result<(), MmcoError> {
        let target = mmco_short_target(difference_of_pic_nums_minus1, pic.frame_num);
        let Ok(target) = i32::try_from(target) else {
            return Err(MmcoError::NoShortTermPic);
        };

        log::debug!("MMCO op 3 for pic_num_x {target}");
        log::trace!("Dpb state before MMCO=3: {self:#?}");

        let to_mark_as_long_pos = self
            .find_short_term_with_pic_num_pos(target)
            .ok_or(MmcoError::NoShortTermPic)?;
        let to_mark_as_long = &self.entries[to_mark_as_long_pos].pic;

        if !matches!(to_mark_as_long.borrow().reference(), Reference::ShortTerm) {
            return Err(MmcoError::ExpectedMarked);
        }

        if to_mark_as_long.borrow().nonexisting {
            return Err(MmcoError::ExpectedExisting);
        }

        for mut picture in self.pictures_mut() {
            let long_already_assigned = matches!(picture.reference(), Reference::LongTerm)
                && picture.long_term_frame_idx == long_term_frame_idx;

            if long_already_assigned {
                // When LongTermFrameIdx equal to
                // long_term_frame_idx is already assigned to a
                // long-term reference frame, that frame is marked as
                // "unused for reference"
                picture.set_reference(Reference::None);
                break;
            }
        }

        let to_mark_as_long = &self.entries[to_mark_as_long_pos].pic;
        to_mark_as_long
            .borrow_mut()
            .set_reference(Reference::LongTerm);
        to_mark_as_long.borrow_mut().long_term_frame_idx = long_term_frame_idx;

        Ok(())
    }

    /// Returns the new `max_long_term_frame_idx`.
    pub fn mmco_op_4(&mut self, max_long_term_frame_idx_plus1: u32) -> MaxLongTermFrameIdx {
        let max = MaxLongTermFrameIdx::from_value_plus1(max_long_term_frame_idx_plus1);
        log::debug!("MMCO op 4, max_long_term_frame_idx: {max:?}");

        log::trace!("Dpb state before MMCO=4: {self:#?}");

        for mut dpb_pic in self
            .pictures_mut()
            .filter(|pic| matches!(pic.reference(), Reference::LongTerm))
            .filter(|pic| max < pic.long_term_frame_idx)
        {
            dpb_pic.set_reference(Reference::None);
        }

        max
    }

    /// Returns the new `max_long_term_frame_idx`.
    pub fn mmco_op_5(&mut self, pic: &mut PictureData) -> MaxLongTermFrameIdx {
        log::debug!("MMCO op 5, marking all pictures in the DPB as unused for reference");
        log::trace!("Dpb state before MMCO=5: {self:#?}");

        self.mark_all_as_unused_for_ref();

        pic.has_mmco_5 = true;

        // A picture including a memory_management_control_operation equal to 5
        // shall have frame_num constraints as described above and, after the
        // decoding of the current picture and the processing of the memory
        // management control operations, the picture shall be inferred to have
        // had frame_num equal to 0 for all subsequent use in the decoding
        // process, except as specified in clause 7.4.1.2.4.
        pic.frame_num = 0;

        // When the current picture includes a
        // memory_management_control_operation equal to 5, after the decoding of
        // the current picture, tempPicOrderCnt is set equal to PicOrderCnt(
        // CurrPic ), TopFieldOrderCnt of the current picture (if any) is set
        // equal to TopFieldOrderCnt − tempPicOrderCnt, and BottomFieldOrderCnt
        // of the current picture (if any) is set equal to BottomFieldOrderCnt −
        // tempPicOrderCnt
        pic.top_field_order_cnt -= pic.pic_order_cnt;
        pic.bottom_field_order_cnt -= pic.pic_order_cnt;
        pic.pic_order_cnt = std::cmp::min(pic.top_field_order_cnt, pic.bottom_field_order_cnt);

        MaxLongTermFrameIdx::NoLongTermFrameIndices
    }

    pub fn mmco_op_6(&mut self, pic: &mut PictureData, long_term_frame_idx: u32) {
        log::debug!("MMCO op 6, long_term_frame_idx: {long_term_frame_idx}");
        log::trace!("Dpb state before MMCO=6: {self:#?}");

        for mut dpb_pic in self.pictures_mut() {
            // When a variable LongTermFrameIdx equal to long_term_frame_idx is
            // already assigned to a long-term reference frame, that frame is
            // marked as "unused for reference".
            if matches!(dpb_pic.reference(), Reference::LongTerm)
                && dpb_pic.long_term_frame_idx == long_term_frame_idx
            {
                dpb_pic.set_reference(Reference::None);

                break;
            }
        }

        pic.set_reference(Reference::LongTerm);
        pic.long_term_frame_idx = long_term_frame_idx;
    }

    /// Returns the lists of reference pictures for `pic`.
    pub fn build_ref_pic_lists(&self, pic: &PictureData) -> ReferencePicLists {
        let num_refs = self
            .pictures()
            .filter(|p| p.is_ref() && !p.nonexisting)
            .count();

        // 8.2.4.2.1 ~ 8.2.4.2.4: When this process is invoked, there shall be
        // at least one reference frame or complementary reference field pair
        // that is currently marked as "used for reference" (i.e., as "used for
        // short-term reference" or "used for long-term reference") and is not
        // marked as "non-existing".
        if num_refs == 0 {
            return ReferencePicLists::default();
        }

        // P lists take every reference; B lists drop non-existing
        // long-term pictures, and non-existing short-term ones under
        // type 0.
        let candidates = |b_list: bool| {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    let picture = entry.pic.borrow();
                    if !picture.is_ref() {
                        return false;
                    }
                    if !b_list {
                        return true;
                    }
                    if matches!(picture.reference(), Reference::LongTerm) {
                        return !picture.nonexisting;
                    }
                    pic.pic_order_cnt_type != 0 || !picture.nonexisting
                })
                .map(|(index, entry)| {
                    let picture = entry.pic.borrow();
                    let long_term_idx = if matches!(picture.reference(), Reference::LongTerm) {
                        Some(picture.long_term_frame_idx)
                    } else {
                        None
                    };
                    (
                        index,
                        RefCandidate {
                            frame_num: picture.frame_num,
                            pic_num: picture.pic_num,
                            poc: picture.pic_order_cnt,
                            long_term_idx,
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let (p_indices, p_cands): (Vec<usize>, Vec<RefCandidate>) =
            candidates(false).into_iter().unzip();
        let (p_list, _) = initial_ref_lists(&p_cands, SliceType::P, pic.pic_order_cnt);
        let (b_indices, b_cands): (Vec<usize>, Vec<RefCandidate>) =
            candidates(true).into_iter().unzip();
        let (b_list0, b_list1) = initial_ref_lists(&b_cands, SliceType::B, pic.pic_order_cnt);
        ReferencePicLists {
            p_list: p_list.iter().map(|pos| p_indices[*pos]).collect(),
            b_list0: b_list0.iter().map(|pos| b_indices[*pos]).collect(),
            b_list1: b_list1.iter().map(|pos| b_indices[*pos]).collect(),
        }
    }
}

impl<T> Default for Dpb<T> {
    fn default() -> Self {
        // See https://github.com/rust-lang/rust/issues/26925 on why this can't
        // be derived.
        Self {
            entries: Vec::default(),
            max_num_pics: Default::default(),
            max_num_reorder_frames: Default::default(),
        }
    }
}

impl<T> std::fmt::Debug for Dpb<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pics = self
            .entries
            .iter()
            .map(|h| &h.pic)
            .enumerate()
            .collect::<Vec<_>>();
        f.debug_struct("Dpb")
            .field("entries", &pics)
            .field("max_num_pics", &self.max_num_pics)
            .field("max_num_reorder_frames", &self.max_num_reorder_frames)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Dpb;
    use crate::h264_picture::{PictureData, Reference};

    #[test]
    fn bump_ready_emits_in_output_order_and_keeps_refs() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        for (order, slot, reference) in [(2, 0u32, false), (0, 1, true), (4, 2, false)] {
            let mut pic = PictureData::default();
            pic.pic_order_cnt = order;
            if reference {
                pic.set_reference(Reference::ShortTerm);
            }
            dpb.store_picture(pic.into_rc(), Some(slot)).expect("room");
        }
        assert_eq!(dpb.bump_ready(), vec![1, 0, 2]);
        // References stay stored; output-only pictures leave.
        assert_eq!(dpb.len(), 1);
    }

    #[test]
    fn update_pic_nums_wraps_short_and_maps_long() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        for frame_num in [5u32, 1] {
            let mut pic = PictureData::default();
            pic.frame_num = frame_num;
            pic.set_reference(Reference::ShortTerm);
            dpb.store_picture(pic.into_rc(), Some(frame_num))
                .expect("room");
        }
        let mut long = PictureData::default();
        long.long_term_frame_idx = 3;
        long.set_reference(Reference::LongTerm);
        dpb.store_picture(long.into_rc(), Some(9)).expect("room");
        dpb.update_pic_nums(2, 16);
        assert_eq!(dpb.entries()[0].pic.borrow().frame_num_wrap, -11);
        assert_eq!(dpb.entries()[0].pic.borrow().pic_num, -11);
        assert_eq!(dpb.entries()[1].pic.borrow().pic_num, 1);
        assert_eq!(dpb.entries()[2].pic.borrow().long_term_pic_num, 3);
    }

    /// Stored short-term picture over `dpb`.
    fn stored_short(dpb: &mut Dpb<u32>, slot: u32, frame_num: u32, poc: i32, nonexisting: bool) {
        let mut picture = PictureData::default();
        picture.frame_num = frame_num;
        picture.pic_num = i32::try_from(frame_num).expect("small frame number");
        picture.pic_order_cnt = poc;
        picture.nonexisting = nonexisting;
        picture.set_reference(Reference::ShortTerm);
        dpb.store_picture(picture.into_rc(), Some(slot)).expect("room");
    }

    /// Stored long-term picture over `dpb`.
    fn stored_long(dpb: &mut Dpb<u32>, slot: u32, idx: u32, poc: i32, nonexisting: bool) {
        let mut picture = PictureData::default();
        picture.pic_order_cnt = poc;
        picture.long_term_frame_idx = idx;
        picture.long_term_pic_num = idx;
        picture.nonexisting = nonexisting;
        picture.set_reference(Reference::LongTerm);
        dpb.store_picture(picture.into_rc(), Some(slot)).expect("room");
    }

    #[test]
    fn build_ref_pic_lists_splits_p_and_b() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        stored_short(&mut dpb, 0, 5, 10, false);
        stored_short(&mut dpb, 1, 3, 2, false);
        stored_long(&mut dpb, 2, 1, 4, false);
        stored_short(&mut dpb, 3, 4, 6, true);
        stored_long(&mut dpb, 4, 0, 0, true);
        let mut cur = PictureData::default();
        cur.pic_order_cnt_type = 0;
        cur.pic_order_cnt = 6;
        let lists = dpb.build_ref_pic_lists(&cur);
        assert_eq!(lists.p_list, vec![0, 3, 1, 4, 2]);
        assert_eq!(lists.b_list0, vec![1, 0, 2]);
        assert_eq!(lists.b_list1, vec![0, 1, 2]);
    }

    #[test]
    fn build_ref_pic_lists_swaps_identical_b1() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        stored_short(&mut dpb, 0, 5, 6, false);
        stored_long(&mut dpb, 1, 0, 0, false);
        let mut cur = PictureData::default();
        cur.pic_order_cnt_type = 0;
        cur.pic_order_cnt = 6;
        let lists = dpb.build_ref_pic_lists(&cur);
        assert_eq!(lists.b_list0, vec![0, 1]);
        assert_eq!(lists.b_list1, vec![1, 0]);
    }

    #[test]
    fn mmco_short_target_names_wrapped_pic_num() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        stored_short(&mut dpb, 0, 5, 10, false);
        let mut current = PictureData::default();
        current.frame_num = 7;
        current.pic_num = 7;
        dpb.mmco_op_1(&current, 1).expect("target present");
        assert!(!dpb.entries()[0].pic.borrow().is_ref());
        assert!(dpb.mmco_op_1(&current, u32::MAX).is_err());
        let widest = u32::try_from(i32::MAX).expect("positive bound");
        assert!(dpb.mmco_op_1(&current, widest).is_err());
    }

    #[test]
    fn mmco_op_3_moves_short_to_long_term() {
        let mut dpb = Dpb::default();
        dpb.set_limits(16, 16);
        stored_short(&mut dpb, 0, 5, 10, false);
        let mut current = PictureData::default();
        current.frame_num = 7;
        current.pic_num = 7;
        dpb.mmco_op_3(&current, 1, 2).expect("move");
        let stored = dpb.entries()[0].pic.borrow();
        assert!(matches!(stored.reference(), Reference::LongTerm));
        assert_eq!(stored.long_term_frame_idx, 2);
    }
}
