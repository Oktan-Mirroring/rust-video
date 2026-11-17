//! VP9 reference slots with DXVA submission payloads.

use std::sync::Arc;

use crate::dpb::Output;
use crate::dxva::{
    PicEntryVpx, PicParamsVp9, SliceVpxShort, VP9_CONTROL_DELTA_ENABLED, VP9_CONTROL_DELTA_UPDATE,
    VP9_FORMAT_CONTEXT_SHIFT, VP9_FORMAT_ERROR_RESILIENT, VP9_FORMAT_FRAME_PARALLEL,
    VP9_FORMAT_FRAME_TYPE, VP9_FORMAT_HIGH_PRECISION_MV, VP9_FORMAT_INTRA_ONLY,
    VP9_FORMAT_REFRESH_CONTEXT, VP9_FORMAT_RESET_SHIFT, VP9_FORMAT_SHOW_FRAME,
    VP9_FORMAT_SUBSAMPLING_X, VP9_FORMAT_SUBSAMPLING_Y, VP9_SEG_ABS_DELTA, VP9_SEG_ENABLED,
    VP9_SEG_TEMPORAL_UPDATE, VP9_SEG_UPDATE_MAP,
};
use crate::error::{BrokenReferenceReason, D3d11Error, ParseReason};
use crate::pins::SlotPins;
use crate::vp9::{Frame, FrameHeader, FrameType, NUM_REF_FRAMES, REFS_PER_FRAME};

/// Session view count behind current view selection.
const VIEWS: u8 = 16;

/// Stored reference frame.
#[derive(Clone, Debug)]
struct Vp9Slot {
    /// Surface view.
    view: u8,
    /// Frame width in luma samples.
    width: u32,
    /// Frame height in luma samples.
    height: u32,
}

/// Reference slots for one sequence.
pub(crate) struct Vp9Dpb {
    /// Reference slots.
    slots: [Option<Vp9Slot>; NUM_REF_FRAMES],
    /// Pins held by live surfaces.
    pins: Arc<SlotPins>,
}

