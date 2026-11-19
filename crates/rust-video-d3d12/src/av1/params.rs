//! AV1 DXVA picture structures behind D3D12 video decode.
//!
//! Field order matches dxva.h in the Windows SDK.

use std::mem::{offset_of, size_of};

/// AV1 reference motion flags: invalid bit.
pub(crate) const AV1_MOTION_INVALID: u8 = 1;
/// AV1 reference motion flags: type shift.
pub(crate) const AV1_MOTION_TYPE_SHIFT: u8 = 1;

/// AV1 reference entry: extent, global motion, slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicEntryAv1 {
    /// Reference width in luma samples.
    pub(crate) width: u32,
    /// Reference height in luma samples.
    pub(crate) height: u32,
    /// Global motion matrix.
    pub(crate) wmmat: [i32; 6],
    /// Packed motion flags.
    pub(crate) motion_flags: u8,
    /// Reference slot, 0xFF when absent.
    pub(crate) index: u8,
    /// Reserved.
    pub(crate) reserved: u16,
}

impl PicEntryAv1 {
    /// Entry naming no reference.
    pub(crate) const INVALID: Self = Self {
        width: 0,
        height: 0,
        wmmat: [0; 6],
        motion_flags: AV1_MOTION_INVALID,
        index: 0xFF,
        reserved: 0,
    };
}

/// AV1 tile grid: column count, row count, context tile, superblock extents.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TilesAv1 {
    /// Tile column count.
    pub(crate) cols: u8,
    /// Tile row count.
    pub(crate) rows: u8,
    /// Context update tile id.
    pub(crate) context_update_id: u16,
    /// Tile column widths in superblocks.
    pub(crate) widths: [u16; 64],
    /// Tile row heights in superblocks.
    pub(crate) heights: [u16; 64],
}

/// AV1 loop filter control: delta enabled bit.
pub(crate) const AV1_LF_DELTA_ENABLED: u8 = 1;
/// AV1 loop filter control: delta update bit.
pub(crate) const AV1_LF_DELTA_UPDATE: u8 = 1 << 1;
/// AV1 loop filter control: multi-delta bit.
pub(crate) const AV1_LF_DELTA_MULTI: u8 = 1 << 2;
/// AV1 loop filter control: delta present bit.
pub(crate) const AV1_LF_DELTA_PRESENT: u8 = 1 << 3;

/// AV1 loop filter parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LoopFilterAv1 {
    /// Luma filter levels.
    pub(crate) filter_level: [u8; 2],
    /// U filter level.
    pub(crate) filter_level_u: u8,
    /// V filter level.
    pub(crate) filter_level_v: u8,
    /// Sharpness level.
    pub(crate) sharpness: u8,
    /// Packed delta control flags.
    pub(crate) control: u8,
    /// Reference deltas.
    pub(crate) ref_deltas: [i8; 8],
    /// Mode deltas.
    pub(crate) mode_deltas: [i8; 2],
    /// Delta resolution.
    pub(crate) delta_lf_res: u8,
    /// Restoration types per plane.
    pub(crate) restoration_type: [u8; 3],
    /// Restoration unit sizes per plane.
    pub(crate) log2_restoration_unit_size: [u16; 3],
    /// Reserved.
    pub(crate) reserved: u16,
}

/// AV1 quantization control: delta present bit.
pub(crate) const AV1_Q_DELTA_PRESENT: u8 = 1;
/// AV1 quantization control: delta resolution shift.
pub(crate) const AV1_Q_DELTA_RES_SHIFT: u8 = 1;

/// AV1 quantization parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct QuantAv1 {
    /// Packed delta control flags.
    pub(crate) control: u8,
    /// Base quantizer index.
    pub(crate) base_qindex: u8,
    /// Y DC delta.
    pub(crate) y_dc_delta: i8,
    /// U DC delta.
    pub(crate) u_dc_delta: i8,
    /// V DC delta.
    pub(crate) v_dc_delta: i8,
    /// U AC delta.
    pub(crate) u_ac_delta: i8,
    /// V AC delta.
    pub(crate) v_ac_delta: i8,
    /// Y matrix id, 0xFF when unused.
    pub(crate) qm_y: u8,
    /// U matrix id, 0xFF when unused.
    pub(crate) qm_u: u8,
    /// V matrix id, 0xFF when unused.
    pub(crate) qm_v: u8,
    /// Reserved.
    pub(crate) reserved: u16,
}

