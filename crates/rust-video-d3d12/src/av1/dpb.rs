//! AV1 reference slots with DXVA submission payloads.
//!
//! Mirrors d11 `av1_dpb.rs`; free slots avoid pinned d12 pool slots.

use std::sync::Arc;

use super::frontend::{
    Frame, FrameHeader, FrameType, SequenceHeader, MAX_SEGMENTS, NUM_REF_FRAMES, REFS_PER_FRAME,
    SEG_LEVELS,
};
use super::params::{
    PicEntryAv1, PicParamsAv1, TileAv1, AV1_CDEF_BITS_SHIFT, AV1_CDEF_DAMPING_SHIFT,
    AV1_CDEF_SECONDARY_SHIFT, AV1_CODING_CDEF, AV1_CODING_DUAL_FILTER, AV1_CODING_FILM_GRAIN,
    AV1_CODING_FILTER_INTRA, AV1_CODING_HIGH_PRECISION_MV, AV1_CODING_INTEGER_MV,
    AV1_CODING_INTERINTRA, AV1_CODING_INTRABC, AV1_CODING_INTRA_EDGE, AV1_CODING_JNT_COMP,
    AV1_CODING_MASKED_COMPOUND, AV1_CODING_NO_CDF_UPDATE, AV1_CODING_NO_FRAME_END_CDF,
    AV1_CODING_REDUCED_TX, AV1_CODING_REFERENCE_MODE, AV1_CODING_REF_MVS_ENABLED,
    AV1_CODING_REF_UPDATE, AV1_CODING_RESTORATION, AV1_CODING_SCREEN_CONTENT, AV1_CODING_SKIP_MODE,
    AV1_CODING_SUPERBLOCK_128, AV1_CODING_SUPERRES, AV1_CODING_SWITCHABLE_MOTION,
    AV1_CODING_TX_MODE_SHIFT, AV1_CODING_USE_REF_MVS, AV1_CODING_WARPED, AV1_FORMAT_MONO_CHROME,
    AV1_FORMAT_SHOWABLE_FRAME, AV1_FORMAT_SHOW_FRAME, AV1_FORMAT_SUBSAMPLING_X,
    AV1_FORMAT_SUBSAMPLING_Y, AV1_FORMAT_TYPE_SHIFT, AV1_GRAIN_APPLY, AV1_GRAIN_CHROMA_FROM_LUMA,
    AV1_GRAIN_COEFF_LAG_SHIFT, AV1_GRAIN_COEFF_SHIFT, AV1_GRAIN_IDENTITY_MATRIX, AV1_GRAIN_OVERLAP,
    AV1_GRAIN_RESTRICTED_RANGE, AV1_GRAIN_SCALE_SHIFT, AV1_GRAIN_SCALING_SHIFT,
    AV1_LF_DELTA_ENABLED, AV1_LF_DELTA_MULTI, AV1_LF_DELTA_PRESENT, AV1_LF_DELTA_UPDATE,
    AV1_MOTION_INVALID, AV1_MOTION_TYPE_SHIFT, AV1_Q_DELTA_PRESENT, AV1_Q_DELTA_RES_SHIFT,
    AV1_SEG_ENABLED, AV1_SEG_TEMPORAL_UPDATE, AV1_SEG_UPDATE_DATA, AV1_SEG_UPDATE_MAP,
};
use super::picture::Av1Submission;
use crate::decode::Output;
use crate::error::{BrokenReferenceReason, D3d12Error, ParseReason};
use crate::pins::SlotPins;

use rust_video_caps::dpb::ViewAlloc;

/// Session view count behind current view selection.
const VIEWS: u8 = 16;

/// Restoration type remap from AV1 to DXVA numbering.
const LR_REMAP: [u8; 4] = [0, 3, 1, 2];

/// Stored reference frame.
#[derive(Clone, Debug)]
struct Av1Slot {
    /// Surface view.
    view: u8,
    /// Frame width in luma samples.
    width: u32,
    /// Frame height in luma samples.
    height: u32,
}

