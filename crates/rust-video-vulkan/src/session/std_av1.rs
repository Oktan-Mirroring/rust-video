//! AV1 standard structures from parsed headers.
//!
//! Pure fills; pointers borrow box-stable backing owned by the tables.

use ash::vk::native::{
    __BindgenBitfieldUnit, StdVideoAV1CDEF, StdVideoAV1ColorConfig, StdVideoAV1ColorConfigFlags,
    StdVideoAV1FilmGrain, StdVideoAV1FilmGrainFlags, StdVideoAV1GlobalMotion,
    StdVideoAV1LoopFilter, StdVideoAV1LoopFilterFlags, StdVideoAV1LoopRestoration,
    StdVideoAV1Quantization, StdVideoAV1QuantizationFlags, StdVideoAV1Segmentation,
    StdVideoAV1SequenceHeader, StdVideoAV1SequenceHeaderFlags, StdVideoAV1TileInfo,
    StdVideoAV1TileInfoFlags, StdVideoAV1TimingInfo, StdVideoAV1TimingInfoFlags,
    StdVideoDecodeAV1PictureInfo, StdVideoDecodeAV1PictureInfoFlags,
    StdVideoDecodeAV1ReferenceInfo, StdVideoDecodeAV1ReferenceInfoFlags,
};

use crate::parse::av1::{FrameHeader, SequenceHeader};

/// AV1 sequence tables with box-stable standard-structure backing.
pub(crate) struct Av1Tables {
    /// Active sequence entry, replaced on sequence change.
    seq: Option<SeqEntry>,
}

/// Box-stable backing of one sequence header.
struct SeqEntry {
    /// Color config behind the header pointer.
    color: Box<StdVideoAV1ColorConfig>,
    /// Timing info behind the header pointer.
    timing: Box<StdVideoAV1TimingInfo>,
    /// Sequence header pointing at the boxes above.
    header: Box<StdVideoAV1SequenceHeader>,
}

impl Av1Tables {
    /// Empty tables.
    pub(crate) fn new() -> Self {
        Self { seq: None }
    }

    /// Standard sequence header for `seq`, replacing any entry.
    pub(crate) fn insert(&mut self, seq: &SequenceHeader) {
        let color = Box::new(fill_color(seq));
        let timing = Box::new(fill_timing(seq));
        let mut header = Box::new(fill_sequence(seq));
        header.pColorConfig = std::ptr::from_ref(color.as_ref());
        header.pTimingInfo = std::ptr::from_ref(timing.as_ref());
        self.seq = Some(SeqEntry {
            color,
            timing,
            header,
        });
    }

    /// Standard sequence header, if stored.
    ///
    /// Links the stored color and timing boxes on every read, so the
    /// insert order of the three boxes never matters.
    pub(crate) fn sequence(&mut self) -> Option<&StdVideoAV1SequenceHeader> {
        let entry = self.seq.as_mut()?;
        entry.header.pColorConfig = std::ptr::from_ref(entry.color.as_ref());
        entry.header.pTimingInfo = std::ptr::from_ref(entry.timing.as_ref());
        Some(entry.header.as_ref())
    }
}

impl Default for Av1Tables {
    fn default() -> Self {
        Self::new()
    }
}

/// Standard color config of `seq`.
fn fill_color(seq: &SequenceHeader) -> StdVideoAV1ColorConfig {
    let color = seq.color();
    let mut flags = StdVideoAV1ColorConfigFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_mono_chrome(u32::from(color.depth.mono_chrome));
    flags.set_color_range(u32::from(color.color_range));
    flags.set_separate_uv_delta_q(u32::from(color.separate_uv_delta_q));
    flags.set_color_description_present_flag(u32::from(color.color_description_present));
    StdVideoAV1ColorConfig {
        flags,
        BitDepth: seq.bit_depth(),
        subsampling_x: color.subsampling_x,
        subsampling_y: color.subsampling_y,
        reserved1: 0,
        color_primaries: u32::from(color.color_primaries),
        transfer_characteristics: u32::from(color.transfer_characteristics),
        matrix_coefficients: u32::from(color.matrix_coefficients),
        chroma_sample_position: u32::from(color.chroma_sample_position),
    }
}

