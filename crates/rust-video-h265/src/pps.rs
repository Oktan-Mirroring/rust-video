//! Picture parameter sets.

use crate::bits::BitReader;
use crate::error::{HevcError, HevcGate, ParseKind};
use crate::nal::MAX_PPS;
use crate::scaling::{parse_scaling_list_data, ScalingLists};
use crate::sps::{parse_bounded_ue, Sps};

/// Dependent slice segments flag bit.
pub const PPS_DEPENDENT: u32 = 1;
/// Output flag present flag bit.
pub const PPS_OUTPUT_FLAG: u32 = 1 << 1;
/// Sign data hiding flag bit.
pub const PPS_SIGN_HIDING: u32 = 1 << 2;
/// CABAC init present flag bit.
pub const PPS_CABAC_INIT: u32 = 1 << 3;
/// Constrained intra prediction flag bit.
pub const PPS_CONSTRAINED_INTRA: u32 = 1 << 4;
/// Transform skip flag bit.
pub const PPS_TRANSFORM_SKIP: u32 = 1 << 5;
/// CU QP delta flag bit.
pub const PPS_CU_QP_DELTA: u32 = 1 << 6;
/// Chroma QP offsets present flag bit.
pub const PPS_CHROMA_OFFSETS: u32 = 1 << 7;
/// Weighted prediction flag bit.
pub const PPS_WEIGHTED_PRED: u32 = 1 << 8;
/// Weighted biprediction flag bit.
pub const PPS_WEIGHTED_BIPRED: u32 = 1 << 9;
/// Transquant bypass flag bit.
pub const PPS_TRANSQUANT_BYPASS: u32 = 1 << 10;
/// Tiles enabled flag bit.
pub const PPS_TILES: u32 = 1 << 11;
/// Uniform tile spacing flag bit.
pub const PPS_UNIFORM_SPACING: u32 = 1 << 12;
/// Loop filter across tiles flag bit.
pub const PPS_LOOP_ACROSS_TILES: u32 = 1 << 13;
/// Wavefront sync flag bit.
pub const PPS_SYNC: u32 = 1 << 14;
/// Loop filter across slices flag bit.
pub const PPS_LOOP_ACROSS_SLICES: u32 = 1 << 15;
/// Deblocking override flag bit.
pub const PPS_DEBLOCK_OVERRIDE: u32 = 1 << 16;
/// Deblocking disabled flag bit.
pub const PPS_DEBLOCK_DISABLED: u32 = 1 << 17;
/// Scaling lists present flag bit.
pub const PPS_SCALING: u32 = 1 << 18;
/// Reference list modification flag bit.
pub const PPS_LISTS_MOD: u32 = 1 << 19;
/// Slice extension present flag bit.
pub const PPS_SLICE_EXT: u32 = 1 << 20;

/// Parsed picture parameter set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pps {
    /// Set id.
    pub id: u8,
    /// Sequence set id.
    pub sps_id: u8,
    /// Packed tool flags.
    pub flags: u32,
    /// Reserved slice header bit count.
    pub num_extra_slice_header_bits: u8,
    /// Default L0 reference count minus one.
    pub num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub num_ref_l1: u8,
    /// Luma QP init delta.
    pub pic_init_qp_minus26: i8,
    /// CU QP delta depth.
    pub diff_cu_qp_delta_depth: u8,
    /// Cb QP offset.
    pub cb_qp_offset: i8,
    /// Cr QP offset.
    pub cr_qp_offset: i8,
    /// Tile column count.
    pub num_tile_columns: u8,
    /// Tile row count.
    pub num_tile_rows: u8,
    /// Tile column widths in treeblocks, derived last.
    pub column_width: Vec<u16>,
    /// Tile row heights in treeblocks, derived last.
    pub row_height: Vec<u16>,
    /// Deblocking beta offset over two.
    pub beta_offset_div2: i8,
    /// Deblocking tc offset over two.
    pub tc_offset_div2: i8,
    /// Resolved picture scaling lists.
    pub scaling: ScalingLists,
    /// Parallel merge level minus 2.
    pub log2_parallel_merge_minus2: u8,
    /// Deblocking control present.
    pub deblock_control_present: bool,
    /// Picture extension present.
    pub pps_extension_present: bool,
    /// Range, multilayer, 3D, and SCC extension flags.
    pub pps_extension_flags: [bool; 4],
}