/// Reference slots for one sequence.
pub(crate) struct Av1Dpb {
    /// Reference slots.
    slots: [Option<Av1Slot>; NUM_REF_FRAMES],
    /// Pins held by live surfaces.
    pins: Arc<SlotPins>,
}

impl Av1Dpb {
    /// Empty slots over the `pins` pool table.
    pub(crate) fn new(pins: Arc<SlotPins>) -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            pins,
        }
    }

    /// Submission plus display-order outputs for `frame`.
    pub(crate) fn decode(
        &mut self,
        frame: &Frame,
        seq: &SequenceHeader,
    ) -> Result<Av1Decoded, D3d12Error> {
        let header = &frame.header;
        if header.show_existing_frame() {
            let index = usize::from(header.frame_to_show_map_idx);
            let shown = match self.slots.get(index) {
                Some(Some(slot)) => slot.clone(),
                _ => {
                    return Err(D3d12Error::BrokenReference(
                        BrokenReferenceReason::NamesEmptySlot {
                            reference: "show_existing_frame",
                        },
                    ));
                }
            };
            if header.frame_type == FrameType::Key {
                for slot in &mut self.slots {
                    *slot = Some(shown.clone());
                }
            }
            return Ok(Av1Decoded {
                submission: None,
                outputs: vec![Output::from_slot(shown.view)],
            });
        }
        if !header.frame_type.is_intra() {
            check_slots(&self.slots, header)?;
        }
        let current = self.free_view()?;
        let submission = self.submit(frame, seq, current)?;
        self.refresh(header, current);
        let outputs = if header.show_frame() {
            vec![Output::from_slot(current)]
        } else {
            Vec::new()
        };
        Ok(Av1Decoded {
            submission: Some(submission),
            outputs,
        })
    }

    /// No delayed output behind AV1 display.
    pub(crate) fn drain() -> Vec<Output> {
        Vec::new()
    }

    /// View outside the reference map and live surface pins.
    fn free_view(&self) -> Result<u8, D3d12Error> {
        self.slots
            .free_view(
                VIEWS,
                |slot| slot.view,
                |index| u8::try_from(index).is_ok_and(|view| self.pins.pinned(view)),
            )
            .ok_or(D3d12Error::NoFreeSlot)
    }

    /// Submission payload for the decoded frame.
    fn submit(
        &self,
        frame: &Frame,
        seq: &SequenceHeader,
        current: u8,
    ) -> Result<Av1Submission, D3d12Error> {
        let header = &frame.header;
        let params = self.fill_params(seq, header, current)?;
        let mut bitstream = Vec::with_capacity(
            frame
                .tiles
                .iter()
                .map(|tile| tile.bytes.len())
                .sum::<usize>()
                + 128,
        );
        let mut tiles = Vec::with_capacity(frame.tiles.len());
        for tile in &frame.tiles {
            let offset =
                u32::try_from(bitstream.len()).map_err(|_| D3d12Error::BitstreamTooLarge {
                    len: bitstream.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            let size =
                u32::try_from(tile.bytes.len()).map_err(|_| D3d12Error::BitstreamTooLarge {
                    len: tile.bytes.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            bitstream.extend_from_slice(&tile.bytes);
            tiles.push(TileAv1 {
                data_offset: offset,
                data_size: size,
                row: tile.row,
                column: tile.column,
                reserved: 0,
                anchor_frame: 0xFF,
                reserved8: 0,
            });
        }
        if tiles.is_empty() {
            return Err(D3d12Error::Parse(ParseReason::FrameWithoutTiles));
        }
        let padding = 128 - (bitstream.len() % 128);
        bitstream.resize(bitstream.len() + padding, 0);
        Ok(Av1Submission {
            params,
            tiles,
            bitstream,
            slot: current,
        })
    }

    /// Picture parameters for the decoded frame.
    fn fill_params(
        &self,
        seq: &SequenceHeader,
        header: &FrameHeader,
        current: u8,
    ) -> Result<PicParamsAv1, D3d12Error> {
        let mut params = PicParamsAv1::empty();
        params.width = header.frame_width;
        params.height = header.frame_height;
        params.max_width = seq.max_frame_width();
        params.max_height = seq.max_frame_height();
        params.curr_pic_index = current;
        params.superres_denom = header.superres_denom;
        params.bitdepth = seq.bit_depth();
        params.seq_profile = seq.profile;
        fill_tiles(&mut params, header);
        params.coding_flags = coding_flags(seq, header);
        params.format_flags = format_flags(seq, header);
        params.primary_ref_frame = header.primary_ref_frame;
        params.order_hint = header.order_hint;
        params.order_hint_bits = seq.order_hint_bits();
        self.fill_refs(&mut params, header)?;
        fill_loop_filter(&mut params, header);
        fill_quantization(&mut params, header);
        fill_cdef(&mut params, header);
        params.interp_filter = header.interp_filter;
        fill_segmentation(&mut params, header);
        fill_film_grain(&mut params, seq, header);
        params.feedback = 0;
        Ok(params)
    }

    /// Reference entries plus the texture map.
    fn fill_refs(&self, params: &mut PicParamsAv1, header: &FrameHeader) -> Result<(), D3d12Error> {
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                params.ref_map[index] = slot.view;
            }
        }
        if header.frame_type.is_intra() {
            return Ok(());
        }
        for position in 0..REFS_PER_FRAME {
            let slot = usize::from(header.ref_frame_idx[position]);
            let Some(stored) = self.slots.get(slot).and_then(|slot| slot.as_ref()) else {
                return Err(D3d12Error::BrokenReference(
                    BrokenReferenceReason::NamesEmptySlot {
                        reference: "ref_frame_idx",
                    },
                ));
            };
            let motion = &header.global_motion[position];
            params.frame_refs[position] = PicEntryAv1 {
                width: stored.width,
                height: stored.height,
                wmmat: motion.params,
                motion_flags: (u8::from(motion.invalid) * AV1_MOTION_INVALID)
                    | (motion.warp_type << AV1_MOTION_TYPE_SHIFT),
                index: header.ref_frame_idx[position],
                reserved: 0,
            };
        }
        Ok(())
    }

    /// Reference slots refreshed from the decoded header.
    fn refresh(&mut self, header: &FrameHeader, current: u8) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if header.refresh_frame_flags >> index & 1 == 0 {
                continue;
            }
            *slot = Some(Av1Slot {
                view: current,
                width: header.frame_width,
                height: header.frame_height,
            });
        }
    }
}