/// Standard timing info of `seq`.
fn fill_timing(seq: &SequenceHeader) -> StdVideoAV1TimingInfo {
    let timing = seq.timing();
    let mut flags = StdVideoAV1TimingInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_equal_picture_interval(u32::from(timing.equal_picture_interval));
    StdVideoAV1TimingInfo {
        flags,
        num_units_in_display_tick: timing.num_units_in_display_tick,
        time_scale: timing.time_scale,
        num_ticks_per_picture_minus_1: timing.num_ticks_per_picture_minus_1,
    }
}

/// Standard sequence header of `seq`, without color and timing links.
fn fill_sequence(seq: &SequenceHeader) -> StdVideoAV1SequenceHeader {
    let mut flags = StdVideoAV1SequenceHeaderFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_still_picture(u32::from(seq.still_picture()));
    flags.set_reduced_still_picture_header(u32::from(seq.reduced()));
    flags.set_use_128x128_superblock(u32::from(seq.use_128x128_superblock()));
    flags.set_enable_filter_intra(u32::from(seq.enable_filter_intra()));
    flags.set_enable_intra_edge_filter(u32::from(seq.enable_intra_edge_filter()));
    flags.set_enable_interintra_compound(u32::from(seq.enable_interintra_compound()));
    flags.set_enable_masked_compound(u32::from(seq.enable_masked_compound()));
    flags.set_enable_warped_motion(u32::from(seq.enable_warped_motion()));
    flags.set_enable_dual_filter(u32::from(seq.enable_dual_filter()));
    flags.set_enable_order_hint(u32::from(seq.enable_order_hint()));
    flags.set_enable_jnt_comp(u32::from(seq.enable_jnt_comp()));
    flags.set_enable_ref_frame_mvs(u32::from(seq.enable_ref_frame_mvs()));
    flags.set_frame_id_numbers_present_flag(u32::from(seq.frame_id_numbers_present()));
    flags.set_enable_superres(u32::from(seq.enable_superres()));
    flags.set_enable_cdef(u32::from(seq.enable_cdef()));
    flags.set_enable_restoration(u32::from(seq.enable_restoration()));
    flags.set_film_grain_params_present(u32::from(seq.film_grain_params_present()));
    flags.set_timing_info_present_flag(u32::from(seq.timing_present()));
    flags.set_initial_display_delay_present_flag(u32::from(seq.initial_display_delay_present()));
    StdVideoAV1SequenceHeader {
        flags,
        seq_profile: u32::from(seq.profile()),
        frame_width_bits_minus_1: seq.frame_width_bits_minus_1(),
        frame_height_bits_minus_1: seq.frame_height_bits_minus_1(),
        max_frame_width_minus_1: seq.max_frame_width_minus_1(),
        max_frame_height_minus_1: seq.max_frame_height_minus_1(),
        delta_frame_id_length_minus_2: seq.delta_frame_id_length_minus_2(),
        additional_frame_id_length_minus_1: seq.additional_frame_id_length_minus_1(),
        order_hint_bits_minus_1: seq.order_hint_bits_minus_1(),
        seq_force_integer_mv: seq.seq_force_integer_mv(),
        seq_force_screen_content_tools: seq.seq_force_screen_content_tools(),
        reserved1: [0; 5],
        pColorConfig: std::ptr::null(),
        pTimingInfo: std::ptr::null(),
    }
}