impl Pps {
    /// Dependent slice segments.
    #[must_use]
    pub const fn dependent_slices(&self) -> bool {
        self.flags & PPS_DEPENDENT != 0
    }

    /// Output flag present in slices.
    #[must_use]
    pub const fn output_flag_present(&self) -> bool {
        self.flags & PPS_OUTPUT_FLAG != 0
    }

    /// Sign data hiding.
    #[must_use]
    pub const fn sign_hiding(&self) -> bool {
        self.flags & PPS_SIGN_HIDING != 0
    }

    /// CABAC init present in slices.
    #[must_use]
    pub const fn cabac_init_present(&self) -> bool {
        self.flags & PPS_CABAC_INIT != 0
    }

    /// Constrained intra prediction.
    #[must_use]
    pub const fn constrained_intra(&self) -> bool {
        self.flags & PPS_CONSTRAINED_INTRA != 0
    }

    /// Transform skip.
    #[must_use]
    pub const fn transform_skip(&self) -> bool {
        self.flags & PPS_TRANSFORM_SKIP != 0
    }

    /// CU QP deltas.
    #[must_use]
    pub const fn cu_qp_delta(&self) -> bool {
        self.flags & PPS_CU_QP_DELTA != 0
    }

    /// Slice chroma QP offsets.
    #[must_use]
    pub const fn chroma_offsets_present(&self) -> bool {
        self.flags & PPS_CHROMA_OFFSETS != 0
    }

    /// Weighted prediction.
    #[must_use]
    pub const fn weighted_pred(&self) -> bool {
        self.flags & PPS_WEIGHTED_PRED != 0
    }

    /// Weighted biprediction.
    #[must_use]
    pub const fn weighted_bipred(&self) -> bool {
        self.flags & PPS_WEIGHTED_BIPRED != 0
    }

    /// Transquant bypass.
    #[must_use]
    pub const fn transquant_bypass(&self) -> bool {
        self.flags & PPS_TRANSQUANT_BYPASS != 0
    }

    /// Tiles.
    #[must_use]
    pub const fn tiles_enabled(&self) -> bool {
        self.flags & PPS_TILES != 0
    }

    /// Uniform tile spacing.
    #[must_use]
    pub const fn uniform_spacing(&self) -> bool {
        self.flags & PPS_UNIFORM_SPACING != 0
    }

    /// Loop filter across tiles.
    #[must_use]
    pub const fn loop_across_tiles(&self) -> bool {
        self.flags & PPS_LOOP_ACROSS_TILES != 0
    }

    /// Wavefront sync.
    #[must_use]
    pub const fn entropy_sync(&self) -> bool {
        self.flags & PPS_SYNC != 0
    }

    /// Loop filter across slices.
    #[must_use]
    pub const fn loop_across_slices(&self) -> bool {
        self.flags & PPS_LOOP_ACROSS_SLICES != 0
    }

    /// Deblocking override.
    #[must_use]
    pub const fn deblocking_override(&self) -> bool {
        self.flags & PPS_DEBLOCK_OVERRIDE != 0
    }

    /// Deblocking disabled.
    #[must_use]
    pub const fn deblocking_disabled(&self) -> bool {
        self.flags & PPS_DEBLOCK_DISABLED != 0
    }

    /// Reference list modification.
    #[must_use]
    pub const fn lists_modification_present(&self) -> bool {
        self.flags & PPS_LISTS_MOD != 0
    }