/// Reference slots behind `header` validated.
fn check_slots(
    slots: &[Option<Av1Slot>; NUM_REF_FRAMES],
    header: &FrameHeader,
) -> Result<(), D3d12Error> {
    for index in &header.ref_frame_idx {
        if !matches!(slots.get(usize::from(*index)), Some(Some(_))) {
            return Err(D3d12Error::BrokenReference(
                BrokenReferenceReason::NamesEmptySlot {
                    reference: "ref_frame_idx",
                },
            ));
        }
    }
    Ok(())
}

/// Packed coding tool flags.
fn coding_flags(seq: &SequenceHeader, header: &FrameHeader) -> u32 {
    let mut flags = AV1_CODING_REF_UPDATE;
    if seq.use_128x128_superblock() {
        flags |= AV1_CODING_SUPERBLOCK_128;
    }
    if seq.enable_intra_edge_filter() {
        flags |= AV1_CODING_INTRA_EDGE;
    }
    if seq.enable_interintra_compound() {
        flags |= AV1_CODING_INTERINTRA;
    }
    if seq.enable_masked_compound() {
        flags |= AV1_CODING_MASKED_COMPOUND;
    }
    if header.allow_warped_motion() {
        flags |= AV1_CODING_WARPED;
    }
    if seq.enable_dual_filter() {
        flags |= AV1_CODING_DUAL_FILTER;
    }
    if seq.enable_jnt_comp() {
        flags |= AV1_CODING_JNT_COMP;
    }
    if header.allow_screen_content_tools() {
        flags |= AV1_CODING_SCREEN_CONTENT;
    }
    if header.force_integer_mv() {
        flags |= AV1_CODING_INTEGER_MV;
    }
    if seq.enable_cdef() {
        flags |= AV1_CODING_CDEF;
    }
    if seq.enable_restoration() {
        flags |= AV1_CODING_RESTORATION;
    }
    if seq.film_grain_params_present() {
        flags |= AV1_CODING_FILM_GRAIN;
    }
    if header.allow_intrabc() {
        flags |= AV1_CODING_INTRABC;
    }
    if header.allow_high_precision_mv() {
        flags |= AV1_CODING_HIGH_PRECISION_MV;
    }
    if header.is_motion_mode_switchable() {
        flags |= AV1_CODING_SWITCHABLE_MOTION;
    }
    if seq.enable_filter_intra() {
        flags |= AV1_CODING_FILTER_INTRA;
    }
    if header.disable_frame_end_update_cdf() {
        flags |= AV1_CODING_NO_FRAME_END_CDF;
    }
    if header.disable_cdf_update() {
        flags |= AV1_CODING_NO_CDF_UPDATE;
    }
    if header.reference_select() {
        flags |= AV1_CODING_REFERENCE_MODE;
    }
    if header.skip_mode_present() {
        flags |= AV1_CODING_SKIP_MODE;
    }
    if header.reduced_tx_set() {
        flags |= AV1_CODING_REDUCED_TX;
    }
    if header.superres_denom != 8 {
        flags |= AV1_CODING_SUPERRES;
    }
    flags |= u32::from(header.tx_mode) << AV1_CODING_TX_MODE_SHIFT;
    if header.use_ref_frame_mvs() {
        flags |= AV1_CODING_USE_REF_MVS;
    }
    if seq.enable_ref_frame_mvs() {
        flags |= AV1_CODING_REF_MVS_ENABLED;
    }
    flags
}