/// AV1 CDEF control: damping shift.
pub(crate) const AV1_CDEF_DAMPING_SHIFT: u8 = 0;
/// AV1 CDEF control: strength bits shift.
pub(crate) const AV1_CDEF_BITS_SHIFT: u8 = 2;
/// AV1 CDEF strength: secondary shift.
pub(crate) const AV1_CDEF_SECONDARY_SHIFT: u8 = 6;

/// AV1 CDEF parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CdefAv1 {
    /// Packed damping and strength bits.
    pub(crate) control: u8,
    /// Y strengths: primary in bits 0-5, secondary in 6-7.
    pub(crate) y_strengths: [u8; 8],
    /// UV strengths: primary in bits 0-5, secondary in 6-7.
    pub(crate) uv_strengths: [u8; 8],
}

/// AV1 segmentation control: enabled bit.
pub(crate) const AV1_SEG_ENABLED: u8 = 1;
/// AV1 segmentation control: map update bit.
pub(crate) const AV1_SEG_UPDATE_MAP: u8 = 1 << 1;
/// AV1 segmentation control: data update bit.
pub(crate) const AV1_SEG_UPDATE_DATA: u8 = 1 << 2;
/// AV1 segmentation control: temporal update bit.
pub(crate) const AV1_SEG_TEMPORAL_UPDATE: u8 = 1 << 3;

/// AV1 segmentation parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SegmentationAv1 {
    /// Packed update control flags.
    pub(crate) control: u8,
    /// Reserved.
    pub(crate) reserved24: [u8; 3],
    /// Feature enable masks per segment.
    pub(crate) feature_mask: [u8; 8],
    /// Feature values per segment and level.
    pub(crate) feature_data: [[i16; 8]; 8],
}

/// AV1 film grain control: grain applied bit.
pub(crate) const AV1_GRAIN_APPLY: u16 = 1;
/// AV1 film grain control: scaling shift.
pub(crate) const AV1_GRAIN_SCALING_SHIFT: u16 = 1;
/// AV1 film grain control: chroma from luma bit.
pub(crate) const AV1_GRAIN_CHROMA_FROM_LUMA: u16 = 1 << 3;
/// AV1 film grain control: coefficient lag shift.
pub(crate) const AV1_GRAIN_COEFF_LAG_SHIFT: u16 = 4;
/// AV1 film grain control: coefficient shift.
pub(crate) const AV1_GRAIN_COEFF_SHIFT: u16 = 6;
/// AV1 film grain control: scale shift.
pub(crate) const AV1_GRAIN_SCALE_SHIFT: u16 = 8;
/// AV1 film grain control: overlap bit.
pub(crate) const AV1_GRAIN_OVERLAP: u16 = 1 << 10;
/// AV1 film grain control: restricted range bit.
pub(crate) const AV1_GRAIN_RESTRICTED_RANGE: u16 = 1 << 11;
/// AV1 film grain control: identity matrix bit.
pub(crate) const AV1_GRAIN_IDENTITY_MATRIX: u16 = 1 << 12;

/// AV1 film grain parameters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FilmGrainAv1 {
    /// Packed grain control flags.
    pub(crate) control: u16,
    /// Grain seed.
    pub(crate) grain_seed: u16,
    /// Y scaling points.
    pub(crate) scaling_points_y: [[u8; 2]; 14],
    /// Y scaling point count.
    pub(crate) num_y_points: u8,
    /// Cb scaling points.
    pub(crate) scaling_points_cb: [[u8; 2]; 10],
    /// Cb scaling point count.
    pub(crate) num_cb_points: u8,
    /// Cr scaling points.
    pub(crate) scaling_points_cr: [[u8; 2]; 10],
    /// Cr scaling point count.
    pub(crate) num_cr_points: u8,
    /// Y autoregressive coefficients.
    pub(crate) ar_coeffs_y: [u8; 24],
    /// Cb autoregressive coefficients.
    pub(crate) ar_coeffs_cb: [u8; 25],
    /// Cr autoregressive coefficients.
    pub(crate) ar_coeffs_cr: [u8; 25],
    /// Cb multiplier.
    pub(crate) cb_mult: u8,
    /// Cb luma multiplier.
    pub(crate) cb_luma_mult: u8,
    /// Cr multiplier.
    pub(crate) cr_mult: u8,
    /// Cr luma multiplier.
    pub(crate) cr_luma_mult: u8,
    /// Reserved.
    pub(crate) reserved: u8,
    /// Cb offset.
    pub(crate) cb_offset: i16,
    /// Cr offset.
    pub(crate) cr_offset: i16,
}