    /// Slice extension present.
    #[must_use]
    pub const fn slice_ext_present(&self) -> bool {
        self.flags & PPS_SLICE_EXT != 0
    }
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
///
/// # Errors
///
/// Returns `Err` on malformed syntax or tier-declined constructs.
///
/// # Arguments
///
/// * `rbsp` - RBSP bytes with the NAL header stripped.
/// * `sps` - sequence set behind derivation; without it tile extents stay
///   explicit and the trailing entry is absent.
pub fn parse_pps<E: HevcError, G: HevcGate<E>>(rbsp: &[u8], sps: Option<&Sps>) -> Result<Pps, E> {
    let mut reader = BitReader::new(rbsp);
    let id = parse_pps_id(&mut reader)?;
    let sps_id = parse_pps_sps_id(&mut reader)?;
    let mut flags = 0u32;
    if reader.read_bit("dependent_slice_segments_enabled_flag")? {
        flags |= PPS_DEPENDENT;
    }
    if reader.read_bit("output_flag_present_flag")? {
        flags |= PPS_OUTPUT_FLAG;
    }
    let num_extra_slice_header_bits = reader.read_bits(3, "num_extra_slice_header_bits")?;
    let num_extra_slice_header_bits = u8::try_from(num_extra_slice_header_bits).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "num_extra_slice_header_bits",
        })
    })?;
    if reader.read_bit("sign_data_hiding_enabled_flag")? {
        flags |= PPS_SIGN_HIDING;
    }
    if reader.read_bit("cabac_init_present_flag")? {
        flags |= PPS_CABAC_INIT;
    }
    let num_ref_l0 = parse_ref_count(&mut reader, "num_ref_idx_l0_default_active_minus1")?;
    let num_ref_l1 = parse_ref_count(&mut reader, "num_ref_idx_l1_default_active_minus1")?;
    let pic_init_qp_minus26 = parse_qp(&mut reader, "pic_init_qp_minus26")?;
    if reader.read_bit("constrained_intra_pred_flag")? {
        flags |= PPS_CONSTRAINED_INTRA;
    }
    if reader.read_bit("transform_skip_enabled_flag")? {
        flags |= PPS_TRANSFORM_SKIP;
    }
    let diff_cu_qp_delta_depth = parse_cu_qp_depth(&mut reader, &mut flags)?;
    let cb_qp_offset = parse_qp(&mut reader, "pps_cb_qp_offset")?;
    let cr_qp_offset = parse_qp(&mut reader, "pps_cr_qp_offset")?;
    if reader.read_bit("pps_slice_chroma_qp_offsets_present_flag")? {
        flags |= PPS_CHROMA_OFFSETS;
    }
    if reader.read_bit("weighted_pred_flag")? {
        flags |= PPS_WEIGHTED_PRED;
    }
    if reader.read_bit("weighted_bipred_flag")? {
        flags |= PPS_WEIGHTED_BIPRED;
    }
    if reader.read_bit("transquant_bypass_enabled_flag")? {
        flags |= PPS_TRANSQUANT_BYPASS;
    }
    let tiles = parse_tiles(&mut reader, sps, &mut flags)?;
    if tiles.sync {
        flags |= PPS_SYNC;
    }
    if tiles.loop_across_slices {
        flags |= PPS_LOOP_ACROSS_SLICES;
    }
    let deblock = parse_pps_deblocking(&mut reader, &mut flags)?;
    let scaling = parse_pps_scaling(&mut reader, sps, &mut flags)?;
    if reader.read_bit("lists_modification_present_flag")? {
        flags |= PPS_LISTS_MOD;
    }
    let log2_parallel_merge_minus2 =
        parse_bounded_ue(&mut reader, "log2_parallel_merge_level_minus2", 31)?;
    if reader.read_bit("slice_segment_header_extension_present_flag")? {
        flags |= PPS_SLICE_EXT;
    }
    let (pps_extension_present, pps_extension_flags) = parse_pps_extension(&mut reader)?;
    let pps = Pps {
        id,
        sps_id,
        flags,
        num_extra_slice_header_bits,
        num_ref_l0,
        num_ref_l1,
        pic_init_qp_minus26,
        diff_cu_qp_delta_depth,
        cb_qp_offset,
        cr_qp_offset,
        num_tile_columns: tiles.columns,
        num_tile_rows: tiles.rows,
        column_width: tiles.widths,
        row_height: tiles.heights,
        beta_offset_div2: deblock.beta,
        tc_offset_div2: deblock.tc,
        scaling,
        log2_parallel_merge_minus2,
        deblock_control_present: deblock.present,
        pps_extension_present,
        pps_extension_flags,
    };
    G::check_pps(&pps, sps)?;
    Ok(pps)
}