/// Packed format flags.
fn format_flags(seq: &SequenceHeader, header: &FrameHeader) -> u8 {
    let mut flags = header.frame_type.id() << AV1_FORMAT_TYPE_SHIFT;
    if header.show_frame() {
        flags |= AV1_FORMAT_SHOW_FRAME;
    }
    if header.showable_frame() {
        flags |= AV1_FORMAT_SHOWABLE_FRAME;
    }
    if seq.subsampling_x() {
        flags |= AV1_FORMAT_SUBSAMPLING_X;
    }
    if seq.subsampling_y() {
        flags |= AV1_FORMAT_SUBSAMPLING_Y;
    }
    if seq.mono_chrome() {
        flags |= AV1_FORMAT_MONO_CHROME;
    }
    flags
}

/// Tile grid into the picture parameters.
fn fill_tiles(params: &mut PicParamsAv1, header: &FrameHeader) {
    params.tiles.cols = header.tile_cols;
    params.tiles.rows = header.tile_rows;
    params.tiles.context_update_id = header.context_update_tile_id;
    for (index, width) in header
        .width_in_sbs_minus_1
        .iter()
        .enumerate()
        .take(usize::from(header.tile_cols))
    {
        params.tiles.widths[index] = width.saturating_add(1);
    }
    for (index, height) in header
        .height_in_sbs_minus_1
        .iter()
        .enumerate()
        .take(usize::from(header.tile_rows))
    {
        params.tiles.heights[index] = height.saturating_add(1);
    }
}

/// Loop filter parameters.
fn fill_loop_filter(params: &mut PicParamsAv1, header: &FrameHeader) {
    params.loop_filter.filter_level = [header.loop_filter_level[0], header.loop_filter_level[1]];
    params.loop_filter.filter_level_u = header.loop_filter_level[2];
    params.loop_filter.filter_level_v = header.loop_filter_level[3];
    params.loop_filter.sharpness = header.loop_filter_sharpness;
    let mut control = 0u8;
    if header.loop_filter_delta_enabled() {
        control |= AV1_LF_DELTA_ENABLED;
    }
    if header.loop_filter_delta_update() {
        control |= AV1_LF_DELTA_UPDATE;
    }
    if header.delta_lf_multi() {
        control |= AV1_LF_DELTA_MULTI;
    }
    if header.delta_lf_present() {
        control |= AV1_LF_DELTA_PRESENT;
    }
    params.loop_filter.control = control;
    params.loop_filter.ref_deltas = header.loop_ref_deltas;
    params.loop_filter.mode_deltas = header.loop_mode_deltas;
    params.loop_filter.delta_lf_res = header.delta_lf_res;
    let uses_lr = header.lr_type.iter().any(|lr_type| *lr_type != 0);
    for (index, lr_type) in header.lr_type.iter().enumerate() {
        params.loop_filter.restoration_type[index] =
            LR_REMAP[usize::from(*lr_type).min(LR_REMAP.len() - 1)];
    }
    if uses_lr {
        params.loop_filter.log2_restoration_unit_size = [
            6 + u16::from(header.lr_unit_shift),
            6 + u16::from(header.lr_unit_shift) - u16::from(header.lr_uv_shift),
            6 + u16::from(header.lr_unit_shift) - u16::from(header.lr_uv_shift),
        ];
    }
}

