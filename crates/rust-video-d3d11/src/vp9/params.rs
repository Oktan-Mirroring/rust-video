//! VP9 DXVA picture structures behind D3D11 video decode.
//!
//! Field order matches dxva.h in the Windows SDK.

use std::mem::{offset_of, size_of};

// dxva.h packs every struct below to 1 byte; the siblings above stay
// naturally aligned because their members already land aligned.

/// VP9 format flags: frame type bit.
pub(crate) const VP9_FORMAT_FRAME_TYPE: u16 = 1;
/// VP9 format flags: show frame bit.
pub(crate) const VP9_FORMAT_SHOW_FRAME: u16 = 1 << 1;
/// VP9 format flags: error resilient mode bit.
pub(crate) const VP9_FORMAT_ERROR_RESILIENT: u16 = 1 << 2;
/// VP9 format flags: horizontal subsampling bit.
pub(crate) const VP9_FORMAT_SUBSAMPLING_X: u16 = 1 << 3;
/// VP9 format flags: vertical subsampling bit.
pub(crate) const VP9_FORMAT_SUBSAMPLING_Y: u16 = 1 << 4;
/// VP9 format flags: refresh frame context bit.
pub(crate) const VP9_FORMAT_REFRESH_CONTEXT: u16 = 1 << 6;
/// VP9 format flags: frame parallel decoding mode bit.
pub(crate) const VP9_FORMAT_FRAME_PARALLEL: u16 = 1 << 7;
/// VP9 format flags: intra-only bit.
pub(crate) const VP9_FORMAT_INTRA_ONLY: u16 = 1 << 8;
/// VP9 format flags: frame context index shift.
pub(crate) const VP9_FORMAT_CONTEXT_SHIFT: u16 = 9;
/// VP9 format flags: reset frame context shift.
pub(crate) const VP9_FORMAT_RESET_SHIFT: u16 = 11;
/// VP9 format flags: high precision motion vectors bit.
pub(crate) const VP9_FORMAT_HIGH_PRECISION_MV: u16 = 1 << 13;

/// `VPx` picture entry: surface index plus one flag bit.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicEntryVpx {
    /// Index in bits 0-6, flag in bit 7.
    pub(crate) entry: u8,
}

impl PicEntryVpx {
    /// Entry naming no surface.
    pub(crate) const INVALID: Self = Self { entry: 0xFF };

    /// Entry for surface `index` without the flag.
    #[must_use]
    pub(crate) const fn frame(index: u8) -> Self {
        Self {
            entry: index & 0x7F,
        }
    }
}

/// VP9 segmentation control: enabled bit.
pub(crate) const VP9_SEG_ENABLED: u8 = 1;
/// VP9 segmentation control: map update bit.
pub(crate) const VP9_SEG_UPDATE_MAP: u8 = 1 << 1;
/// VP9 segmentation control: temporal update bit.
pub(crate) const VP9_SEG_TEMPORAL_UPDATE: u8 = 1 << 2;
/// VP9 segmentation control: absolute delta bit.
pub(crate) const VP9_SEG_ABS_DELTA: u8 = 1 << 3;

/// VP9 segmentation parameters, 83 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SegmentationVp9 {
    /// Packed update control flags.
    pub(crate) control: u8,
    /// Segment tree probabilities.
    pub(crate) tree_probs: [u8; 7],
    /// Segment prediction probabilities.
    pub(crate) pred_probs: [u8; 3],
    /// Feature values per segment and level.
    pub(crate) feature_data: [[i16; 4]; 8],
    /// Feature enable masks per segment.
    pub(crate) feature_mask: [u8; 8],
}

/// VP9 loop filter control: delta enabled bit.
pub(crate) const VP9_CONTROL_DELTA_ENABLED: u8 = 1;
/// VP9 loop filter control: delta update bit.
pub(crate) const VP9_CONTROL_DELTA_UPDATE: u8 = 1 << 1;

