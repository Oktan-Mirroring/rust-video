//! H.265 Annex B ingest: parameter sets plus slice headers.
//!
//! Frame-only progressive pictures for single-picture submissions.

use crate::bits::{rbsp, BitReader};
use crate::error::D3d12Error;
use crate::h264::Crop;

/// Sequence set count, H.265 7.4.3.2.1.
pub(crate) const MAX_SPS: usize = 16;

/// Picture set count, H.265 7.4.3.3.1.
pub(crate) const MAX_PPS: usize = 64;

/// IDR slice types, H.265 Table 7-1.
const IDR_TYPES: &[u8] = &[19, 20];

/// NAL units in one Annex B packet, start codes stripped.
pub(crate) fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = 0;
    let mut zeros = 0;
    let mut index = 0;
    while index < data.len() {
        if data[index] == 1 && zeros >= 2 {
            let end = index - zeros;
            if end > start {
                nals.push(&data[start..end]);
            }
            start = index + 1;
            zeros = 0;
        } else if data[index] == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        index += 1;
    }
    if start < data.len() {
        nals.push(&data[start..]);
    }
    nals
}

/// Parsed sequence parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sps {
    /// Set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Coded width in luma samples.
    pub(crate) width: u32,
    /// Coded height in luma samples.
    pub(crate) height: u32,
    /// Visible rectangle, absent when unconformant.
    pub(crate) crop: Option<Crop>,
    /// POC LSB width minus 4.
    pub(crate) log2_max_poc_lsb_minus4: u8,
    /// Maximum buffering minus one.
    pub(crate) max_dec_pic_buffering_minus1: u8,
    /// Minimum luma block width minus 3.
    pub(crate) log2_min_cb_minus3: u8,
    /// Luma block size range.
    pub(crate) log2_diff_max_min_cb: u8,
    /// Minimum transform width minus 2.
    pub(crate) log2_min_tb_minus2: u8,
    /// Transform size range.
    pub(crate) log2_diff_max_min_tb: u8,
    /// Inter transform depth.
    pub(crate) max_depth_inter: u8,
    /// Intra transform depth.
    pub(crate) max_depth_intra: u8,
    /// Packed tool flags.
    pub(crate) flags: u8,
}

/// Asymmetric motion partitions in the sequence flags.
const FLAG_AMP: u8 = 1;
/// Sample adaptive offset in the sequence flags.
const FLAG_SAO: u8 = 1 << 1;
/// Temporal motion vectors in the sequence flags.
const FLAG_TEMPORAL_MVP: u8 = 1 << 2;
/// Strong intra smoothing in the sequence flags.
const FLAG_STRONG_INTRA: u8 = 1 << 3;

impl Sps {
    /// Coded width in luma samples.
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    /// Coded height in luma samples.
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    /// Visible width in luma samples.
    pub(crate) fn visible_width(&self) -> u32 {
        self.crop
            .map_or_else(|| self.width, |crop| self.width - crop.left - crop.right)
    }

    /// Visible height in luma samples.
    pub(crate) fn visible_height(&self) -> u32 {
        self.crop
            .map_or_else(|| self.height, |crop| self.height - crop.top - crop.bottom)
    }

    /// Minimum luma block edge in samples.
    pub(crate) fn min_cb(&self) -> u32 {
        1 << (self.log2_min_cb_minus3 + 3)
    }

    /// True for asymmetric motion partitions.
    pub(crate) const fn amp(&self) -> bool {
        self.flags & FLAG_AMP != 0
    }

    /// True for sample adaptive offset.
    pub(crate) const fn sao(&self) -> bool {
        self.flags & FLAG_SAO != 0
    }

    /// True for temporal motion vector prediction.
    pub(crate) const fn temporal_mvp(&self) -> bool {
        self.flags & FLAG_TEMPORAL_MVP != 0
    }

    /// True for strong intra smoothing.
    pub(crate) const fn strong_intra_smoothing(&self) -> bool {
        self.flags & FLAG_STRONG_INTRA != 0
    }
}