/// Quantization parameters.
fn fill_quantization(params: &mut PicParamsAv1, header: &FrameHeader) {
    params.quantization.control = (u8::from(header.delta_q_present()) * AV1_Q_DELTA_PRESENT)
        | (header.delta_q_res << AV1_Q_DELTA_RES_SHIFT);
    params.quantization.base_qindex = header.base_q_idx;
    params.quantization.y_dc_delta = header.delta_q_y_dc;
    params.quantization.u_dc_delta = header.delta_q_u_dc;
    params.quantization.v_dc_delta = header.delta_q_v_dc;
    params.quantization.u_ac_delta = header.delta_q_u_ac;
    params.quantization.v_ac_delta = header.delta_q_v_ac;
    if header.using_qmatrix() {
        params.quantization.qm_y = header.qm_y;
        params.quantization.qm_u = header.qm_u;
        params.quantization.qm_v = header.qm_v;
    }
}

/// CDEF parameters.
fn fill_cdef(params: &mut PicParamsAv1, header: &FrameHeader) {
    params.cdef.control = (header.cdef_damping_minus_3 << AV1_CDEF_DAMPING_SHIFT)
        | (header.cdef_bits << AV1_CDEF_BITS_SHIFT);
    for index in 0..8 {
        params.cdef.y_strengths[index] =
            header.cdef_y_pri[index] | (header.cdef_y_sec[index] << AV1_CDEF_SECONDARY_SHIFT);
        params.cdef.uv_strengths[index] =
            header.cdef_uv_pri[index] | (header.cdef_uv_sec[index] << AV1_CDEF_SECONDARY_SHIFT);
    }
}

/// Segmentation parameters.
fn fill_segmentation(params: &mut PicParamsAv1, header: &FrameHeader) {
    let mut control = 0u8;
    if header.segmentation_enabled() {
        control |= AV1_SEG_ENABLED;
    }
    if header.segmentation_update_map() {
        control |= AV1_SEG_UPDATE_MAP;
    }
    if header.segmentation_update_data() {
        control |= AV1_SEG_UPDATE_DATA;
    }
    if header.segmentation_temporal_update() {
        control |= AV1_SEG_TEMPORAL_UPDATE;
    }
    params.segmentation.control = control;
    for segment in 0..MAX_SEGMENTS {
        let mut mask = 0u8;
        for level in 0..SEG_LEVELS {
            if header.seg_enabled[segment][level] {
                mask |= 1 << level;
            }
            params.segmentation.feature_data[segment][level] = header.seg_value[segment][level];
        }
        params.segmentation.feature_mask[segment] = mask;
    }
}