/// Standard picture info of one frame with box-stable backing.
///
/// The info borrows the boxed sub-structures; moving the owner keeps the
/// heap boxes stable, so the pointers hold until the owner drops.
pub(crate) struct Av1PictureStd {
    /// Picture info pointing at the boxes below.
    info: StdVideoDecodeAV1PictureInfo,
    /// Tile info behind the picture pointer.
    tile: Box<StdVideoAV1TileInfo>,
    /// MI column starts behind the tile pointer.
    mi_col_starts: Box<[u16]>,
    /// MI row starts behind the tile pointer.
    mi_row_starts: Box<[u16]>,
    /// Superblock widths behind the tile pointer.
    width_sb: Box<[u16]>,
    /// Superblock heights behind the tile pointer.
    height_sb: Box<[u16]>,
    /// Quantizer behind the picture pointer.
    quant: Box<StdVideoAV1Quantization>,
    /// Segmentation behind the picture pointer.
    segmentation: Box<StdVideoAV1Segmentation>,
    /// Loop filter behind the picture pointer.
    loop_filter: Box<StdVideoAV1LoopFilter>,
    /// CDEF behind the picture pointer.
    cdef: Box<StdVideoAV1CDEF>,
    /// Loop restoration behind the picture pointer.
    restoration: Box<StdVideoAV1LoopRestoration>,
    /// Global motion behind the picture pointer.
    global: Box<StdVideoAV1GlobalMotion>,
    /// Film grain behind the picture pointer, linked only when applied.
    grain: Box<StdVideoAV1FilmGrain>,
}

impl Av1PictureStd {
    /// Standard picture info of `header` with resolved order hints.
    ///
    /// `order_hints` carries the stored order hint per reference name;
    /// `expected` carries the expected frame id per name. `super128`
    /// selects the 128x128 superblock grid behind the tile starts.
    pub(crate) fn fill(
        header: &FrameHeader,
        super128: bool,
        order_hints: [u8; 8],
        expected: [u32; 8],
    ) -> Self {
        let tiles = header.tiles();
        let cols = usize::from(tiles.cols);
        let rows = usize::from(tiles.rows);
        let shift = if super128 { 5 } else { 4 };
        let mut mi_col_starts = vec![0_u16; cols + 1];
        for (index, start) in mi_col_starts.iter_mut().enumerate().take(cols) {
            *start = u16::from(tiles.start_col_sb[index]) << shift;
        }
        mi_col_starts[cols] = mi_units(header.sizes().frame_width);
        let mut mi_row_starts = vec![0_u16; rows + 1];
        for (index, start) in mi_row_starts.iter_mut().enumerate().take(rows) {
            *start = u16::from(tiles.start_row_sb[index]) << shift;
        }
        mi_row_starts[rows] = mi_units(header.sizes().frame_height);
        let mut width_sb = vec![0_u16; cols];
        for (index, width) in width_sb.iter_mut().enumerate() {
            *width = u16::from(tiles.width_in_sbs_minus_1[index]);
        }
        let mut height_sb = vec![0_u16; rows];
        for (index, height) in height_sb.iter_mut().enumerate() {
            *height = u16::from(tiles.height_in_sbs_minus_1[index]);
        }
        let mut tile_flags = StdVideoAV1TileInfoFlags {
            _bitfield_align_1: [],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
        };
        tile_flags.set_uniform_tile_spacing_flag(u32::from(tiles.uniform));
        let tile = Box::new(StdVideoAV1TileInfo {
            flags: tile_flags,
            TileCols: tiles.cols,
            TileRows: tiles.rows,
            context_update_tile_id: tiles.context_update_tile_id,
            tile_size_bytes_minus_1: tiles.size_bytes_minus_1,
            reserved1: [0; 7],
            pMiColStarts: std::ptr::null(),
            pMiRowStarts: std::ptr::null(),
            pWidthInSbsMinus1: std::ptr::null(),
            pHeightInSbsMinus1: std::ptr::null(),
        });
        let quant = Box::new(fill_quant(header));
        let segmentation = Box::new(fill_segmentation(header));
        let loop_filter = Box::new(fill_loop_filter(header));
        let cdef = Box::new(fill_cdef(header));
        let restoration = Box::new(fill_restoration(header));
        let global = Box::new(fill_global(header));
        let grain = Box::new(fill_grain(header));
        let mut owned = Self {
            info: fill_picture_info(header, order_hints, expected),
            tile,
            mi_col_starts: mi_col_starts.into_boxed_slice(),
            mi_row_starts: mi_row_starts.into_boxed_slice(),
            width_sb: width_sb.into_boxed_slice(),
            height_sb: height_sb.into_boxed_slice(),
            quant,
            segmentation,
            loop_filter,
            cdef,
            restoration,
            global,
            grain,
        };
        owned.tile.pMiColStarts = owned.mi_col_starts.as_ptr();
        owned.tile.pMiRowStarts = owned.mi_row_starts.as_ptr();
        owned.tile.pWidthInSbsMinus1 = owned.width_sb.as_ptr();
        owned.tile.pHeightInSbsMinus1 = owned.height_sb.as_ptr();
        owned.info.pTileInfo = std::ptr::from_ref(owned.tile.as_ref());
        owned.info.pQuantization = std::ptr::from_ref(owned.quant.as_ref());
        owned.info.pSegmentation = std::ptr::from_ref(owned.segmentation.as_ref());
        owned.info.pLoopFilter = std::ptr::from_ref(owned.loop_filter.as_ref());
        owned.info.pCDEF = std::ptr::from_ref(owned.cdef.as_ref());
        owned.info.pLoopRestoration = std::ptr::from_ref(owned.restoration.as_ref());
        owned.info.pGlobalMotion = std::ptr::from_ref(owned.global.as_ref());
        if header.grain().apply_grain {
            owned.info.pFilmGrain = std::ptr::from_ref(owned.grain.as_ref());
        }
        owned
    }