/// Sequence parameter set from RBSP `rbsp`.
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d12Error> {
    let mut reader = BitReader::new(rbsp);
    reader.read_bits(4, "sps_video_parameter_set_id")?;
    if reader.read_bits(3, "sps_max_sub_layers_minus1")? != 0 {
        return Err(D3d12Error::Unsupported("sublayers".to_owned()));
    }
    reader.read_bit("sps_temporal_id_nesting_flag")?;
    parse_profile_tier(&mut reader)?;
    let sps_id = parse_id(&mut reader, "seq_parameter_set_id", MAX_SPS)?;
    if reader.read_ue("chroma_format_idc")? != 1 {
        return Err(D3d12Error::Unsupported("chroma outside 4:2:0".to_owned()));
    }
    let width = reader.read_ue("pic_width_in_luma_samples")?;
    let height = reader.read_ue("pic_height_in_luma_samples")?;
    if width == 0 || height == 0 {
        return Err(D3d12Error::Parse("empty coded size".to_owned()));
    }
    let crop = parse_window(&mut reader, width, height)?;
    if reader.read_ue("bit_depth_luma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported("luma depth above 8 bit".to_owned()));
    }
    if reader.read_ue("bit_depth_chroma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported(
            "chroma depth above 8 bit".to_owned(),
        ));
    }
    let log2_max_poc_lsb_minus4 = parse_small(&mut reader, "log2_max_pic_order_cnt_lsb")?;
    reader.read_bit("sps_sub_layer_ordering_info_present_flag")?;
    let max_dec_pic_buffering_minus1 =
        parse_small(&mut reader, "sps_max_dec_pic_buffering_minus1")?;
    reader.read_ue("sps_max_num_reorder_pics")?;
    reader.read_ue("sps_max_latency_increase_plus1")?;
    let coding_block_min = parse_small(&mut reader, "log2_min_luma_coding_block_size")?;
    let coding_block_range = parse_small(&mut reader, "log2_diff_max_min_luma_coding_block")?;
    let transform_min = parse_small(&mut reader, "log2_min_luma_transform_block_size")?;
    let transform_range = parse_small(&mut reader, "log2_diff_max_min_luma_transform")?;
    let max_depth_inter = parse_small(&mut reader, "max_transform_hierarchy_depth_inter")?;
    let max_depth_intra = parse_small(&mut reader, "max_transform_hierarchy_depth_intra")?;
    if reader.read_bit("scaling_list_enabled_flag")? {
        return Err(D3d12Error::Unsupported("scaling lists pending".to_owned()));
    }
    let mut flags = 0;
    set_flag(&mut flags, FLAG_AMP, reader.read_bit("amp_enabled_flag")?);
    set_flag(
        &mut flags,
        FLAG_SAO,
        reader.read_bit("sample_adaptive_offset_enabled_flag")?,
    );
    if reader.read_bit("pcm_enabled_flag")? {
        return Err(D3d12Error::Unsupported("pcm pending".to_owned()));
    }
    if reader.read_ue("num_short_term_ref_pic_sets")? != 0 {
        return Err(D3d12Error::Unsupported("reference sets pending".to_owned()));
    }
    if reader.read_bit("long_term_ref_pics_present_flag")? {
        return Err(D3d12Error::Unsupported("long-term refs pending".to_owned()));
    }
    set_flag(
        &mut flags,
        FLAG_TEMPORAL_MVP,
        reader.read_bit("sps_temporal_mvp_enabled_flag")?,
    );
    set_flag(
        &mut flags,
        FLAG_STRONG_INTRA,
        reader.read_bit("strong_intra_smoothing_enabled_flag")?,
    );
    if reader.read_bit("vui_parameters_present_flag")? {
        return Err(D3d12Error::Unsupported("vui pending".to_owned()));
    }
    if reader.read_bit("sps_extension_present_flag")? {
        return Err(D3d12Error::Unsupported("sps extension pending".to_owned()));
    }
    Ok(Sps {
        seq_parameter_set_id: sps_id,
        width,
        height,
        crop,
        log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1,
        log2_min_cb_minus3: coding_block_min,
        log2_diff_max_min_cb: coding_block_range,
        log2_min_tb_minus2: transform_min,
        log2_diff_max_min_tb: transform_range,
        max_depth_inter,
        max_depth_intra,
        flags,
    })
}

/// Flag bit set in `flags` when `set`.
fn set_flag(flags: &mut u8, bit: u8, set: bool) {
    if set {
        *flags |= bit;
    }
}

/// General profile, tier, and level.
fn parse_profile_tier(reader: &mut BitReader) -> Result<(), D3d12Error> {
    reader.read_bits(2, "general_profile_space")?;
    reader.read_bit("general_tier_flag")?;
    let profile_idc = reader.read_bits(5, "general_profile_idc")?;
    let compat = reader.read_bits(32, "general_profile_compatibility_flags")?;
    let progressive = reader.read_bit("general_progressive_source_flag")?;
    let interlaced = reader.read_bit("general_interlaced_source_flag")?;
    reader.read_bit("general_non_packed_constraint_flag")?;
    reader.read_bit("general_frame_only_constraint_flag")?;
    reader.read_bits(32, "general_reserved_zero_bits")?;
    reader.read_bits(11, "general_reserved_zero_bits")?;
    reader.read_bit("general_reserved_zero_bit")?;
    if profile_idc != 1 && compat & 0x4000_0000 == 0 {
        return Err(D3d12Error::Unsupported("profile outside main".to_owned()));
    }
    if interlaced && !progressive {
        return Err(D3d12Error::Unsupported("interlaced".to_owned()));
    }
    let level = reader.read_bits(8, "general_level_idc")?;
    if !matches!(
        level,
        30 | 60 | 63 | 90 | 93 | 120 | 123 | 150 | 153 | 156 | 180 | 183 | 186 | 210
    ) {
        return Err(D3d12Error::Parse("general_level_idc range".to_owned()));
    }
    Ok(())
}

/// Set id below `max`.
fn parse_id(reader: &mut BitReader, syntax: &'static str, max: usize) -> Result<u8, D3d12Error> {
    let id = reader.read_ue(syntax)?;
    let id = u8::try_from(id).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))?;
    if usize::from(id) >= max {
        return Err(D3d12Error::Parse(format!("{syntax} range")));
    }
    Ok(id)
}