/// Film grain parameters.
fn fill_film_grain(params: &mut PicParamsAv1, seq: &SequenceHeader, header: &FrameHeader) {
    let grain = &header.film_grain;
    if !grain.apply_grain() {
        return;
    }
    let mut control = AV1_GRAIN_APPLY;
    control |= u16::from(grain.grain_scaling_minus_8) << AV1_GRAIN_SCALING_SHIFT;
    if grain.chroma_scaling_from_luma() {
        control |= AV1_GRAIN_CHROMA_FROM_LUMA;
    }
    control |= u16::from(grain.ar_coeff_lag) << AV1_GRAIN_COEFF_LAG_SHIFT;
    control |= u16::from(grain.ar_coeff_shift_minus_6) << AV1_GRAIN_COEFF_SHIFT;
    control |= u16::from(grain.grain_scale_shift) << AV1_GRAIN_SCALE_SHIFT;
    if grain.overlap_flag() {
        control |= AV1_GRAIN_OVERLAP;
    }
    if grain.clip_to_restricted_range() {
        control |= AV1_GRAIN_RESTRICTED_RANGE;
    }
    if seq.matrix_coefficients == 0 {
        control |= AV1_GRAIN_IDENTITY_MATRIX;
    }
    params.film_grain.control = control;
    params.film_grain.grain_seed = grain.grain_seed;
    params.film_grain.scaling_points_y = grain.scaling_points_y;
    params.film_grain.num_y_points = grain.num_y_points;
    params.film_grain.scaling_points_cb = grain.scaling_points_cb;
    params.film_grain.num_cb_points = grain.num_cb_points;
    params.film_grain.scaling_points_cr = grain.scaling_points_cr;
    params.film_grain.num_cr_points = grain.num_cr_points;
    params.film_grain.ar_coeffs_y = grain.ar_coeffs_y;
    params.film_grain.ar_coeffs_cb = grain.ar_coeffs_cb;
    params.film_grain.ar_coeffs_cr = grain.ar_coeffs_cr;
    params.film_grain.cb_mult = grain.cb_mult;
    params.film_grain.cb_luma_mult = grain.cb_luma_mult;
    params.film_grain.cr_mult = grain.cr_mult;
    params.film_grain.cr_luma_mult = grain.cr_luma_mult;
    params.film_grain.cb_offset = grain.cb_offset.cast_signed();
    params.film_grain.cr_offset = grain.cr_offset.cast_signed();
}

/// Submission plus display-order outputs.
#[derive(Debug)]
pub(crate) struct Av1Decoded {
    /// Driver submission, absent for existing-frame display.
    pub(crate) submission: Option<Av1Submission>,
    /// Display-order outputs.
    pub(crate) outputs: Vec<Output>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::Av1Dpb;
    use crate::av1::frontend::{
        FilmGrain, Frame, FrameHeader, FrameType, GlobalMotion, SequenceHeader, TileData,
        FH_SHOW_FRAME, SEQ_CDEF, SEQ_ORDER_HINT, SEQ_SUBSAMPLING_X, SEQ_SUBSAMPLING_Y,
    };
    use crate::decode::Output;

    /// Empty pin table behind buffer tests.
    fn pins() -> Arc<crate::pins::SlotPins> {
        Arc::new(crate::pins::SlotPins::new())
    }

    /// Sequence header with 320x240 maximum and order hints.
    fn seq() -> SequenceHeader {
        SequenceHeader {
            profile: 0,
            flags: SEQ_ORDER_HINT | SEQ_SUBSAMPLING_X | SEQ_SUBSAMPLING_Y | SEQ_CDEF,
            operating: Vec::new(),
            num_units_in_display_tick: 0,
            time_scale: 0,
            num_ticks_per_picture_minus1: 0,
            buffer_delay_length_minus1: 0,
            num_units_in_decoding_tick: 0,
            buffer_removal_length_minus1: 0,
            presentation_length_minus1: 0,
            frame_width_bits_minus1: 8,
            frame_height_bits_minus1: 7,
            max_frame_width_minus1: 319,
            max_frame_height_minus1: 239,
            delta_frame_id_length_minus2: 0,
            additional_frame_id_length_minus1: 0,
            force_screen_content_tools: 2,
            force_integer_mv: 2,
            order_hint_bits_minus1: 6,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            chroma_sample_position: 0,
        }
    }

    /// Frame header with `frame_type`, `order_hint`, and `refresh` flags.
    fn header(frame_type: FrameType, order_hint: u8, refresh: u8, show: bool) -> FrameHeader {
        let mut header = FrameHeader {
            frame_type,
            order_hint,
            refresh_frame_flags: refresh,
            frame_width: 320,
            frame_height: 240,
            upscaled_width: 320,
            render_width: 320,
            render_height: 240,
            tile_cols: 1,
            tile_rows: 1,
            base_q_idx: 100,
            tx_mode: 2,
            ..FrameHeader::default()
        };
        if show {
            header.flags |= FH_SHOW_FRAME;
        }
        header
    }