/// AV1 coding flags: 128x128 superblocks.
pub(crate) const AV1_CODING_SUPERBLOCK_128: u32 = 1;
/// AV1 coding flags: intra edge filter.
pub(crate) const AV1_CODING_INTRA_EDGE: u32 = 1 << 1;
/// AV1 coding flags: interintra compound.
pub(crate) const AV1_CODING_INTERINTRA: u32 = 1 << 2;
/// AV1 coding flags: masked compound.
pub(crate) const AV1_CODING_MASKED_COMPOUND: u32 = 1 << 3;
/// AV1 coding flags: warped motion.
pub(crate) const AV1_CODING_WARPED: u32 = 1 << 4;
/// AV1 coding flags: dual filter.
pub(crate) const AV1_CODING_DUAL_FILTER: u32 = 1 << 5;
/// AV1 coding flags: joint compound.
pub(crate) const AV1_CODING_JNT_COMP: u32 = 1 << 6;
/// AV1 coding flags: screen content tools.
pub(crate) const AV1_CODING_SCREEN_CONTENT: u32 = 1 << 7;
/// AV1 coding flags: integer motion vectors.
pub(crate) const AV1_CODING_INTEGER_MV: u32 = 1 << 8;
/// AV1 coding flags: CDEF.
pub(crate) const AV1_CODING_CDEF: u32 = 1 << 9;
/// AV1 coding flags: restoration.
pub(crate) const AV1_CODING_RESTORATION: u32 = 1 << 10;
/// AV1 coding flags: film grain.
pub(crate) const AV1_CODING_FILM_GRAIN: u32 = 1 << 11;
/// AV1 coding flags: intra block copy.
pub(crate) const AV1_CODING_INTRABC: u32 = 1 << 12;
/// AV1 coding flags: high precision motion vectors.
pub(crate) const AV1_CODING_HIGH_PRECISION_MV: u32 = 1 << 13;
/// AV1 coding flags: switchable motion mode.
pub(crate) const AV1_CODING_SWITCHABLE_MOTION: u32 = 1 << 14;
/// AV1 coding flags: filter intra.
pub(crate) const AV1_CODING_FILTER_INTRA: u32 = 1 << 15;
/// AV1 coding flags: no frame-end CDF update.
pub(crate) const AV1_CODING_NO_FRAME_END_CDF: u32 = 1 << 16;
/// AV1 coding flags: no CDF update.
pub(crate) const AV1_CODING_NO_CDF_UPDATE: u32 = 1 << 17;
/// AV1 coding flags: compound reference mode.
pub(crate) const AV1_CODING_REFERENCE_MODE: u32 = 1 << 18;
/// AV1 coding flags: skip mode present.
pub(crate) const AV1_CODING_SKIP_MODE: u32 = 1 << 19;
/// AV1 coding flags: reduced transform set.
pub(crate) const AV1_CODING_REDUCED_TX: u32 = 1 << 20;
/// AV1 coding flags: superres.
pub(crate) const AV1_CODING_SUPERRES: u32 = 1 << 21;
/// AV1 coding flags: transform mode shift.
pub(crate) const AV1_CODING_TX_MODE_SHIFT: u32 = 22;
/// AV1 coding flags: use reference motion vectors.
pub(crate) const AV1_CODING_USE_REF_MVS: u32 = 1 << 24;
/// AV1 coding flags: reference motion vectors enabled.
pub(crate) const AV1_CODING_REF_MVS_ENABLED: u32 = 1 << 25;
/// AV1 coding flags: reference frame update.
pub(crate) const AV1_CODING_REF_UPDATE: u32 = 1 << 26;