/// Small unsigned field below 32.
fn parse_small(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if value > 31 {
        return Err(D3d12Error::Parse(format!("{syntax} range")));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// Conformance window in luma samples, validated against the coded size.
fn parse_window(
    reader: &mut BitReader,
    width: u32,
    height: u32,
) -> Result<Option<Crop>, D3d12Error> {
    if !reader.read_bit("conformance_window_flag")? {
        return Ok(None);
    }
    let left = reader.read_ue("conf_win_left_offset")?;
    let right = reader.read_ue("conf_win_right_offset")?;
    let top = reader.read_ue("conf_win_top_offset")?;
    let bottom = reader.read_ue("conf_win_bottom_offset")?;
    let horizontal = left.checked_add(right).and_then(|sum| sum.checked_mul(2));
    let vertical = top.checked_add(bottom).and_then(|sum| sum.checked_mul(2));
    match (horizontal, vertical) {
        (Some(consumed), Some(rows)) if consumed < width && rows < height => Ok(Some(Crop {
            left: left * 2,
            right: right * 2,
            top: top * 2,
            bottom: bottom * 2,
        })),
        _ => Err(D3d12Error::Parse("conf_win outside coded size".to_owned())),
    }
}

/// Parsed picture parameter set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pps {
    /// Set id.
    pub(crate) pic_parameter_set_id: u8,
    /// Sequence set id.
    pub(crate) seq_parameter_set_id: u8,
    /// Packed flags.
    pub(crate) flags: u32,
    /// Reserved slice header bits.
    pub(crate) num_extra_slice_header_bits: u8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_l1: u8,
    /// Luma QP init delta.
    pub(crate) init_qp_minus26: i8,
    /// CU QP delta depth.
    pub(crate) diff_cu_qp_delta_depth: u8,
    /// Cb QP offset.
    pub(crate) cb_qp_offset: i8,
    /// Cr QP offset.
    pub(crate) cr_qp_offset: i8,
    /// Tile columns minus one.
    pub(crate) tile_columns_minus1: u8,
    /// Tile rows minus one.
    pub(crate) tile_rows_minus1: u8,
    /// Tile column widths minus one.
    pub(crate) column_width_minus1: [u16; 19],
    /// Tile row heights minus one.
    pub(crate) row_height_minus1: [u16; 21],
    /// Beta deblocking offset.
    pub(crate) beta_offset_div2: i8,
    /// Tc deblocking offset.
    pub(crate) tc_offset_div2: i8,
    /// Parallel merge level minus 2.
    pub(crate) log2_parallel_merge_level_minus2: u8,
}

/// Dependent slices in the picture flags.
const P_DEPENDENT_SLICES: u32 = 1;
/// Output flag in the picture flags.
const P_OUTPUT_FLAG: u32 = 1 << 1;
/// Sign data hiding in the picture flags.
const P_SIGN_HIDING: u32 = 1 << 2;
/// CABAC init in the picture flags.
const P_CABAC_INIT: u32 = 1 << 3;
/// Constrained intra prediction in the picture flags.
const P_CONSTRAINED_INTRA: u32 = 1 << 4;
/// Transform skip in the picture flags.
const P_TRANSFORM_SKIP: u32 = 1 << 5;
/// CU QP deltas in the picture flags.
const P_CU_QP_DELTA: u32 = 1 << 6;
/// Slice chroma QP offsets in the picture flags.
const P_CHROMA_QP_OFFSETS: u32 = 1 << 7;
/// Weighted prediction in the picture flags.
const P_WEIGHTED_PRED: u32 = 1 << 8;
/// Weighted biprediction in the picture flags.
const P_WEIGHTED_BIPRED: u32 = 1 << 9;
/// Transform bypass in the picture flags.
const P_TRANSQUANT_BYPASS: u32 = 1 << 10;
/// Tiles in the picture flags.
const P_TILES: u32 = 1 << 11;
/// Uniform tile spacing in the picture flags.
const P_UNIFORM_SPACING: u32 = 1 << 12;
/// Entropy sync in the picture flags.
const P_ENTROPY_SYNC: u32 = 1 << 13;
/// Loop filter across tiles in the picture flags.
const P_LOOP_TILES: u32 = 1 << 14;
/// Loop filter across slices in the picture flags.
const P_LOOP_SLICES: u32 = 1 << 15;
/// Deblocking override in the picture flags.
const P_DEBLOCK_OVERRIDE: u32 = 1 << 16;
/// Disabled deblocking in the picture flags.
const P_DEBLOCK_DISABLED: u32 = 1 << 17;
/// Reference list modification in the picture flags.
const P_LISTS_MOD: u32 = 1 << 18;
/// Slice header extension in the picture flags.
const P_SLICE_EXT: u32 = 1 << 19;

impl Pps {
    /// True for dependent slice segments.
    pub(crate) const fn dependent_slices(&self) -> bool {
        self.flags & P_DEPENDENT_SLICES != 0
    }

    /// True for the output flag in slices.
    pub(crate) const fn output_flag_present(&self) -> bool {
        self.flags & P_OUTPUT_FLAG != 0
    }

    /// True for sign data hiding.
    pub(crate) const fn sign_data_hiding(&self) -> bool {
        self.flags & P_SIGN_HIDING != 0
    }

    /// True for CABAC init in slices.
    pub(crate) const fn cabac_init_present(&self) -> bool {
        self.flags & P_CABAC_INIT != 0
    }

    /// True for constrained intra prediction.
    pub(crate) const fn constrained_intra(&self) -> bool {
        self.flags & P_CONSTRAINED_INTRA != 0
    }

    /// True for transform skip.
    pub(crate) const fn transform_skip(&self) -> bool {
        self.flags & P_TRANSFORM_SKIP != 0
    }

    /// True for CU QP deltas.
    pub(crate) const fn cu_qp_delta(&self) -> bool {
        self.flags & P_CU_QP_DELTA != 0
    }

    /// True for slice chroma QP offsets.
    pub(crate) const fn slice_chroma_qp_offsets_present(&self) -> bool {
        self.flags & P_CHROMA_QP_OFFSETS != 0
    }

    /// True for weighted prediction.
    pub(crate) const fn weighted_pred(&self) -> bool {
        self.flags & P_WEIGHTED_PRED != 0
    }

    /// True for weighted biprediction.
    pub(crate) const fn weighted_bipred(&self) -> bool {
        self.flags & P_WEIGHTED_BIPRED != 0
    }

    /// True for transform bypass.
    pub(crate) const fn transquant_bypass(&self) -> bool {
        self.flags & P_TRANSQUANT_BYPASS != 0
    }

    /// True for tiles.
    pub(crate) const fn tiles(&self) -> bool {
        self.flags & P_TILES != 0
    }

    /// True for uniform tile spacing.
    pub(crate) const fn uniform_spacing(&self) -> bool {
        self.flags & P_UNIFORM_SPACING != 0
    }

    /// True for entropy sync.
    pub(crate) const fn entropy_sync(&self) -> bool {
        self.flags & P_ENTROPY_SYNC != 0
    }

    /// True for the loop filter across tiles.
    pub(crate) const fn loop_across_tiles(&self) -> bool {
        self.flags & P_LOOP_TILES != 0
    }

    /// True for the loop filter across slices.
    pub(crate) const fn loop_across_slices(&self) -> bool {
        self.flags & P_LOOP_SLICES != 0
    }

    /// True for deblocking override.
    pub(crate) const fn deblocking_override(&self) -> bool {
        self.flags & P_DEBLOCK_OVERRIDE != 0
    }

    /// True for disabled deblocking.
    pub(crate) const fn deblocking_disabled(&self) -> bool {
        self.flags & P_DEBLOCK_DISABLED != 0
    }

    /// True for reference list modification.
    pub(crate) const fn lists_modification_present(&self) -> bool {
        self.flags & P_LISTS_MOD != 0
    }

    /// True for slice header extension.
    pub(crate) const fn slice_ext_present(&self) -> bool {
        self.flags & P_SLICE_EXT != 0
    }
}

/// Picture set ids, flags, and scalars ahead of the tile section.
struct PpsHeader {
    /// Set id.
    pps_id: u8,
    /// Sequence set id.
    sps_id: u8,
    /// Packed flags.
    flags: u32,
    /// Reserved slice header bits.
    num_extra_slice_header_bits: u8,
    /// Default L0 reference count minus one.
    num_ref_l0: u8,
    /// Default L1 reference count minus one.
    num_ref_l1: u8,
    /// Luma QP init delta.
    init_qp_minus26: i8,
    /// CU QP delta depth.
    diff_cu_qp_delta_depth: u8,
    /// Cb QP offset.
    cb_qp_offset: i8,
    /// Cr QP offset.
    cr_qp_offset: i8,
}

/// Picture set ids, flags, and scalars ahead of the tile section.
fn parse_pps_header(reader: &mut BitReader, sps: &Sps) -> Result<PpsHeader, D3d12Error> {
    let pps_id = parse_id(reader, "pic_parameter_set_id", MAX_PPS)?;
    let raw_sps = reader.read_ue("pps_seq_parameter_set_id")?;
    let sps_id = u8::try_from(raw_sps)
        .map_err(|_| D3d12Error::Parse("pps_seq_parameter_set_id range".to_owned()))?;
    if sps_id != sps.seq_parameter_set_id {
        return Err(D3d12Error::Parse("pps cites unknown sps".to_owned()));
    }
    let mut flags = 0;
    set_wide_flag(
        &mut flags,
        P_DEPENDENT_SLICES,
        reader.read_bit("dependent_slice_segments_enabled_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_OUTPUT_FLAG,
        reader.read_bit("output_flag_present_flag")?,
    );
    let num_extra_slice_header_bits =
        u8::try_from(reader.read_bits(3, "num_extra_slice_header_bits")?)
            .map_err(|_| D3d12Error::Parse("num_extra_slice_header_bits range".to_owned()))?;
    set_wide_flag(
        &mut flags,
        P_SIGN_HIDING,
        reader.read_bit("sign_data_hiding_enabled_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_CABAC_INIT,
        reader.read_bit("cabac_init_present_flag")?,
    );
    let num_ref_l0 = parse_small(&mut *reader, "num_ref_idx_l0_default_active_minus1")?;
    let num_ref_l1 = parse_small(&mut *reader, "num_ref_idx_l1_default_active_minus1")?;
    let init_qp_minus26 = parse_qp(&mut *reader, "init_qp_minus26")?;
    set_wide_flag(
        &mut flags,
        P_CONSTRAINED_INTRA,
        reader.read_bit("constrained_intra_pred_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_TRANSFORM_SKIP,
        reader.read_bit("transform_skip_enabled_flag")?,
    );
    let cu_qp_delta = reader.read_bit("cu_qp_delta_enabled_flag")?;
    set_wide_flag(&mut flags, P_CU_QP_DELTA, cu_qp_delta);
    let diff_cu_qp_delta_depth = if cu_qp_delta {
        parse_small(&mut *reader, "diff_cu_qp_delta_depth")?
    } else {
        0
    };
    let cb_qp_offset = parse_qp(&mut *reader, "pps_cb_qp_offset")?;
    let cr_qp_offset = parse_qp(&mut *reader, "pps_cr_qp_offset")?;
    set_wide_flag(
        &mut flags,
        P_CHROMA_QP_OFFSETS,
        reader.read_bit("pps_slice_chroma_qp_offsets_present_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_WEIGHTED_PRED,
        reader.read_bit("weighted_pred_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_WEIGHTED_BIPRED,
        reader.read_bit("weighted_bipred_flag")?,
    );
    set_wide_flag(
        &mut flags,
        P_TRANSQUANT_BYPASS,
        reader.read_bit("transquant_bypass_enabled_flag")?,
    );
    Ok(PpsHeader {
        pps_id,
        sps_id,
        flags,
        num_extra_slice_header_bits,
        num_ref_l0,
        num_ref_l1,
        init_qp_minus26,
        diff_cu_qp_delta_depth,
        cb_qp_offset,
        cr_qp_offset,
    })
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d12Error> {
    let mut reader = BitReader::new(rbsp);
    let header = parse_pps_header(&mut reader, sps)?;
    let mut pps = Pps {
        pic_parameter_set_id: header.pps_id,
        seq_parameter_set_id: header.sps_id,
        flags: header.flags,
        num_extra_slice_header_bits: header.num_extra_slice_header_bits,
        num_ref_l0: header.num_ref_l0,
        num_ref_l1: header.num_ref_l1,
        init_qp_minus26: header.init_qp_minus26,
        diff_cu_qp_delta_depth: header.diff_cu_qp_delta_depth,
        cb_qp_offset: header.cb_qp_offset,
        cr_qp_offset: header.cr_qp_offset,
        tile_columns_minus1: 0,
        tile_rows_minus1: 0,
        column_width_minus1: [0; 19],
        row_height_minus1: [0; 21],
        beta_offset_div2: 0,
        tc_offset_div2: 0,
        log2_parallel_merge_level_minus2: 0,
    };
    parse_pps_tiles(&mut reader, sps, &mut pps)?;
    parse_pps_deblocking(&mut reader, &mut pps)?;
    if reader.read_bit("pps_scaling_list_data_present_flag")? {
        return Err(D3d12Error::Unsupported("scaling lists pending".to_owned()));
    }
    set_wide_flag(
        &mut pps.flags,
        P_LISTS_MOD,
        reader.read_bit("lists_modification_present_flag")?,
    );
    let merge_level = parse_small(&mut reader, "log2_parallel_merge_level_minus2")?;
    let ctb_log2 = sps
        .log2_min_cb_minus3
        .saturating_add(3)
        .saturating_add(sps.log2_diff_max_min_cb);
    if merge_level > ctb_log2.saturating_sub(2) {
        return Err(D3d12Error::Parse(
            "log2_parallel_merge_level range".to_owned(),
        ));
    }
    pps.log2_parallel_merge_level_minus2 = merge_level;
    set_wide_flag(
        &mut pps.flags,
        P_SLICE_EXT,
        reader.read_bit("slice_segment_header_extension_present_flag")?,
    );
    if reader.read_bit("pps_extension_present_flag")? {
        return Err(D3d12Error::Unsupported("pps extension pending".to_owned()));
    }
    Ok(pps)
}

/// QP delta field.
fn parse_qp(reader: &mut BitReader, syntax: &'static str) -> Result<i8, D3d12Error> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// Tile layout section of a picture set.
fn parse_pps_tiles(reader: &mut BitReader, sps: &Sps, pps: &mut Pps) -> Result<(), D3d12Error> {
    set_wide_flag(
        &mut pps.flags,
        P_TILES,
        reader.read_bit("tiles_enabled_flag")?,
    );
    set_wide_flag(
        &mut pps.flags,
        P_ENTROPY_SYNC,
        reader.read_bit("entropy_coding_sync_enabled_flag")?,
    );
    if !pps.tiles() {
        set_wide_flag(
            &mut pps.flags,
            P_LOOP_SLICES,
            reader.read_bit("pps_loop_filter_across_slices_enabled_flag")?,
        );
        return Ok(());
    }
    pps.tile_columns_minus1 = parse_small(&mut *reader, "num_tile_columns_minus1")?;
    pps.tile_rows_minus1 = parse_small(&mut *reader, "num_tile_rows_minus1")?;
    if pps.tile_columns_minus1 >= 19 || pps.tile_rows_minus1 >= 21 {
        return Err(D3d12Error::Unsupported("tile grid pending".to_owned()));
    }
    set_wide_flag(
        &mut pps.flags,
        P_UNIFORM_SPACING,
        reader.read_bit("uniform_spacing_flag")?,
    );
    if pps.uniform_spacing() {
        uniform_tiles(sps, pps)?;
    } else {
        for index in 0..pps.tile_columns_minus1 {
            pps.column_width_minus1[usize::from(index)] =
                parse_tile_extent(reader, "column_width_minus1")?;
        }
        for index in 0..pps.tile_rows_minus1 {
            pps.row_height_minus1[usize::from(index)] =
                parse_tile_extent(reader, "row_height_minus1")?;
        }
    }
    set_wide_flag(
        &mut pps.flags,
        P_LOOP_TILES,
        reader.read_bit("loop_filter_across_tiles_enabled_flag")?,
    );
    set_wide_flag(
        &mut pps.flags,
        P_LOOP_SLICES,
        reader.read_bit("pps_loop_filter_across_slices_enabled_flag")?,
    );
    Ok(())
}

/// Wide flag bit set in `flags` when `set`.
fn set_wide_flag(flags: &mut u32, bit: u32, set: bool) {
    if set {
        *flags |= bit;
    }
}

/// Tile extent below the coding tree grid.
fn parse_tile_extent(reader: &mut BitReader, syntax: &'static str) -> Result<u16, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    u16::try_from(value).map_err(|_| D3d12Error::Parse(format!("{syntax} range")))
}

/// Uniform tile widths and heights from the coding tree grid.
fn uniform_tiles(sps: &Sps, pps: &mut Pps) -> Result<(), D3d12Error> {
    let ctb = sps.min_cb() << sps.log2_diff_max_min_cb;
    let ctbs_w = sps.width.div_ceil(ctb).max(1);
    let ctbs_h = sps.height.div_ceil(ctb).max(1);
    let columns = u32::from(pps.tile_columns_minus1) + 1;
    let rows = u32::from(pps.tile_rows_minus1) + 1;
    for (index, slot) in pps.column_width_minus1.iter_mut().enumerate() {
        let i =
            u32::try_from(index).map_err(|_| D3d12Error::Parse("tile column range".to_owned()))?;
        if i >= columns {
            break;
        }
        let width = (i + 1) * ctbs_w / columns - i * ctbs_w / columns;
        *slot = u16::try_from(width.saturating_sub(1))
            .map_err(|_| D3d12Error::Parse("tile column range".to_owned()))?;
    }
    for (index, slot) in pps.row_height_minus1.iter_mut().enumerate() {
        let i = u32::try_from(index).map_err(|_| D3d12Error::Parse("tile row range".to_owned()))?;
        if i >= rows {
            break;
        }
        let height = (i + 1) * ctbs_h / rows - i * ctbs_h / rows;
        *slot = u16::try_from(height.saturating_sub(1))
            .map_err(|_| D3d12Error::Parse("tile row range".to_owned()))?;
    }
    Ok(())
}

/// Deblocking section of a picture set.
fn parse_pps_deblocking(reader: &mut BitReader, pps: &mut Pps) -> Result<(), D3d12Error> {
    if !reader.read_bit("deblocking_filter_control_present_flag")? {
        return Ok(());
    }
    set_wide_flag(
        &mut pps.flags,
        P_DEBLOCK_OVERRIDE,
        reader.read_bit("deblocking_filter_override_enabled_flag")?,
    );
    set_wide_flag(
        &mut pps.flags,
        P_DEBLOCK_DISABLED,
        reader.read_bit("pps_deblocking_filter_disabled_flag")?,
    );
    if pps.deblocking_disabled() {
        return Ok(());
    }
    pps.beta_offset_div2 = parse_qp(&mut *reader, "pps_beta_offset_div2")?;
    pps.tc_offset_div2 = parse_qp(&mut *reader, "pps_tc_offset_div2")?;
    Ok(())
}

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceType {
    /// Bi-predictive.
    B,
    /// Predictive.
    P,
    /// Intra.
    I,
}

impl SliceType {
    /// Type for the `slice_type` value.
    fn new(value: u32) -> Result<Self, D3d12Error> {
        match value {
            0 => Ok(Self::B),
            1 => Ok(Self::P),
            2 => Ok(Self::I),
            _ => Err(D3d12Error::Parse("slice_type range".to_owned())),
        }
    }

    /// True for intra slices.
    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::I)
    }
}

/// Parsed slice segment header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SliceHeader {
    /// Picture set id.
    pub(crate) pps_id: u8,
    /// Slice coding type.
    pub(crate) slice_type: SliceType,
    /// Address of the first coding tree block.
    pub(crate) address: u32,
    /// First slice of the picture.
    pub(crate) first: bool,
}

/// Slice header against its parameter set.
fn parse_slice(
    reader: &mut BitReader,
    idr: bool,
    pps: &Pps,
    sao: bool,
) -> Result<SliceHeader, D3d12Error> {
    let first = reader.read_bit("first_slice_segment_in_pic_flag")?;
    if idr {
        reader.read_bit("no_output_of_prior_pics_flag")?;
    }
    let raw_pps = reader.read_ue("slice_pic_parameter_set_id")?;
    let pps_id = u8::try_from(raw_pps)
        .map_err(|_| D3d12Error::Parse("slice_pic_parameter_set_id range".to_owned()))?;
    let address = if first {
        0
    } else {
        if pps.dependent_slices() && reader.read_bit("dependent_slice_segment_flag")? {
            return Err(D3d12Error::Unsupported(
                "dependent slices pending".to_owned(),
            ));
        }
        reader.read_ue("slice_segment_address")?
    };
    for _ in 0..pps.num_extra_slice_header_bits {
        reader.read_bit("slice_reserved_flag")?;
    }
    let slice_type = SliceType::new(reader.read_ue("slice_type")?)?;
    if !slice_type.is_intra() {
        return Err(D3d12Error::Unsupported(
            "inter picture needs references".to_owned(),
        ));
    }
    if !idr {
        return Err(D3d12Error::Unsupported(
            "non-IDR intra needs POC tracking".to_owned(),
        ));
    }
    if pps.output_flag_present() {
        reader.read_bit("pic_output_flag")?;
    }
    let sao_luma = sao && reader.read_bit("slice_sao_luma_flag")?;
    let sao_chroma = sao && reader.read_bit("slice_sao_chroma_flag")?;
    if pps.slice_chroma_qp_offsets_present() {
        reader.read_se("slice_cb_qp_offset")?;
        reader.read_se("slice_cr_qp_offset")?;
    }
    parse_slice_filters(reader, pps, sao_luma, sao_chroma)?;
    if pps.tiles() || pps.entropy_sync() {
        skip_entry_points(reader)?;
    }
    if pps.slice_ext_present() {
        let length = reader.read_ue("slice_segment_header_extension_length")?;
        for _ in 0..length {
            reader.read_bits(8, "slice_segment_header_extension_data_byte")?;
        }
    }
    align(reader)?;
    Ok(SliceHeader {
        pps_id,
        slice_type,
        address,
        first,
    })
}

/// Deblocking and loop filter section of a slice header.
fn parse_slice_filters(
    reader: &mut BitReader,
    pps: &Pps,
    sao_luma: bool,
    sao_chroma: bool,
) -> Result<(), D3d12Error> {
    let mut disabled = pps.deblocking_disabled();
    if pps.deblocking_override() && reader.read_bit("deblocking_filter_override_flag")? {
        disabled = reader.read_bit("slice_deblocking_filter_disabled_flag")?;
        if !disabled {
            reader.read_se("slice_beta_offset_div2")?;
            reader.read_se("slice_tc_offset_div2")?;
        }
    }
    if pps.loop_across_slices() && (sao_luma || sao_chroma || !disabled) {
        reader.read_bit("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(())
}

/// Entry point offsets, skipped past their table.
fn skip_entry_points(reader: &mut BitReader) -> Result<(), D3d12Error> {
    let count = reader.read_ue("num_entry_point_offsets")?;
    if count == 0 {
        return Ok(());
    }
    let width = reader.read_ue("offset_len_minus1")? + 1;
    if width > 32 {
        return Err(D3d12Error::Parse("offset_len range".to_owned()));
    }
    for _ in 0..count {
        reader.read_bits(width, "entry_point_offset_minus1")?;
    }
    Ok(())
}

/// Bit offset advanced to the next byte boundary.
fn align(reader: &mut BitReader) -> Result<(), D3d12Error> {
    let remainder = reader.position() % 8;
    if remainder != 0 {
        let width = u32::try_from(8 - remainder)
            .map_err(|_| D3d12Error::Parse("alignment range".to_owned()))?;
        reader.read_bits(width, "alignment")?;
    }
    Ok(())
}

/// One coded slice: header plus its NAL bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSlice {
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
    /// NAL bytes without the start code.
    pub(crate) nal: Vec<u8>,
    /// True for IDR slices.
    pub(crate) idr: bool,
}

/// One coded picture: slices sharing the set and IDR flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// Picture slices in bitstream order.
    pub(crate) slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub(crate) idr: bool,
}

impl Picture {
    /// Picture set id.
    pub(crate) fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.header.pps_id)
    }

    /// True when every slice is intra.
    pub(crate) fn intra(&self) -> bool {
        !self.slices.is_empty()
            && self
                .slices
                .iter()
                .all(|slice| slice.header.slice_type.is_intra())
    }
}

/// Annex B ingest: parameter sets plus picture assembly.
pub(crate) struct Ingest {
    /// Stored sequence sets by id.
    sps: [Option<Sps>; MAX_SPS],
    /// Stored picture sets by id.
    pps: [Option<Pps>; MAX_PPS],
    /// Slices of the open picture.
    pending: Vec<PendingSlice>,
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
        Self {
            sps: core::array::from_fn(|_| None),
            pps: core::array::from_fn(|_| None),
            pending: Vec::new(),
        }
    }

    /// Sequence set `id`, if stored.
    pub(crate) fn sps(&self, id: u8) -> Option<&Sps> {
        self.sps.get(usize::from(id))?.as_ref()
    }

    /// Picture set `id`, if stored.
    pub(crate) fn pps(&self, id: u8) -> Option<&Pps> {
        self.pps.get(usize::from(id))?.as_ref()
    }

    /// Pictures completed by `data`; trailing slices complete the picture.
    pub(crate) fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Picture>, D3d12Error> {
        let mut pictures = Vec::new();
        for nal in split_nals(data) {
            self.ingest_nal(nal, &mut pictures)?;
        }
        if let Some(picture) = self.complete_picture()? {
            pictures.push(picture);
        }
        Ok(pictures)
    }

    /// Open picture, if any slices arrived without a boundary.
    pub(crate) fn finish(&mut self) -> Result<Option<Picture>, D3d12Error> {
        self.complete_picture()
    }

    /// One NAL unit into the ingest state.
    fn ingest_nal(&mut self, nal: &[u8], pictures: &mut Vec<Picture>) -> Result<(), D3d12Error> {
        let Some(header) = nal.get(..2) else {
            return Ok(());
        };
        if header[0] >> 7 != 0 {
            return Err(D3d12Error::Parse("nal forbidden bit".to_owned()));
        }
        let nal_type = (header[0] >> 1) & 0x3F;
        let layer_id = ((u16::from(header[0]) & 1) << 5) | (u16::from(header[1]) >> 3);
        if layer_id != 0 {
            return Err(D3d12Error::Unsupported("multilayer".to_owned()));
        }
        match nal_type {
            0..=21 => self.ingest_vcl(nal, nal_type, pictures),
            33 => self.ingest_sps(nal),
            34 => self.ingest_pps(nal),
            35..=37 => {
                if let Some(picture) = self.complete_picture()? {
                    pictures.push(picture);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Coded slice into the open picture.
    fn ingest_vcl(
        &mut self,
        nal: &[u8],
        nal_type: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), D3d12Error> {
        if !(0..=21).contains(&nal_type) || (10..=15).contains(&nal_type) {
            return Err(D3d12Error::Unsupported("reserved slice type".to_owned()));
        }
        let idr = IDR_TYPES.contains(&nal_type);
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d12Error::Parse("slice without header".to_owned()));
        };
        let pps_id = peek_pps_id(body, idr)?;
        let pps = self
            .pps(pps_id)
            .ok_or_else(|| D3d12Error::Parse("slice cites unknown pps".to_owned()))?
            .clone();
        let sao = self
            .sps(pps.seq_parameter_set_id)
            .ok_or_else(|| D3d12Error::Parse("slice cites unknown sps".to_owned()))?
            .sao();
        let mut reader = BitReader::new(body);
        let parsed = parse_slice(&mut reader, idr, &pps, sao)?;
        if parsed.first && !self.pending.is_empty() {
            if let Some(picture) = self.complete_picture()? {
                pictures.push(picture);
            }
        }
        self.pending.push(PendingSlice {
            header: parsed,
            nal: nal.to_vec(),
            idr,
        });
        Ok(())
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), D3d12Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d12Error::Parse("sps without body".to_owned()));
        };
        let sps = parse_sps(body)?;
        let id = sps.seq_parameter_set_id;
        self.sps[usize::from(id)] = Some(sps);
        Ok(())
    }

    /// Picture set into the store.
    fn ingest_pps(&mut self, nal: &[u8]) -> Result<(), D3d12Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d12Error::Parse("pps without body".to_owned()));
        };
        let mut probe = BitReader::new(body);
        probe.read_ue("pic_parameter_set_id")?;
        let sps_id = probe.read_ue("seq_parameter_set_id")?;
        let sps_id = u8::try_from(sps_id)
            .map_err(|_| D3d12Error::Parse("seq_parameter_set_id range".to_owned()))?;
        let sps = self
            .sps(sps_id)
            .ok_or_else(|| D3d12Error::Parse("pps cites unknown sps".to_owned()))?
            .clone();
        let pps = parse_pps(body, &sps)?;
        let id = pps.pic_parameter_set_id;
        self.pps[usize::from(id)] = Some(pps);
        Ok(())
    }

    /// Open slices as one validated picture.
    fn complete_picture(&mut self) -> Result<Option<Picture>, D3d12Error> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let slices = core::mem::take(&mut self.pending);
        let first = &slices[0];
        if !first.header.first {
            return Err(D3d12Error::Parse("picture without first slice".to_owned()));
        }
        for slice in &slices {
            if slice.header.pps_id != first.header.pps_id || slice.idr != first.idr {
                return Err(D3d12Error::Parse("picture slices disagree".to_owned()));
            }
        }
        let mut previous = 0;
        for slice in slices.iter().skip(1) {
            if slice.header.first || slice.header.address <= previous {
                return Err(D3d12Error::Unsupported("arbitrary slice order".to_owned()));
            }
            previous = slice.header.address;
        }
        Ok(Some(Picture {
            idr: first.idr,
            slices,
        }))
    }
}

