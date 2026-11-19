// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

use crate::h264::Marking;
use crate::h264_dpb::MaxLongTermFrameIdx;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Frame,
    Top,
    Bottom,
}

impl Field {
    /// Returns the field of opposite parity.
    pub fn opposite(self) -> Self {
        match self {
            Field::Frame => Field::Frame,
            Field::Top => Field::Bottom,
            Field::Bottom => Field::Top,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    #[default]
    None,
    ShortTerm,
    LongTerm,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum IsIdr {
    #[default]
    No,
    Yes {
        idr_pic_id: u16,
    },
}

pub struct PictureData {
    pub pic_order_cnt_type: u8,
    pub top_field_order_cnt: i32,
    pub bottom_field_order_cnt: i32,
    pub pic_order_cnt: i32,
    pub pic_order_cnt_msb: i32,
    pub pic_order_cnt_lsb: i32,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt0: i32,
    pub delta_pic_order_cnt1: i32,

    pub pic_num: i32,
    pub long_term_pic_num: u32,
    pub frame_num: u32,
    pub frame_num_offset: u32,
    pub frame_num_wrap: i32,
    pub long_term_frame_idx: u32,

    pub nal_ref_idc: u8,
    pub is_idr: IsIdr,
    reference: Reference,

    // Does memory management op 5 needs to be executed after this
    // picture has finished decoding?
    pub has_mmco_5: bool,

    // Created by the decoding process for gaps in frame_num.
    // Not for decode or output.
    pub nonexisting: bool,

    pub field: Field,

    // Values from slice_hdr to be used during reference marking and
    // memory management after finishing this picture.
    pub ref_pic_marking: Marking,

    /// Presentation timestamp of the picture.
    pub timestamp: u64,
}

impl Default for PictureData {
    fn default() -> Self {
        Self {
            pic_order_cnt_type: 0,
            top_field_order_cnt: 0,
            bottom_field_order_cnt: 0,
            pic_order_cnt: 0,
            pic_order_cnt_msb: 0,
            pic_order_cnt_lsb: 0,
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt0: 0,
            delta_pic_order_cnt1: 0,
            pic_num: 0,
            long_term_pic_num: 0,
            frame_num: 0,
            frame_num_offset: 0,
            frame_num_wrap: 0,
            long_term_frame_idx: 0,
            nal_ref_idc: 0,
            is_idr: IsIdr::No,
            reference: Reference::None,
            has_mmco_5: false,
            nonexisting: false,
            field: Field::Frame,
            ref_pic_marking: Marking::Silent,
            timestamp: 0,
        }
    }
}

/// A `PictureData` within a `Rc<RefCell>`.
#[derive(Default, Debug, Clone)]
pub struct RcPictureData {
    pic: Rc<RefCell<PictureData>>,
}

impl Deref for RcPictureData {
    type Target = Rc<RefCell<PictureData>>;

    fn deref(&self) -> &Self::Target {
        &self.pic
    }
}

impl PictureData {
    pub fn new_non_existing(frame_num: u32, timestamp: u64) -> Self {
        PictureData {
            frame_num,
            nonexisting: true,
            nal_ref_idc: 1,
            field: Field::Frame,
            pic_num: frame_num.cast_signed(),
            reference: Reference::ShortTerm,
            timestamp,
            ..Default::default()
        }
    }

    /// Whether the current picture is a reference, either `ShortTerm` or `LongTerm`.
    pub fn is_ref(&self) -> bool {
        !matches!(self.reference, Reference::None)
    }

    /// Returns a reference to the picture's Reference
    pub fn reference(&self) -> &Reference {
        &self.reference
    }

    /// Mark the picture as a reference picture.
    pub fn set_reference(&mut self, reference: Reference) {
        log::debug!("Set reference of {self:#?} to {reference:?}");
        self.reference = reference;
    }

    pub fn pic_num_f(&self, max_pic_num: i32) -> i32 {
        if matches!(self.reference(), Reference::LongTerm) {
            max_pic_num
        } else {
            self.pic_num
        }
    }

    pub fn long_term_pic_num_f(&self, max_long_term_frame_idx: MaxLongTermFrameIdx) -> u32 {
        if matches!(self.reference(), Reference::LongTerm) {
            self.long_term_pic_num
        } else {
            2 * max_long_term_frame_idx.to_value_plus1()
        }
    }

    /// Consume this picture and return a Rc'd version.
    pub fn into_rc(self) -> RcPictureData {
        RcPictureData {
            pic: Rc::new(RefCell::new(self)),
        }
    }
}

impl std::fmt::Debug for PictureData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PictureData")
            .field("pic_order_cnt_type", &self.pic_order_cnt_type)
            .field("top_field_order_cnt", &self.top_field_order_cnt)
            .field("bottom_field_order_cnt", &self.bottom_field_order_cnt)
            .field("pic_order_cnt", &self.pic_order_cnt)
            .field("pic_order_cnt_msb", &self.pic_order_cnt_msb)
            .field("pic_order_cnt_lsb", &self.pic_order_cnt_lsb)
            .field(
                "delta_pic_order_cnt_bottom",
                &self.delta_pic_order_cnt_bottom,
            )
            .field("delta_pic_order_cnt0", &self.delta_pic_order_cnt0)
            .field("delta_pic_order_cnt1", &self.delta_pic_order_cnt1)
            .field("pic_num", &self.pic_num)
            .field("long_term_pic_num", &self.long_term_pic_num)
            .field("frame_num", &self.frame_num)
            .field("frame_num_offset", &self.frame_num_offset)
            .field("frame_num_wrap", &self.frame_num_wrap)
            .field("long_term_frame_idx", &self.long_term_frame_idx)
            .field("nal_ref_idc", &self.nal_ref_idc)
            .field("is_idr", &self.is_idr)
            .field("reference", &self.reference)
            .field("has_mmco_5", &self.has_mmco_5)
            .field("nonexisting", &self.nonexisting)
            .field("field", &self.field)
            .field("ref_pic_marking", &self.ref_pic_marking)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}