/// AV1 format flags: frame type shift.
pub(crate) const AV1_FORMAT_TYPE_SHIFT: u8 = 0;
/// AV1 format flags: show frame bit.
pub(crate) const AV1_FORMAT_SHOW_FRAME: u8 = 1 << 2;
/// AV1 format flags: showable frame bit.
pub(crate) const AV1_FORMAT_SHOWABLE_FRAME: u8 = 1 << 3;
/// AV1 format flags: horizontal subsampling bit.
pub(crate) const AV1_FORMAT_SUBSAMPLING_X: u8 = 1 << 4;
/// AV1 format flags: vertical subsampling bit.
pub(crate) const AV1_FORMAT_SUBSAMPLING_Y: u8 = 1 << 5;
/// AV1 format flags: monochrome bit.
pub(crate) const AV1_FORMAT_MONO_CHROME: u8 = 1 << 6;

/// AV1 picture parameters, 912 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PicParamsAv1 {
    /// Frame width in luma samples.
    pub(crate) width: u32,
    /// Frame height in luma samples.
    pub(crate) height: u32,
    /// Maximum frame width in luma samples.
    pub(crate) max_width: u32,
    /// Maximum frame height in luma samples.
    pub(crate) max_height: u32,
    /// Current texture index.
    pub(crate) curr_pic_index: u8,
    /// Superres denominator.
    pub(crate) superres_denom: u8,
    /// Bit depth.
    pub(crate) bitdepth: u8,
    /// Sequence profile.
    pub(crate) seq_profile: u8,
    /// Tile grid.
    pub(crate) tiles: TilesAv1,
    /// Packed coding tool flags.
    pub(crate) coding_flags: u32,
    /// Packed format flags.
    pub(crate) format_flags: u8,
    /// Primary reference frame.
    pub(crate) primary_ref_frame: u8,
    /// Frame order hint.
    pub(crate) order_hint: u8,
    /// Order hint bit count.
    pub(crate) order_hint_bits: u8,
    /// Reference entries per slot.
    pub(crate) frame_refs: [PicEntryAv1; 7],
    /// Texture index per reference slot.
    pub(crate) ref_map: [u8; 8],
    /// Loop filter parameters.
    pub(crate) loop_filter: LoopFilterAv1,
    /// Quantization parameters.
    pub(crate) quantization: QuantAv1,
    /// CDEF parameters.
    pub(crate) cdef: CdefAv1,
    /// Interpolation filter.
    pub(crate) interp_filter: u8,
    /// Segmentation parameters.
    pub(crate) segmentation: SegmentationAv1,
    /// Film grain parameters.
    pub(crate) film_grain: FilmGrainAv1,
    /// Reserved.
    pub(crate) reserved32: u32,
    /// Driver-echoed picture serial.
    pub(crate) feedback: u32,
}