    /// Decoded frame with one tile.
    fn frame(header: FrameHeader) -> Frame {
        Frame {
            header,
            tiles: vec![TileData {
                bytes: vec![0xAA, 0xBB],
                row: 0,
                column: 0,
            }],
        }
    }

    /// Key frame refreshing every slot.
    fn key(show: bool) -> Frame {
        frame(header(FrameType::Key, 0, 0xFF, show))
    }

    /// Inter frame over `refs` refreshing `refresh`.
    fn inter(order_hint: u8, refs: [u8; 7], refresh: u8, show: bool) -> Frame {
        let mut header = header(FrameType::Inter, order_hint, refresh, show);
        header.ref_frame_idx = refs;
        frame(header)
    }

    /// Existing-frame display of `slot`.
    fn show_existing(slot: u8, frame_type: FrameType, order_hint: u8) -> Frame {
        let mut header = FrameHeader {
            frame_type,
            order_hint,
            frame_to_show_map_idx: slot,
            ..FrameHeader::default()
        };
        header.flags |= crate::av1::frontend::FH_SHOW_EXISTING;
        Frame {
            header,
            tiles: Vec::new(),
        }
    }

    /// Output views in order.
    fn slots(outputs: &[Output]) -> Vec<u8> {
        outputs.iter().map(Output::slot).collect()
    }

    #[test]
    fn key_frame_submits_and_shows() {
        let mut dpb = Av1Dpb::new(pins());
        let decoded = dpb.decode(&key(true), &seq()).expect("key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(slots(&decoded.outputs), vec![submission.slot]);
        let params = &submission.params;
        assert_eq!((params.width, params.height), (320, 240));
        assert_eq!((params.max_width, params.max_height), (320, 240));
        assert_eq!(params.format_flags, 0x34);
        assert_eq!(params.order_hint_bits, 7);
        assert!(params.ref_map.iter().all(|index| *index == 0xFF));
        assert!(params.frame_refs.iter().all(|entry| entry.index == 0xFF));
        assert_eq!(params.feedback, 0);
        assert_eq!(params.tiles.cols, 1);
        assert_eq!(params.tiles.widths[0], 1);
    }

    #[test]
    fn all_views_pinned_errors_no_free_slot() {
        let pins = pins();
        let mut dpb = Av1Dpb::new(Arc::clone(&pins));
        let _held: Vec<_> = (0u8..16).map(|view| pins.pin(view)).collect();
        let error = dpb.decode(&key(true), &seq()).unwrap_err();
        assert!(matches!(error, crate::error::D3d12Error::NoFreeSlot));
        assert_eq!(error.to_string(), "no free surface slot");
    }

    #[test]
    fn inter_references_key() {
        let mut dpb = Av1Dpb::new(pins());
        dpb.decode(&key(true), &seq()).expect("key");
        let decoded = dpb
            .decode(&inter(1, [0; 7], 0x02, true), &seq())
            .expect("inter");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slot, 1);
        let params = &submission.params;
        assert_eq!(params.ref_map[0], 0);
        assert_eq!(params.frame_refs[0].index, 0);
        assert_eq!(
            (params.frame_refs[0].width, params.frame_refs[0].height),
            (320, 240)
        );
        assert_eq!(params.format_flags & 3, 1);
        assert_eq!(slots(&decoded.outputs), vec![1]);
    }

    #[test]
    fn show_existing_outputs_slot() {
        let mut dpb = Av1Dpb::new(pins());
        dpb.decode(&key(true), &seq()).expect("key");
        dpb.decode(&inter(6, [0; 7], 0x02, false), &seq())
            .expect("hidden");
        let decoded = dpb
            .decode(&show_existing(1, FrameType::Inter, 6), &seq())
            .expect("show existing");
        assert!(decoded.submission.is_none());
        assert_eq!(slots(&decoded.outputs), vec![1]);
        assert!(Av1Dpb::drain().is_empty(), "av1 never delays output");
    }