impl Vp9Dpb {
    /// Empty slots.
    pub(crate) fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            pins: Arc::new(SlotPins::new()),
        }
    }

    /// Pins shared with live surfaces.
    #[must_use]
    pub(crate) fn pins(&self) -> &Arc<SlotPins> {
        &self.pins
    }

    /// Submission plus display-order outputs for `frame`.
    pub(crate) fn decode(&mut self, frame: &Frame) -> Result<Vp9Decoded, D3d11Error> {
        let header = &frame.header;
        if header.show_existing_frame() {
            let index = usize::from(header.frame_to_show_map_idx);
            let shown = match self.slots.get(index) {
                Some(Some(slot)) => slot.clone(),
                _ => {
                    return Err(D3d11Error::BrokenReference(
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
            return Ok(Vp9Decoded {
                submission: None,
                outputs: vec![Output::from_slot(shown.view)],
                current_index: None,
            });
        }
        if needs_refs(header) {
            check_slots(&self.slots, header)?;
        }
        let current = self.free_view()?;
        let submission = self.submit(frame, current)?;
        self.refresh(header, current);
        let (outputs, current_index) = if header.show_frame() {
            (vec![Output::from_slot(current)], Some(0))
        } else {
            (Vec::new(), None)
        };
        Ok(Vp9Decoded {
            submission: Some(submission),
            outputs,
            current_index,
        })
    }

    /// No delayed output behind VP9 display.
    pub(crate) fn drain() -> Vec<Output> {
        Vec::new()
    }

    /// View outside the reference map and live surface pins.
    fn free_view(&self) -> Result<u8, D3d11Error> {
        (0..VIEWS)
            .find(|view| {
                !self.pins.is_pinned(usize::from(*view))
                    && !self.slots.iter().flatten().any(|slot| slot.view == *view)
            })
            .ok_or(D3d11Error::NoFreeSlot)
    }

    /// Submission payload for the decoded frame.
    fn submit(&self, frame: &Frame, current: u8) -> Result<SubmittedVp9, D3d11Error> {
        let header = &frame.header;
        let params = self.fill_params(header, current)?;
        let bitstream = frame.bytes.clone();
        let mut offset =
            usize::from(header.uncompressed_size) + usize::from(header.header_size_in_bytes);
        let mut slices = Vec::with_capacity(frame.tiles.len());
        let last = frame.tiles.len().saturating_sub(1);
        for (index, tile) in frame.tiles.iter().enumerate() {
            if index != last {
                offset += 4;
            }
            let location = u32::try_from(offset).map_err(|_| D3d11Error::BitstreamTooLarge {
                len: offset,
                capacity: u64::from(u32::MAX),
            })?;
            let size =
                u32::try_from(tile.bytes.len()).map_err(|_| D3d11Error::BitstreamTooLarge {
                    len: tile.bytes.len(),
                    capacity: u64::from(u32::MAX),
                })?;
            let overruns = offset
                .checked_add(tile.bytes.len())
                .is_none_or(|end| end > bitstream.len());
            if overruns {
                return Err(D3d11Error::Parse(ParseReason::Truncated {
                    syntax: "tile_data",
                }));
            }
            slices.push(SliceVpxShort {
                location,
                bytes: size,
                chopping: 0,
            });
            offset += tile.bytes.len();
        }
        if slices.is_empty() {
            return Err(D3d11Error::Parse(ParseReason::FrameWithoutTiles));
        }
        Ok(SubmittedVp9 {
            params,
            slices,
            bitstream,
            slot: current,
        })
    }

    /// Picture parameters for the decoded frame.
    fn fill_params(&self, header: &FrameHeader, current: u8) -> Result<PicParamsVp9, D3d11Error> {
        let mut params = PicParamsVp9::empty();
        params.curr_pic = PicEntryVpx::frame(current);
        params.profile = header.profile;
        params.format_flags = format_flags(header);
        params.width = header.frame_width;
        params.height = header.frame_height;
        params.bit_depth_luma_minus8 = header.bit_depth.saturating_sub(8);
        params.bit_depth_chroma_minus8 = header.bit_depth.saturating_sub(8);
        params.interp_filter = header.interp_filter;
        self.fill_refs(&mut params, header)?;
        params.ref_frame_sign_bias = header.ref_frame_sign_bias.map(u8::cast_signed);
        params.filter_level = header.filter_level.cast_signed();
        params.sharpness_level = header.sharpness.cast_signed();
        params.control_flags = control_flags(header);
        params.ref_deltas = header.ref_deltas;
        params.mode_deltas = header.mode_deltas;
        params.base_qindex = i16::from(header.base_q_idx);
        params.y_dc_delta_q = header.delta_q_y_dc;
        params.uv_dc_delta_q = header.delta_q_uv_dc;
        params.uv_ac_delta_q = header.delta_q_uv_ac;
        fill_segmentation(&mut params, header);
        params.log2_tile_cols = header.tile_cols_log2;
        params.log2_tile_rows = header.tile_rows_log2;
        params.uncompressed_header_size = header.uncompressed_size;
        params.first_partition_size = header.header_size_in_bytes;
        Ok(params)
    }

    /// Reference entries plus the texture map.
    fn fill_refs(&self, params: &mut PicParamsVp9, header: &FrameHeader) -> Result<(), D3d11Error> {
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(slot) = slot {
                params.ref_frame_map[index] = PicEntryVpx::frame(slot.view);
                params.ref_frame_coded_width[index] = slot.width;
                params.ref_frame_coded_height[index] = slot.height;
            }
        }
        if !needs_refs(header) {
            return Ok(());
        }
        for position in 0..REFS_PER_FRAME {
            let slot = usize::from(header.ref_frame_idx[position]);
            let Some(stored) = self.slots.get(slot).and_then(|slot| slot.as_ref()) else {
                return Err(D3d11Error::BrokenReference(
                    BrokenReferenceReason::NamesEmptySlot {
                        reference: "ref_frame_idx",
                    },
                ));
            };
            params.frame_refs[position] = PicEntryVpx::frame(stored.view);
        }
        Ok(())
    }

    /// Reference slots refreshed from the decoded header.
    fn refresh(&mut self, header: &FrameHeader, current: u8) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if header.refresh_frame_flags >> index & 1 == 0 {
                continue;
            }
            *slot = Some(Vp9Slot {
                view: current,
                width: header.frame_width,
                height: header.frame_height,
            });
        }
    }
}