impl PicParamsAv1 {
    /// Parameters with invalid references and map.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            width: 0,
            height: 0,
            max_width: 0,
            max_height: 0,
            curr_pic_index: 0xFF,
            superres_denom: 8,
            bitdepth: 8,
            seq_profile: 0,
            tiles: TilesAv1 {
                cols: 0,
                rows: 0,
                context_update_id: 0,
                widths: [0; 64],
                heights: [0; 64],
            },
            coding_flags: 0,
            format_flags: 0,
            primary_ref_frame: 7,
            order_hint: 0,
            order_hint_bits: 0,
            frame_refs: [PicEntryAv1::INVALID; 7],
            ref_map: [0xFF; 8],
            loop_filter: LoopFilterAv1 {
                filter_level: [0; 2],
                filter_level_u: 0,
                filter_level_v: 0,
                sharpness: 0,
                control: 0,
                ref_deltas: [0; 8],
                mode_deltas: [0; 2],
                delta_lf_res: 0,
                restoration_type: [0; 3],
                log2_restoration_unit_size: [8; 3],
                reserved: 0,
            },
            quantization: QuantAv1 {
                control: 0,
                base_qindex: 0,
                y_dc_delta: 0,
                u_dc_delta: 0,
                v_dc_delta: 0,
                u_ac_delta: 0,
                v_ac_delta: 0,
                qm_y: 0xFF,
                qm_u: 0xFF,
                qm_v: 0xFF,
                reserved: 0,
            },
            cdef: CdefAv1 {
                control: 0,
                y_strengths: [0; 8],
                uv_strengths: [0; 8],
            },
            interp_filter: 0,
            segmentation: SegmentationAv1 {
                control: 0,
                reserved24: [0; 3],
                feature_mask: [0; 8],
                feature_data: [[0; 8]; 8],
            },
            film_grain: FilmGrainAv1 {
                control: 0,
                grain_seed: 0,
                scaling_points_y: [[0; 2]; 14],
                num_y_points: 0,
                scaling_points_cb: [[0; 2]; 10],
                num_cb_points: 0,
                scaling_points_cr: [[0; 2]; 10],
                num_cr_points: 0,
                ar_coeffs_y: [0; 24],
                ar_coeffs_cb: [0; 25],
                ar_coeffs_cr: [0; 25],
                cb_mult: 0,
                cb_luma_mult: 0,
                cr_mult: 0,
                cr_luma_mult: 0,
                reserved: 0,
                cb_offset: 0,
                cr_offset: 0,
            },
            reserved32: 0,
            feedback: 0,
        }
    }
}

/// AV1 tile entry, 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TileAv1 {
    /// Tile offset in the bitstream buffer.
    pub(crate) data_offset: u32,
    /// Tile length in bytes.
    pub(crate) data_size: u32,
    /// Tile row.
    pub(crate) row: u16,
    /// Tile column.
    pub(crate) column: u16,
    /// Reserved.
    pub(crate) reserved: u16,
    /// Anchor frame.
    pub(crate) anchor_frame: u8,
    /// Reserved.
    pub(crate) reserved8: u8,
}

/// Sizes and offsets against dxva.h; the driver reads these bytes verbatim.
const _: () = {
    assert!(size_of::<PicEntryAv1>() == 36);
    assert!(size_of::<TilesAv1>() == 260);
    assert!(size_of::<LoopFilterAv1>() == 28);
    assert!(size_of::<QuantAv1>() == 12);
    assert!(size_of::<CdefAv1>() == 17);
    assert!(size_of::<SegmentationAv1>() == 140);
    assert!(size_of::<FilmGrainAv1>() == 158);
    assert!(size_of::<PicParamsAv1>() == 912);
    assert!(size_of::<TileAv1>() == 16);
    assert!(offset_of!(PicParamsAv1, tiles) == 20);
    assert!(offset_of!(PicParamsAv1, coding_flags) == 280);
    assert!(offset_of!(PicParamsAv1, format_flags) == 284);
    assert!(offset_of!(PicParamsAv1, frame_refs) == 288);
    assert!(offset_of!(PicParamsAv1, ref_map) == 540);
    assert!(offset_of!(PicParamsAv1, loop_filter) == 548);
    assert!(offset_of!(PicParamsAv1, quantization) == 576);
    assert!(offset_of!(PicParamsAv1, cdef) == 588);
    assert!(offset_of!(PicParamsAv1, interp_filter) == 605);
    assert!(offset_of!(PicParamsAv1, segmentation) == 606);
    assert!(offset_of!(PicParamsAv1, film_grain) == 746);
    assert!(offset_of!(PicParamsAv1, feedback) == 908);
    assert!(offset_of!(TileAv1, anchor_frame) == 14);
};

#[cfg(test)]
mod tests {
    use super::{PicEntryAv1, PicParamsAv1};

    #[test]
    fn av1_empty_invalidates_map() {
        let params = PicParamsAv1::empty();
        assert!(params.ref_map.iter().all(|index| *index == 0xFF));
        assert!(params.frame_refs.iter().all(|entry| entry.index == 0xFF));
        assert_eq!(params.superres_denom, 8);
        assert_eq!(params.primary_ref_frame, 7);
        assert_eq!(params.quantization.qm_y, 0xFF);
        assert_eq!(PicEntryAv1::INVALID.index, 0xFF);
    }
}