/// Picture set id behind a slice body prefix.
fn peek_pps_id(body: &[u8], idr: bool) -> Result<u8, D3d12Error> {
    let mut reader = BitReader::new(body);
    reader.read_bit("first_slice_segment_in_pic_flag")?;
    if idr {
        reader.read_bit("no_output_of_prior_pics_flag")?;
    }
    let id = reader.read_ue("slice_pic_parameter_set_id")?;
    u8::try_from(id).map_err(|_| D3d12Error::Parse("slice_pic_parameter_set_id range".to_owned()))
}

/// Synthetic inputs behind submission tests.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Test bit writer.
    struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        fn bit(&mut self, set: bool) {
            if set {
                self.current |= 1 << (7 - self.filled);
            }
            self.filled += 1;
            if self.filled == 8 {
                self.bytes.push(self.current);
                self.current = 0;
                self.filled = 0;
            }
        }

        /// Low `count` bits of `value`, most significant first.
        fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Unsigned Exp-Golomb code.
        fn ue(&mut self, value: u32) {
            let code = value + 1;
            let width = 32 - code.leading_zeros();
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code, width);
        }

        /// Signed Exp-Golomb code.
        fn se(&mut self, value: i32) {
            let code = if value > 0 {
                u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
            } else {
                u32::try_from(-value).unwrap_or(u32::MAX) * 2
            };
            self.ue(code);
        }

        /// Finished RBSP with emulation prevention inserted.
        fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.filled != 0 {
                self.bit(false);
            }
            let mut out = Vec::new();
            let mut zeros = 0;
            for byte in self.bytes {
                if zeros == 2 && byte <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                out.push(byte);
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            out
        }
    }

    /// Main-profile 64x48 sequence set body.
    pub(crate) fn sps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.bits(0, 2);
        writer.bit(false);
        writer.bits(1, 5);
        writer.bits(0x4000_0000, 32);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bits(0, 32);
        writer.bits(0, 11);
        writer.bit(false);
        writer.bits(93, 8);
        writer.ue(0);
        writer.ue(1);
        writer.ue(64);
        writer.ue(48);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(1);
        writer.ue(1);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// Picture set body over sequence set zero.
    pub(crate) fn pps_body() -> Vec<u8> {
        pps_body_merge(0)
    }

    /// Picture set body with merge level `level`.
    pub(crate) fn pps_body_merge(level: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 3);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.se(0);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.se(0);
        writer.se(0);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(level);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// IDR slice body over the test sets.
    pub(crate) fn idr_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(2);
        writer.bit(false);
        writer.bit(false);
        writer.finish()
    }

    /// P slice body over the test sets.
    pub(crate) fn p_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.finish()
    }

    /// NAL unit with two-byte header over the body.
    pub(crate) fn nal(nal_type: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![nal_type << 1, 1];
        nal.extend_from_slice(body);
        nal
    }

    /// Packet of NAL units with four-byte start codes.
    pub(crate) fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{idr_body, nal, p_body, packet, pps_body, pps_body_merge, sps_body};
    use super::{parse_pps, parse_sps, rbsp, split_nals, Ingest};

    #[test]
    fn split_nals_strips_start_codes() {
        let nals = split_nals(b"\0\0\0\x01\x42\x01abc\0\0\x01\x44\x01de");
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0][0] >> 1, 33);
    }

    #[test]
    fn sps_reports_coded_size() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        assert_eq!((sps.width, sps.height), (64, 48));
        assert_eq!(sps.visible_width(), 64);
        assert_eq!(sps.visible_height(), 48);
        assert!(sps.sao());
    }

    #[test]
    fn pps_reports_flags_and_defaults() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        let pps = parse_pps(&rbsp(&pps_body()), &sps).expect("valid pps");
        assert!(!pps.tiles());
        assert!(!pps.deblocking_override());
        assert_eq!(pps.log2_parallel_merge_level_minus2, 0);
    }

    #[test]
    fn pps_rejects_merge_level_above_ctb() {
        let sps = parse_sps(&rbsp(&sps_body())).expect("valid sps");
        assert!(parse_pps(&rbsp(&pps_body_merge(2)), &sps).is_err());
    }

    #[test]
    fn ingest_assembles_idr_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        assert!(pictures[0].intra());
    }

    #[test]
    fn ingest_rejects_inter_slices() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(1, &p_body()),
        ]);
        let mut ingest = Ingest::new();
        assert!(ingest.ingest_packet(&data).is_err());
    }

    #[test]
    fn ingest_rejects_garbage() {
        let mut ingest = Ingest::new();
        assert!(ingest
            .ingest_packet(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00])
            .is_err());
    }
}
