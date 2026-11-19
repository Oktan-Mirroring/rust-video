// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::h265::{anchors_tid0, is_bla, is_cra, is_idr, is_irap, is_rasl, SliceHeader, Sps};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Reference {
    #[default]
    None,
    ShortTerm,
    LongTerm,
}

/// Output flags of an h.265 picture.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct PictureOutput {
    pub no_rasl_output_flag: bool,
    pub pic_output_flag: bool,
    pub no_output_of_prior_pics_flag: bool,
}

/// Decoder-kept state of an h.265 picture.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct PictureState {
    pub valid_for_prev_tid0_pic: bool,
    pub first_picture_after_eos: bool,
    pub needed_for_output: bool,
}

/// Data associated with an h.265 picture. Most fields are extracted from the
/// slice header and kept for future processing.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct PictureData {
    // Fields extracted from the slice header. These are the CamelCase
    // variables, unless noted otherwise.
    pub nalu_type: u8,
    pub output: PictureOutput,
    pub state: PictureState,
    pub slice_pic_order_cnt_lsb: i32,
    pub pic_order_cnt_msb: i32,
    pub pic_order_cnt_val: i32,

    // Internal state.
    reference: Reference,
    pub pic_latency_cnt: i32,
    pub short_term_ref_pic_set_size_bits: u32,
}

impl PictureData {
    /// Instantiates a new `PictureData` for a coded picture.
    ///
    /// See 8.1.3 Decoding process for a coded picture with `nuh_layer_id` equal
    /// to 0. Runs the picture order count process (clause 8.3.1) to initialize
    /// the POC values.
    pub fn new_from_slice(
        header: &SliceHeader,
        nalu_type: u8,
        temporal_id: u8,
        first_picture_in_bitstream: bool,
        first_picture_after_eos: bool,
        prev_tid0_poc: Option<i32>,
        sps: &Sps,
    ) -> Self {
        // We assume HandleCraAsBlafFLag == 0, as it is only set through
        // external means, which we do not provide.

        let slice_pic_order_cnt_lsb: i32 = header.poc_lsb.unwrap_or(0).cast_signed();
        let pic_output_flag = header.pic_output;
        let no_output_of_prior_pics_flag = header.no_output_prior;
        let st_rps_bits = header.rps_bits.map_or(0, |bits| bits.bits);

        // Compute the output flags:
        // The value of NoRaslOutputFlag is equal to 1 for each IDR access
        // unit, each BLA access unit, and each CRA access unit that is the
        // first access unit in the bitstream in decoding order, is the first
        // access unit that follows an end of sequence NAL unit in decoding
        // order, or has HandleCraAsBlaFlag equal to 1.
        let no_rasl_output_flag = is_idr(nalu_type)
            || is_bla(nalu_type)
            || (is_cra(nalu_type) && first_picture_in_bitstream)
            || first_picture_after_eos;

        let pic_output_flag = if is_rasl(nalu_type) && no_rasl_output_flag {
            false
        } else {
            pic_output_flag
        };

        // Compute the Picture Order Count. See 8.3.1 Decoding Process for
        // Picture Order Count
        let reset_msb = is_irap(nalu_type) && no_rasl_output_flag;
        let pic_order_cnt_val = sps
            .poc_from_lsb(prev_tid0_poc, header.poc_lsb.unwrap_or(0), reset_msb)
            .unwrap_or(0);
        // Equation (8-2), solved for the most significant bits.
        let pic_order_cnt_msb = pic_order_cnt_val - slice_pic_order_cnt_lsb;

        // Compute whether this picture will be a valid prevTid0Pic, i.e.:
        //
        // Let prevTid0Pic be the previous picture in decoding order that has
        // TemporalId equal to 0 and that is not a RASL, RADL or SLNR picture.
        //
        // Use this flag to correctly set up the field in the decoder during
        // `finish_picture`.
        let valid_for_prev_tid0_pic = anchors_tid0(nalu_type, temporal_id);

        let no_output_of_prior_pics_flag =
            if is_irap(nalu_type) && no_rasl_output_flag && !first_picture_in_bitstream {
                is_cra(nalu_type) || no_output_of_prior_pics_flag
            } else {
                false
            };

        Self {
            nalu_type,
            output: PictureOutput {
                no_rasl_output_flag,
                pic_output_flag,
                no_output_of_prior_pics_flag,
            },
            state: PictureState {
                valid_for_prev_tid0_pic,
                first_picture_after_eos,
                needed_for_output: false,
            },
            slice_pic_order_cnt_lsb,
            pic_order_cnt_msb,
            pic_order_cnt_val,
            reference: Reference::default(),
            pic_latency_cnt: 0,
            short_term_ref_pic_set_size_bits: u32::from(st_rps_bits),
        }
    }

    /// Whether the current picture is a reference, either `ShortTerm` or `LongTerm`.
    pub fn is_ref(&self) -> bool {
        !matches!(self.reference, Reference::None)
    }

    pub fn set_reference(&mut self, reference: Reference) {
        log::debug!(
            "Set reference of POC {} to {:?}",
            self.pic_order_cnt_val,
            reference
        );

        self.reference = reference;
    }

    pub fn reference(&self) -> &Reference {
        &self.reference
    }
}