/// Tile grid with its loop filter flags.
struct Tiles {
    /// Tile column count.
    columns: u8,
    /// Tile row count.
    rows: u8,
    /// Tile column widths in treeblocks, derived last with a sequence set.
    widths: Vec<u16>,
    /// Tile row heights in treeblocks, derived last with a sequence set.
    heights: Vec<u16>,
    /// Entropy coding sync enabled.
    sync: bool,
    /// Loop filter across slices enabled.
    loop_across_slices: bool,
}

/// Picture deblocking presence with its offsets.
struct Deblock {
    /// Deblocking control present.
    present: bool,
    /// Beta offset over two.
    beta: i8,
    /// Tc offset over two.
    tc: i8,
}

/// Set id below the set count.
fn parse_pps_id<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<u8, E> {
    let id = reader.read_ue("pic_parameter_set_id")?;
    let id = u8::try_from(id).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "pic_parameter_set_id",
        })
    })?;
    if usize::from(id) >= MAX_PPS {
        return Err(E::parse(ParseKind::Range {
            syntax: "pic_parameter_set_id",
        }));
    }
    Ok(id)
}

/// Sequence id behind the picture set.
fn parse_pps_sps_id<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<u8, E> {
    let id = reader.read_ue("pps_seq_parameter_set_id")?;
    u8::try_from(id).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "pps_seq_parameter_set_id",
        })
    })
}

/// Picture and sequence ids behind a picture set.
///
/// # Errors
///
/// Returns `Err` on truncated or out-of-range ids.
///
/// # Returns
///
/// Leading ids with set counts unenforced.
pub fn parse_pps_ids<E: HevcError>(rbsp: &[u8]) -> Result<(u8, u8), E> {
    let mut reader = BitReader::new(rbsp);
    let pps_id = reader.read_ue("pic_parameter_set_id")?;
    let pps_id = u8::try_from(pps_id).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "pic_parameter_set_id",
        })
    })?;
    let sps_id = reader.read_ue("pps_seq_parameter_set_id")?;
    let sps_id = u8::try_from(sps_id).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "pps_seq_parameter_set_id",
        })
    })?;
    Ok((pps_id, sps_id))
}

/// Reference count field below the 31 ceiling.
pub fn parse_ref_count<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
) -> Result<u8, E> {
    parse_bounded_ue(reader, syntax, 31)
}

/// QP delta field.
fn parse_qp<E: HevcError>(reader: &mut BitReader<'_, E>, syntax: &'static str) -> Result<i8, E> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

/// CU QP delta depth, zeroed without deltas.
fn parse_cu_qp_depth<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    flags: &mut u32,
) -> Result<u8, E> {
    if !reader.read_bit("cu_qp_delta_enabled_flag")? {
        return Ok(0);
    }
    *flags |= PPS_CU_QP_DELTA;
    let value = reader.read_ue("diff_cu_qp_delta_depth")?;
    u8::try_from(value).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "diff_cu_qp_delta_depth",
        })
    })
}

/// Tile grid: counts plus explicit widths with derived last entries.
fn parse_tiles<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: Option<&Sps>,
    flags: &mut u32,
) -> Result<Tiles, E> {
    let enabled = reader.read_bit("tiles_enabled_flag")?;
    let sync = reader.read_bit("entropy_coding_sync_enabled_flag")?;
    if !enabled {
        return Ok(Tiles {
            columns: 1,
            rows: 1,
            widths: Vec::new(),
            heights: Vec::new(),
            sync,
            loop_across_slices: reader.read_bit("pps_loop_filter_across_slices_enabled_flag")?,
        });
    }
    *flags |= PPS_TILES;
    let columns = parse_tile_count(reader, "num_tile_columns_minus1")?;
    let rows = parse_tile_count(reader, "num_tile_rows_minus1")?;
    let (widths, heights) = if reader.read_bit("uniform_spacing_flag")? {
        *flags |= PPS_UNIFORM_SPACING;
        (Vec::new(), Vec::new())
    } else {
        let total_w = sps.map(Sps::width_in_ctbs);
        let total_h = sps.map(Sps::height_in_ctbs);
        (
            parse_tile_widths(reader, columns, total_w, "column_width_minus1")?,
            parse_tile_widths(reader, rows, total_h, "row_height_minus1")?,
        )
    };
    if reader.read_bit("loop_filter_across_tiles_enabled_flag")? {
        *flags |= PPS_LOOP_ACROSS_TILES;
    }
    Ok(Tiles {
        columns,
        rows,
        widths,
        heights,
        sync,
        loop_across_slices: reader.read_bit("pps_loop_filter_across_slices_enabled_flag")?,
    })
}