    /// Standard picture info.
    pub(crate) fn info(&self) -> &StdVideoDecodeAV1PictureInfo {
        &self.info
    }
}

/// Mode-info units of `samples`.
fn mi_units(samples: u32) -> u16 {
    u16::try_from(2 * samples.div_ceil(8)).unwrap_or(u16::MAX)
}

/// Signed bytes of `src`.
fn signed<const N: usize>(src: &[u8; N]) -> [i8; N] {
    let mut out = [0_i8; N];
    for (index, coeff) in out.iter_mut().enumerate() {
        *coeff = src[index].cast_signed();
    }
    out
}

/// Standard picture info shell of `header`, without sub-structure links.
fn fill_picture_info(
    header: &FrameHeader,
    order_hints: [u8; 8],
    expected: [u32; 8],
) -> StdVideoDecodeAV1PictureInfo {
    let mut flags = StdVideoDecodeAV1PictureInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_error_resilient_mode(u32::from(header.error_resilient_mode()));
    flags.set_disable_cdf_update(u32::from(header.disable_cdf_update()));
    flags.set_use_superres(u32::from(header.use_superres()));
    flags.set_render_and_frame_size_different(u32::from(header.render_and_frame_size_different()));
    flags.set_allow_screen_content_tools(u32::from(header.allow_screen_content_tools()));
    flags.set_is_filter_switchable(u32::from(header.is_filter_switchable()));
    flags.set_force_integer_mv(u32::from(header.force_integer_mv()));
    flags.set_frame_size_override_flag(u32::from(header.frame_size_override_flag()));
    flags
        .set_buffer_removal_time_present_flag(u32::from(header.buffer_removal_time_present_flag()));
    flags.set_allow_intrabc(u32::from(header.allow_intrabc()));
    flags.set_frame_refs_short_signaling(u32::from(header.refs().short_signaling));
    flags.set_allow_high_precision_mv(u32::from(header.allow_high_precision_mv()));
    flags.set_is_motion_mode_switchable(u32::from(header.is_motion_mode_switchable()));
    flags.set_use_ref_frame_mvs(u32::from(header.use_ref_frame_mvs()));
    flags.set_disable_frame_end_update_cdf(u32::from(header.disable_frame_end_update_cdf()));
    flags.set_allow_warped_motion(u32::from(header.allow_warped_motion()));
    flags.set_reduced_tx_set(u32::from(header.reduced_tx_set()));
    flags.set_reference_select(u32::from(header.reference_select()));
    flags.set_skip_mode_present(u32::from(header.skip_mode_present()));
    flags.set_delta_q_present(u32::from(header.delta_q_present()));
    flags.set_delta_lf_present(u32::from(header.delta_lf_present()));
    flags.set_delta_lf_multi(u32::from(header.delta_lf_multi()));
    flags.set_segmentation_enabled(u32::from(header.segmentation().enabled));
    flags.set_segmentation_update_map(u32::from(header.segmentation().update.update_map));
    flags.set_segmentation_temporal_update(u32::from(header.segmentation().update.temporal_update));
    flags.set_segmentation_update_data(u32::from(header.segmentation().update.update_data));
    let restoration = header.restoration();
    flags.set_UsesLr(u32::from(restoration.lr_type.iter().any(|lr| *lr != 0)));
    flags.set_usesChromaLr(u32::from(
        restoration.lr_type[1] != 0 || restoration.lr_type[2] != 0,
    ));
    flags.set_apply_grain(u32::from(header.grain().apply_grain));
    StdVideoDecodeAV1PictureInfo {
        flags,
        frame_type: u32::from(header.frame_type()),
        current_frame_id: header.current_frame_id(),
        OrderHint: header.order_hint(),
        primary_ref_frame: header.primary_ref_frame(),
        refresh_frame_flags: header.refresh_frame_flags(),
        reserved1: 0,
        interpolation_filter: u32::from(header.interpolation_filter()),
        TxMode: u32::from(header.tx_mode()),
        delta_q_res: header.delta_q_res(),
        delta_lf_res: header.delta_lf_res(),
        SkipModeFrame: *header.skip_mode_frames(),
        coded_denom: header.coded_denom(),
        reserved2: [0; 3],
        OrderHints: order_hints,
        expectedFrameId: expected,
        pTileInfo: std::ptr::null(),
        pQuantization: std::ptr::null(),
        pSegmentation: std::ptr::null(),
        pLoopFilter: std::ptr::null(),
        pCDEF: std::ptr::null(),
        pLoopRestoration: std::ptr::null(),
        pGlobalMotion: std::ptr::null(),
        pFilmGrain: std::ptr::null(),
    }
}