/// True for inter frames that read reference slots.
fn needs_refs(header: &FrameHeader) -> bool {
    !header.frame_type.is_intra() && !header.intra_only()
}

/// Reference slots behind `header` validated.
fn check_slots(
    slots: &[Option<Vp9Slot>; NUM_REF_FRAMES],
    header: &FrameHeader,
) -> Result<(), D3d11Error> {
    for index in &header.ref_frame_idx {
        if !matches!(slots.get(usize::from(*index)), Some(Some(_))) {
            return Err(D3d11Error::BrokenReference(
                BrokenReferenceReason::NamesEmptySlot {
                    reference: "ref_frame_idx",
                },
            ));
        }
    }
    Ok(())
}

/// Packed format and picture flags.
fn format_flags(header: &FrameHeader) -> u16 {
    let mut flags = if header.frame_type.is_intra() {
        0
    } else {
        VP9_FORMAT_FRAME_TYPE
    };
    if header.show_frame() {
        flags |= VP9_FORMAT_SHOW_FRAME;
    }
    if header.error_resilient_mode() {
        flags |= VP9_FORMAT_ERROR_RESILIENT;
    }
    if header.subsampling_x() {
        flags |= VP9_FORMAT_SUBSAMPLING_X;
    }
    if header.subsampling_y() {
        flags |= VP9_FORMAT_SUBSAMPLING_Y;
    }
    if header.refresh_frame_context() {
        flags |= VP9_FORMAT_REFRESH_CONTEXT;
    }
    if header.frame_parallel_decoding_mode() {
        flags |= VP9_FORMAT_FRAME_PARALLEL;
    }
    if header.intra_only() {
        flags |= VP9_FORMAT_INTRA_ONLY;
    }
    flags |= u16::from(header.frame_context_idx) << VP9_FORMAT_CONTEXT_SHIFT;
    flags |= u16::from(header.reset_frame_context) << VP9_FORMAT_RESET_SHIFT;
    if header.allow_high_precision_mv() {
        flags |= VP9_FORMAT_HIGH_PRECISION_MV;
    }
    flags
}

/// Packed loop filter control flags.
fn control_flags(header: &FrameHeader) -> u8 {
    let mut flags = 0;
    if header.loop_filter_delta_enabled() {
        flags |= VP9_CONTROL_DELTA_ENABLED;
    }
    if header.loop_filter_delta_update() {
        flags |= VP9_CONTROL_DELTA_UPDATE;
    }
    flags
}

/// Segmentation parameters.
fn fill_segmentation(params: &mut PicParamsVp9, header: &FrameHeader) {
    let mut control = 0u8;
    if header.segmentation_enabled() {
        control |= VP9_SEG_ENABLED;
    }
    if header.segmentation_update_map() {
        control |= VP9_SEG_UPDATE_MAP;
    }
    if header.segmentation_temporal_update() {
        control |= VP9_SEG_TEMPORAL_UPDATE;
    }
    if header.segmentation_abs_delta() {
        control |= VP9_SEG_ABS_DELTA;
    }
    params.segmentation.control = control;
    params.segmentation.tree_probs = header.tree_probs;
    params.segmentation.pred_probs = header.pred_probs;
    params.segmentation.feature_data = header.seg_value;
    for (segment, enabled) in header.seg_enabled.iter().enumerate() {
        let mut mask = 0u8;
        for (level, on) in enabled.iter().enumerate() {
            if *on {
                mask |= 1 << level;
            }
        }
        params.segmentation.feature_mask[segment] = mask;
    }
}