/// Tile count from its minus-one field.
fn parse_tile_count<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
) -> Result<u8, E> {
    let minus1 = reader.read_ue(syntax)?;
    let count = minus1
        .checked_add(1)
        .ok_or(E::parse(ParseKind::Range { syntax }))?;
    u8::try_from(count).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

/// Explicit tile extents with the last entry derived from `total`.
///
/// The trailing entry derives against a known grid; without `total`
/// the extents stay explicit and unvalidated.
fn parse_tile_widths<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    count: u8,
    total: Option<u32>,
    syntax: &'static str,
) -> Result<Vec<u16>, E> {
    let mut widths = Vec::with_capacity(usize::from(count));
    let mut consumed = 0u32;
    for _ in 1..count {
        let minus1 = reader.read_ue(syntax)?;
        let width = minus1
            .checked_add(1)
            .ok_or(E::parse(ParseKind::Range { syntax }))?;
        if let Some(total) = total {
            consumed = consumed
                .checked_add(width)
                .filter(|sum| *sum < total)
                .ok_or(E::parse(ParseKind::Range { syntax }))?;
        }
        widths.push(u16::try_from(width).map_err(|_| E::parse(ParseKind::Range { syntax }))?);
    }
    if let Some(total) = total {
        let last = total.saturating_sub(consumed);
        widths.push(u16::try_from(last).map_err(|_| E::parse(ParseKind::Range { syntax }))?);
    }
    Ok(widths)
}

/// Picture deblocking presence with its offsets.
fn parse_pps_deblocking<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    flags: &mut u32,
) -> Result<Deblock, E> {
    if !reader.read_bit("deblocking_filter_control_present_flag")? {
        return Ok(Deblock {
            present: false,
            beta: 0,
            tc: 0,
        });
    }
    if reader.read_bit("deblocking_filter_override_enabled_flag")? {
        *flags |= PPS_DEBLOCK_OVERRIDE;
    }
    if reader.read_bit("pps_deblocking_filter_disabled_flag")? {
        *flags |= PPS_DEBLOCK_DISABLED;
        return Ok(Deblock {
            present: true,
            beta: 0,
            tc: 0,
        });
    }
    let beta = parse_qp(reader, "pps_beta_offset_div2")?;
    let tc = parse_qp(reader, "pps_tc_offset_div2")?;
    Ok(Deblock {
        present: true,
        beta,
        tc,
    })
}

/// Picture scaling lists, copied from the sequence set without data.
fn parse_pps_scaling<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: Option<&Sps>,
    flags: &mut u32,
) -> Result<ScalingLists, E> {
    if !reader.read_bit("pps_scaling_list_data_present_flag")? {
        return Ok(sps.map_or_else(ScalingLists::default, |sps| sps.scaling.clone()));
    }
    *flags |= PPS_SCALING;
    parse_scaling_list_data(reader)
}

/// Picture extension presence with its range, multilayer, 3D, and SCC flags.
fn parse_pps_extension<E: HevcError>(
    reader: &mut BitReader<'_, E>,
) -> Result<(bool, [bool; 4]), E> {
    if !reader.read_bit("pps_extension_present_flag")? {
        return Ok((false, [false; 4]));
    }
    let mut flags = [false; 4];
    for (slot, syntax) in flags.iter_mut().zip([
        "pps_range_extension_flag",
        "pps_multilayer_extension_flag",
        "pps_3d_extension_flag",
        "pps_scc_extension_flag",
    ]) {
        *slot = reader.read_bit(syntax)?;
    }
    reader.read_bits(4, "pps_extension_4bits")?;
    Ok((true, flags))
}