/// Standard quantizer of `header`.
fn fill_quant(header: &FrameHeader) -> StdVideoAV1Quantization {
    let quant = header.quant();
    let mut flags = StdVideoAV1QuantizationFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_using_qmatrix(u32::from(quant.using_qmatrix));
    flags.set_diff_uv_delta(u32::from(quant.diff_uv_delta));
    StdVideoAV1Quantization {
        flags,
        base_q_idx: quant.base_q_idx,
        DeltaQYDc: quant.delta_q_y_dc,
        DeltaQUDc: quant.delta_q_u_dc,
        DeltaQUAc: quant.delta_q_u_ac,
        DeltaQVDc: quant.delta_q_v_dc,
        DeltaQVAc: quant.delta_q_v_ac,
        qm_y: quant.qm_y,
        qm_u: quant.qm_u,
        qm_v: quant.qm_v,
    }
}

/// Standard segmentation of `header`.
fn fill_segmentation(header: &FrameHeader) -> StdVideoAV1Segmentation {
    let seg = header.segmentation();
    let mut enabled = [0_u8; 8];
    let mut data = [[0_i16; 8]; 8];
    for segment in 0..8 {
        for level in 0..8 {
            if seg.feature_enabled[segment][level] {
                enabled[segment] |= 1 << level;
            }
            data[segment][level] = seg.feature_value[segment][level];
        }
    }
    StdVideoAV1Segmentation {
        FeatureEnabled: enabled,
        FeatureData: data,
    }
}