/// Driver submission for one decoded frame.
#[derive(Debug)]
pub(crate) struct SubmittedVp9 {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsVp9,
    /// Slice control buffer, one entry per tile.
    pub(crate) slices: Vec<SliceVpxShort>,
    /// Coded frame bytes behind the slice offsets.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface view.
    pub(crate) slot: u8,
}

/// Submission plus display-order outputs.
#[derive(Debug)]
pub(crate) struct Vp9Decoded {
    /// Driver submission, absent for existing-frame display.
    pub(crate) submission: Option<SubmittedVp9>,
    /// Display-order outputs.
    pub(crate) outputs: Vec<Output>,
    /// Current frame output behind post-submit copies.
    pub(crate) current_index: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::Vp9Dpb;
    use crate::dpb::Output;
    use crate::vp9::{
        Frame, FrameHeader, FrameType, TileData, FH_INTRA_ONLY, FH_SEG_ABS_DELTA, FH_SEG_ENABLED,
        FH_SEG_TEMPORAL_UPDATE, FH_SEG_UPDATE_MAP, FH_SHOW_FRAME, FH_SUBSAMPLING_X,
        FH_SUBSAMPLING_Y,
    };

    /// Frame header with `frame_type`, `refresh` flags, and `show` display.
    fn header(frame_type: FrameType, refresh: u8, show: bool) -> FrameHeader {
        let mut header = FrameHeader {
            frame_type,
            profile: 0,
            bit_depth: 8,
            frame_width: 320,
            frame_height: 240,
            render_width: 320,
            render_height: 240,
            refresh_frame_flags: refresh,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
            header_size_in_bytes: 4,
            uncompressed_size: 10,
            base_q_idx: 100,
            ..FrameHeader::default()
        };
        header.flags |= FH_SUBSAMPLING_X | FH_SUBSAMPLING_Y;
        if show {
            header.flags |= FH_SHOW_FRAME;
        }
        header
    }

    /// Decoded frame with one tile over consistent bytes.
    fn frame(header: FrameHeader) -> Frame {
        let prefix =
            usize::from(header.uncompressed_size) + usize::from(header.header_size_in_bytes);
        let mut bytes = vec![0u8; prefix];
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        Frame {
            header,
            bytes,
            tiles: vec![TileData {
                bytes: vec![0xAA, 0xBB],
                row: 0,
                column: 0,
            }],
        }
    }

    /// Key frame refreshing every slot.
    fn key(show: bool) -> Frame {
        frame(header(FrameType::Key, 0xFF, show))
    }

    /// Inter frame over `refs` refreshing `refresh`.
    fn inter(refs: [u8; 3], refresh: u8, show: bool) -> Frame {
        let mut head = header(FrameType::Inter, refresh, show);
        head.ref_frame_idx = refs;
        frame(head)
    }

    /// Existing-frame display of `slot`.
    fn show_existing(slot: u8, frame_type: FrameType) -> Frame {
        let mut header = FrameHeader {
            frame_type,
            frame_to_show_map_idx: slot,
            ..FrameHeader::default()
        };
        header.flags |= crate::vp9::FH_SHOW_EXISTING;
        Frame {
            header,
            bytes: Vec::new(),
            tiles: Vec::new(),
        }
    }

    /// Output views in order.
    fn slots(outputs: &[Output]) -> Vec<u8> {
        outputs.iter().map(Output::slot).collect()
    }