    #[test]
    fn show_existing_empty_slot_breaks() {
        let mut dpb = Av1Dpb::new(pins());
        let error = dpb
            .decode(&show_existing(2, FrameType::Inter, 0), &seq())
            .unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d12Error::BrokenReference(_)
        ));
    }

    #[test]
    fn missing_reference_breaks() {
        let mut dpb = Av1Dpb::new(pins());
        let error = dpb
            .decode(&inter(1, [0; 7], 0x02, true), &seq())
            .unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d12Error::BrokenReference(_)
        ));
    }

    #[test]
    fn refresh_zero_uses_spare_view() {
        let mut dpb = Av1Dpb::new(pins());
        dpb.decode(&key(true), &seq()).expect("key");
        let decoded = dpb
            .decode(&inter(1, [0; 7], 0x00, true), &seq())
            .expect("inter");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slot, 1);
        assert_eq!(submission.params.ref_map, [0; 8]);
    }

    #[test]
    fn submission_carries_tiles() {
        let mut dpb = Av1Dpb::new(pins());
        let decoded = dpb.decode(&key(true), &seq()).expect("key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.bitstream.len(), 128);
        assert_eq!(submission.bitstream[..2], [0xAA, 0xBB]);
        assert!(submission.bitstream[2..].iter().all(|byte| *byte == 0));
        assert_eq!(submission.tiles.len(), 1);
        let tile = &submission.tiles[0];
        assert_eq!((tile.data_offset, tile.data_size), (0, 2));
        assert_eq!((tile.row, tile.column), (0, 0));
        assert_eq!(tile.anchor_frame, 0xFF);
    }

    #[test]
    fn film_grain_fill_copies_points() {
        let mut dpb = Av1Dpb::new(pins());
        let mut grain = FilmGrain {
            flags: crate::av1::frontend::FG_APPLY | crate::av1::frontend::FG_OVERLAP,
            grain_seed: 42,
            num_y_points: 1,
            num_cb_points: 1,
            ..FilmGrain::default()
        };
        grain.scaling_points_y[0] = [10, 20];
        grain.scaling_points_cb[0] = [30, 40];
        grain.ar_coeffs_y[0] = 128;
        let mut key = key(true);
        key.header.film_grain = grain;
        let decoded = dpb.decode(&key, &seq()).expect("key");
        let params = &decoded.submission.expect("submission").params;
        assert_eq!(params.film_grain.grain_seed, 42);
        assert_eq!(params.film_grain.scaling_points_y[0], [10, 20]);
        assert_eq!(params.film_grain.scaling_points_cb[0], [30, 40]);
        assert_eq!(params.film_grain.ar_coeffs_y[0], 128);
        assert_eq!(params.film_grain.control & 1, 1);
    }

    #[test]
    fn show_existing_key_refreshes_all() {
        let mut dpb = Av1Dpb::new(pins());
        dpb.decode(&key(true), &seq()).expect("key");
        dpb.decode(&inter(6, [0; 7], 0x02, false), &seq())
            .expect("hidden");
        dpb.decode(&show_existing(1, FrameType::Key, 6), &seq())
            .expect("show key");
        let decoded = dpb
            .decode(&inter(7, [0; 7], 0x04, true), &seq())
            .expect("inter");
        assert_eq!(
            decoded.submission.expect("submission").params.ref_map,
            [1; 8]
        );
    }

    #[test]
    fn motion_flags_pack_invalid_and_type() {
        let mut dpb = Av1Dpb::new(pins());
        dpb.decode(&key(true), &seq()).expect("key");
        let mut frame = inter(1, [0; 7], 0x02, true);
        frame.header.global_motion[0] = GlobalMotion {
            warp_type: 3,
            params: [1, 2, 3, 4, 5, 6],
            invalid: false,
        };
        let decoded = dpb.decode(&frame, &seq()).expect("inter");
        let entry = &decoded.submission.expect("submission").params.frame_refs[0];
        assert_eq!(entry.motion_flags, 3 << 1);
        assert_eq!(entry.wmmat, [1, 2, 3, 4, 5, 6]);
    }
}