/// Standard loop filter of `header`.
fn fill_loop_filter(header: &FrameHeader) -> StdVideoAV1LoopFilter {
    let filter = header.loop_filter();
    let mut flags = StdVideoAV1LoopFilterFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_loop_filter_delta_enabled(u32::from(filter.delta_enabled));
    flags.set_loop_filter_delta_update(u32::from(filter.delta_update));
    StdVideoAV1LoopFilter {
        flags,
        loop_filter_level: filter.level,
        loop_filter_sharpness: filter.sharpness,
        update_ref_delta: u8::from(filter.delta_update),
        loop_filter_ref_deltas: filter.ref_deltas,
        update_mode_delta: u8::from(filter.delta_update),
        loop_filter_mode_deltas: filter.mode_deltas,
    }
}

/// Standard CDEF of `header`.
fn fill_cdef(header: &FrameHeader) -> StdVideoAV1CDEF {
    let cdef = header.cdef();
    StdVideoAV1CDEF {
        cdef_damping_minus_3: cdef.damping_minus_3,
        cdef_bits: cdef.bits,
        cdef_y_pri_strength: cdef.y_pri,
        cdef_y_sec_strength: cdef.y_sec,
        cdef_uv_pri_strength: cdef.uv_pri,
        cdef_uv_sec_strength: cdef.uv_sec,
    }
}

/// Standard loop restoration of `header`.
fn fill_restoration(header: &FrameHeader) -> StdVideoAV1LoopRestoration {
    const REMAP: [u32; 4] = [0, 3, 1, 2];
    let restoration = header.restoration();
    let shift = u16::from(restoration.unit_shift);
    let uv_shift = u16::from(restoration.uv_shift);
    StdVideoAV1LoopRestoration {
        FrameRestorationType: [
            REMAP[usize::from(restoration.lr_type[0])],
            REMAP[usize::from(restoration.lr_type[1])],
            REMAP[usize::from(restoration.lr_type[2])],
        ],
        LoopRestorationSize: [1 + shift, 1 + shift - uv_shift, 1 + shift - uv_shift],
    }
}

/// Standard global motion of `header`.
fn fill_global(header: &FrameHeader) -> StdVideoAV1GlobalMotion {
    StdVideoAV1GlobalMotion {
        GmType: *header.gm_type(),
        gm_params: *header.gm_params(),
    }
}

/// Standard film grain of `header`.
fn fill_grain(header: &FrameHeader) -> StdVideoAV1FilmGrain {
    let grain = header.grain();
    let mut flags = StdVideoAV1FilmGrainFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_chroma_scaling_from_luma(u32::from(grain.chroma_scaling_from_luma));
    flags.set_overlap_flag(u32::from(grain.clip.overlap_flag));
    flags.set_clip_to_restricted_range(u32::from(grain.clip.clip_to_restricted_range));
    flags.set_update_grain(u32::from(grain.update_grain));
    let luma = signed(&grain.ar_coeffs_y);
    let chroma_u = signed(&grain.ar_coeffs_cb);
    let chroma_v = signed(&grain.ar_coeffs_cr);
    StdVideoAV1FilmGrain {
        flags,
        grain_scaling_minus_8: grain.grain_scaling_minus_8,
        ar_coeff_lag: grain.ar_coeff_lag,
        ar_coeff_shift_minus_6: grain.ar_coeff_shift_minus_6,
        grain_scale_shift: grain.grain_scale_shift,
        grain_seed: grain.grain_seed,
        film_grain_params_ref_idx: grain.ref_idx,
        num_y_points: grain.num_y_points,
        point_y_value: grain.point_y_value,
        point_y_scaling: grain.point_y_scaling,
        num_cb_points: grain.num_cb_points,
        point_cb_value: grain.point_cb_value,
        point_cb_scaling: grain.point_cb_scaling,
        num_cr_points: grain.num_cr_points,
        point_cr_value: grain.point_cr_value,
        point_cr_scaling: grain.point_cr_scaling,
        ar_coeffs_y_plus_128: luma,
        ar_coeffs_cb_plus_128: chroma_u,
        ar_coeffs_cr_plus_128: chroma_v,
        cb_mult: grain.cb_mult,
        cb_luma_mult: grain.cb_luma_mult,
        cb_offset: grain.cb_offset,
        cr_mult: grain.cr_mult,
        cr_luma_mult: grain.cr_luma_mult,
        cr_offset: grain.cr_offset,
    }
}