    #[test]
    fn key_frame_submits_and_shows() {
        let mut dpb = Vp9Dpb::new();
        let decoded = dpb.decode(&key(true)).expect("key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(slots(&decoded.outputs), vec![submission.slot]);
        let params = &submission.params;
        let format_flags = params.format_flags;
        let base_qindex = params.base_qindex;
        let first_partition_size = params.first_partition_size;
        let uncompressed_header_size = params.uncompressed_header_size;
        let feedback = params.feedback;
        assert_eq!((params.width, params.height), (320, 240));
        assert_eq!(params.profile, 0);
        assert_eq!(format_flags, 0x1A);
        assert_eq!(params.curr_pic.entry, 0);
        assert_eq!(params.bit_depth_luma_minus8, 0);
        assert_eq!(params.bit_depth_chroma_minus8, 0);
        assert!(params.ref_frame_map.iter().all(|entry| entry.entry == 0xFF));
        assert!(params.frame_refs.iter().all(|entry| entry.entry == 0xFF));
        assert_eq!(base_qindex, 100);
        assert_eq!(first_partition_size, 4);
        assert_eq!(uncompressed_header_size, 10);
        assert_eq!(feedback, 0);
        assert_eq!(submission.bitstream.len(), 16);
        assert_eq!(submission.slices.len(), 1);
        assert_eq!(
            (submission.slices[0].location, submission.slices[0].bytes),
            (14, 2)
        );
    }

    #[test]
    fn all_views_pinned_errors_no_free_slot() {
        let mut dpb = Vp9Dpb::new();
        for view in 0u8..16 {
            dpb.pins().pin(view);
        }
        let error = dpb.decode(&key(true)).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::NoFreeSlot));
        assert_eq!(error.to_string(), "no free surface slot");
    }

    #[test]
    fn inter_references_key() {
        let mut dpb = Vp9Dpb::new();
        dpb.decode(&key(true)).expect("key");
        let decoded = dpb.decode(&inter([0, 0, 0], 0x02, true)).expect("inter");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slot, 1);
        let params = &submission.params;
        assert_eq!(params.ref_frame_map[0].entry, 0);
        assert_eq!(
            (
                params.ref_frame_coded_width[0],
                params.ref_frame_coded_height[0]
            ),
            (320, 240)
        );
        assert_eq!(params.frame_refs[0].entry, 0);
        assert_eq!(params.format_flags & 1, 1);
        assert_eq!(slots(&decoded.outputs), vec![1]);
    }

    #[test]
    fn intra_only_skips_reference_check() {
        let mut dpb = Vp9Dpb::new();
        let mut head = header(FrameType::Inter, 0x02, true);
        head.flags |= FH_INTRA_ONLY;
        let decoded = dpb.decode(&frame(head)).expect("intra-only");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slot, 0);
        assert!(submission
            .params
            .frame_refs
            .iter()
            .all(|entry| entry.entry == 0xFF));
    }

    #[test]
    fn show_existing_outputs_slot() {
        let mut dpb = Vp9Dpb::new();
        dpb.decode(&key(true)).expect("key");
        dpb.decode(&inter([0, 0, 0], 0x02, false)).expect("hidden");
        let decoded = dpb
            .decode(&show_existing(1, FrameType::Inter))
            .expect("show existing");
        assert!(decoded.submission.is_none());
        assert_eq!(slots(&decoded.outputs), vec![1]);
        assert!(Vp9Dpb::drain().is_empty(), "vp9 never delays output");
    }

    #[test]
    fn show_existing_empty_slot_breaks() {
        let mut dpb = Vp9Dpb::new();
        let error = dpb.decode(&show_existing(2, FrameType::Inter)).unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d11Error::BrokenReference(_)
        ));
    }

    #[test]
    fn missing_reference_breaks() {
        let mut dpb = Vp9Dpb::new();
        let error = dpb.decode(&inter([0, 0, 0], 0x02, true)).unwrap_err();
        assert!(matches!(
            error,
            crate::error::D3d11Error::BrokenReference(_)
        ));
    }

    #[test]
    fn refresh_zero_uses_spare_view() {
        let mut dpb = Vp9Dpb::new();
        dpb.decode(&key(true)).expect("key");
        let decoded = dpb.decode(&inter([0, 0, 0], 0x00, true)).expect("inter");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slot, 1);
        assert!(submission
            .params
            .ref_frame_map
            .iter()
            .all(|entry| entry.entry == 0));
    }

    #[test]
    fn submission_carries_whole_frame() {
        let mut dpb = Vp9Dpb::new();
        let input = key(true);
        let decoded = dpb.decode(&input).expect("key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.bitstream, input.bytes);
        assert_eq!(submission.slices.len(), 1);
        let slice = &submission.slices[0];
        let start = usize::try_from(slice.location).expect("offset fits");
        let len = usize::try_from(slice.bytes).expect("size fits");
        assert_eq!(&submission.bitstream[start..start + len], &[0xAA, 0xBB]);
    }

    #[test]
    fn multi_tile_slices_skip_size_fields() {
        let mut dpb = Vp9Dpb::new();
        let head = header(FrameType::Key, 0xFF, true);
        let prefix = usize::from(head.uncompressed_size) + usize::from(head.header_size_in_bytes);
        let mut bytes = vec![0u8; prefix];
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&[0xA0, 0xA1, 0xA2]);
        bytes.extend_from_slice(&[0xB0, 0xB1]);
        let input = Frame {
            header: head,
            bytes,
            tiles: vec![
                TileData {
                    bytes: vec![0xA0, 0xA1, 0xA2],
                    row: 0,
                    column: 0,
                },
                TileData {
                    bytes: vec![0xB0, 0xB1],
                    row: 0,
                    column: 1,
                },
            ],
        };
        let decoded = dpb.decode(&input).expect("tiled key");
        let submission = decoded.submission.expect("submission");
        assert_eq!(submission.slices.len(), 2);
        let first = &submission.slices[0];
        let second = &submission.slices[1];
        assert_eq!(
            (first.location, first.bytes),
            (u32::try_from(prefix + 4).expect("offset fits"), 3)
        );
        assert_eq!(
            (second.location, second.bytes),
            (u32::try_from(prefix + 7).expect("offset fits"), 2)
        );
        let first_payload = &submission.bitstream[prefix + 4..prefix + 7];
        assert_eq!(first_payload, &[0xA0, 0xA1, 0xA2]);
        let second_payload = &submission.bitstream[prefix + 7..prefix + 9];
        assert_eq!(second_payload, &[0xB0, 0xB1]);
    }

    #[test]
    fn segmentation_fill_copies_maps() {
        let mut dpb = Vp9Dpb::new();
        let mut head = header(FrameType::Key, 0xFF, true);
        head.flags |=
            FH_SEG_ENABLED | FH_SEG_UPDATE_MAP | FH_SEG_TEMPORAL_UPDATE | FH_SEG_ABS_DELTA;
        head.tree_probs = [128; 7];
        head.pred_probs = [200; 3];
        head.seg_enabled[0] = [true, false, true, true];
        head.seg_value[0] = [5, 0, 2, 0];
        let decoded = dpb.decode(&frame(head)).expect("segmented key");
        let params = &decoded.submission.expect("submission").params;
        let feature_data = params.segmentation.feature_data;
        assert_eq!(params.segmentation.control, 0x0F);
        assert_eq!(params.segmentation.tree_probs, [128; 7]);
        assert_eq!(params.segmentation.pred_probs, [200; 3]);
        assert_eq!(feature_data[0], [5, 0, 2, 0]);
        assert_eq!(params.segmentation.feature_mask[0], 0b1101);
        assert!(params.segmentation.feature_mask[1..]
            .iter()
            .all(|mask| *mask == 0));
    }

    #[test]
    fn tile_overrun_rejects() {
        let mut dpb = Vp9Dpb::new();
        let head = header(FrameType::Key, 0xFF, true);
        let input = Frame {
            header: head,
            bytes: vec![0u8; 10],
            tiles: vec![TileData {
                bytes: vec![0xAA],
                row: 0,
                column: 0,
            }],
        };
        let error = dpb.decode(&input).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Parse(_)));
    }
}