/// VP9 picture parameters, 208 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsVp9 {
    /// Current surface.
    pub(crate) curr_pic: PicEntryVpx,
    /// Profile id.
    pub(crate) profile: u8,
    /// Packed format and picture flags.
    pub(crate) format_flags: u16,
    /// Frame width in luma samples.
    pub(crate) width: u32,
    /// Frame height in luma samples.
    pub(crate) height: u32,
    /// Luma bit depth minus 8.
    pub(crate) bit_depth_luma_minus8: u8,
    /// Chroma bit depth minus 8.
    pub(crate) bit_depth_chroma_minus8: u8,
    /// Interpolation filter id.
    pub(crate) interp_filter: u8,
    /// Reserved.
    pub(crate) reserved_8: u8,
    /// Surface per reference slot.
    pub(crate) ref_frame_map: [PicEntryVpx; 8],
    /// Coded width per reference slot.
    pub(crate) ref_frame_coded_width: [u32; 8],
    /// Coded height per reference slot.
    pub(crate) ref_frame_coded_height: [u32; 8],
    /// Reference surfaces per position.
    pub(crate) frame_refs: [PicEntryVpx; 3],
    /// Reference sign bias per frame.
    pub(crate) ref_frame_sign_bias: [i8; 4],
    /// Loop filter level.
    pub(crate) filter_level: i8,
    /// Loop filter sharpness.
    pub(crate) sharpness_level: i8,
    /// Packed loop filter control flags.
    pub(crate) control_flags: u8,
    /// Loop filter reference deltas.
    pub(crate) ref_deltas: [i8; 4],
    /// Loop filter mode deltas.
    pub(crate) mode_deltas: [i8; 2],
    /// Base quantizer index.
    pub(crate) base_qindex: i16,
    /// Y DC delta.
    pub(crate) y_dc_delta_q: i8,
    /// UV DC delta.
    pub(crate) uv_dc_delta_q: i8,
    /// UV AC delta.
    pub(crate) uv_ac_delta_q: i8,
    /// Segmentation parameters.
    pub(crate) segmentation: SegmentationVp9,
    /// Tile column shift.
    pub(crate) log2_tile_cols: u8,
    /// Tile row shift.
    pub(crate) log2_tile_rows: u8,
    /// Uncompressed header size in bytes.
    pub(crate) uncompressed_header_size: u16,
    /// First partition size in bytes.
    pub(crate) first_partition_size: u16,
    /// Reserved.
    pub(crate) reserved_16: u16,
    /// Reserved.
    pub(crate) reserved_32: u32,
    /// Driver-echoed picture serial.
    pub(crate) feedback: u32,
}

impl PicParamsVp9 {
    /// Parameters with invalid references and map.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            curr_pic: PicEntryVpx::INVALID,
            profile: 0,
            format_flags: 0,
            width: 0,
            height: 0,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            interp_filter: 0,
            reserved_8: 0,
            ref_frame_map: [PicEntryVpx::INVALID; 8],
            ref_frame_coded_width: [0; 8],
            ref_frame_coded_height: [0; 8],
            frame_refs: [PicEntryVpx::INVALID; 3],
            ref_frame_sign_bias: [0; 4],
            filter_level: 0,
            sharpness_level: 0,
            control_flags: 0,
            ref_deltas: [0; 4],
            mode_deltas: [0; 2],
            base_qindex: 0,
            y_dc_delta_q: 0,
            uv_dc_delta_q: 0,
            uv_ac_delta_q: 0,
            segmentation: SegmentationVp9 {
                control: 0,
                tree_probs: [0; 7],
                pred_probs: [0; 3],
                feature_data: [[0; 4]; 8],
                feature_mask: [0; 8],
            },
            log2_tile_cols: 0,
            log2_tile_rows: 0,
            uncompressed_header_size: 0,
            first_partition_size: 0,
            reserved_16: 0,
            reserved_32: 0,
            feedback: 0,
        }
    }
}

/// `VPx` short slice control entry, 10 bytes.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SliceVpxShort {
    /// Tile offset in the bitstream buffer.
    pub(crate) location: u32,
    /// Tile length in bytes.
    pub(crate) bytes: u32,
    /// Bad slice chopping flag.
    pub(crate) chopping: u16,
}

/// Sizes and offsets against dxva.h; the driver reads these bytes verbatim.
const _: () = {
    assert!(size_of::<PicEntryVpx>() == 1);
    assert!(size_of::<SegmentationVp9>() == 83);
    assert!(size_of::<PicParamsVp9>() == 208);
    assert!(size_of::<SliceVpxShort>() == 10);
    assert!(offset_of!(SegmentationVp9, feature_data) == 11);
    assert!(offset_of!(SegmentationVp9, feature_mask) == 75);
    assert!(offset_of!(PicParamsVp9, format_flags) == 2);
    assert!(offset_of!(PicParamsVp9, width) == 4);
    assert!(offset_of!(PicParamsVp9, ref_frame_map) == 16);
    assert!(offset_of!(PicParamsVp9, ref_frame_coded_width) == 24);
    assert!(offset_of!(PicParamsVp9, frame_refs) == 88);
    assert!(offset_of!(PicParamsVp9, base_qindex) == 104);
    assert!(offset_of!(PicParamsVp9, segmentation) == 109);
    assert!(offset_of!(PicParamsVp9, uncompressed_header_size) == 194);
    assert!(offset_of!(PicParamsVp9, first_partition_size) == 196);
    assert!(offset_of!(PicParamsVp9, feedback) == 204);
};

#[cfg(test)]
mod tests {
    use super::{PicEntryVpx, PicParamsVp9};

    #[test]
    fn vpx_entry_names_surface() {
        assert_eq!(PicEntryVpx::frame(3).entry, 3);
        assert_eq!(PicEntryVpx::INVALID.entry, 0xFF);
    }

    #[test]
    fn vp9_empty_invalidates_map() {
        let params = PicParamsVp9::empty();
        let curr = params.curr_pic.entry;
        let feedback = params.feedback;
        assert_eq!(curr, 0xFF);
        assert!(params.ref_frame_map.iter().all(|entry| entry.entry == 0xFF));
        assert!(params.frame_refs.iter().all(|entry| entry.entry == 0xFF));
        assert_eq!(feedback, 0);
    }
}