/// Standard reference info of one stored frame.
pub(crate) fn fill_reference(
    order_hint: u8,
    frame_type: u8,
    sign_bias: u8,
    saved_hints: [u8; 8],
    disable_cdf_end: bool,
    segmentation_enabled: bool,
) -> StdVideoDecodeAV1ReferenceInfo {
    let mut flags = StdVideoDecodeAV1ReferenceInfoFlags {
        _bitfield_align_1: [],
        _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
    };
    flags.set_disable_frame_end_update_cdf(u32::from(disable_cdf_end));
    flags.set_segmentation_enabled(u32::from(segmentation_enabled));
    StdVideoDecodeAV1ReferenceInfo {
        flags,
        frame_type,
        RefFrameSignBias: sign_bias,
        OrderHint: order_hint,
        SavedOrderHints: saved_hints,
    }
}

/// Expected frame id of reference `index` behind `seq`.
pub(crate) fn expected_frame_id(seq: &SequenceHeader, header: &FrameHeader, index: usize) -> u32 {
    if !seq.frame_id_numbers_present() {
        return 0;
    }
    let width = u32::from(seq.additional_frame_id_length_minus_1())
        + u32::from(seq.delta_frame_id_length_minus_2())
        + 3;
    let delta = header.refs().delta_frame_id_minus1[index] + 1;
    header
        .current_frame_id()
        .wrapping_add(1 << width)
        .wrapping_sub(delta)
        % (1 << width)
}

#[cfg(test)]
mod tests {
    use super::{expected_frame_id, fill_grain, fill_reference, fill_restoration};

    #[test]
    fn reference_names_order_and_bias() {
        let info = fill_reference(9, 1, 0b0010_0100, [1; 8], true, false);
        assert_eq!(info.OrderHint, 9);
        assert_eq!(info.frame_type, 1);
        assert_eq!(info.RefFrameSignBias, 0b0010_0100);
        assert_eq!(info.SavedOrderHints, [1; 8]);
    }

    #[test]
    fn restoration_remaps_types_and_sizes() {
        let header = crate::parse::av1::FrameHeader::default();
        let restoration = fill_restoration(&header);
        assert_eq!(restoration.FrameRestorationType, [0, 0, 0]);
        assert_eq!(restoration.LoopRestorationSize, [1, 1, 1]);
    }

    #[test]
    fn grain_defaults_apply_nothing() {
        let header = crate::parse::av1::FrameHeader::default();
        let grain = fill_grain(&header);
        assert_eq!(grain.grain_seed, 0);
        assert_eq!(grain.num_y_points, 0);
        assert_eq!(grain.ar_coeffs_y_plus_128, [0; 24]);
    }

    #[test]
    fn expected_ids_wrap_on_frame_id_width() {
        let seq = crate::parse::av1::SequenceHeader::test_with_frame_ids(4, 2);
        let header = crate::parse::av1::FrameHeader::test_with_ids(7, [2, 0, 0, 0, 0, 0, 0]);
        assert_eq!(expected_frame_id(&seq, &header, 0), 4);
        assert_eq!(expected_frame_id(&seq, &header, 1), 6);
    }
}
