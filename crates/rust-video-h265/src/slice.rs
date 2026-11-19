//! Slice segment headers.

use crate::bits::BitReader;
use crate::error::{HevcError, ParseKind, UnsupportedKind};
use crate::nal::{is_idr, is_irap, MAX_LT_SPS};
use crate::pps::{parse_ref_count, Pps};
use crate::rps::{parse_short_term_rps, ShortTermRps};
use crate::sps::Sps;

/// Slice coding type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SliceType {
    /// Bi-predictive.
    B,
    /// Predictive.
    P,
    /// Intra.
    I,
}

impl SliceType {
    /// Type for the `slice_type` value.
    fn new<E: HevcError>(value: u32) -> Result<Self, E> {
        match value {
            0 => Ok(Self::B),
            1 => Ok(Self::P),
            2 => Ok(Self::I),
            _ => Err(E::parse(ParseKind::Range {
                syntax: "slice_type",
            })),
        }
    }

    /// True for intra slices.
    #[must_use]
    pub const fn is_intra(self) -> bool {
        matches!(self, Self::I)
    }

    /// True for bi-predictive slices.
    #[must_use]
    pub const fn is_b(self) -> bool {
        matches!(self, Self::B)
    }
}

/// Long-term picture description behind POC derivation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LtSpec {
    /// Long-term POC LSB.
    pub poc_lsb: u32,
    /// Used by the current picture.
    pub used: bool,
    /// MSB cycle present.
    pub msb_present: bool,
    /// MSB cycle delta.
    pub msb_cycle: u32,
}

impl LtSpec {
    /// Long-term POC with signaled MSB cycle, H.265 8.3.2.
    ///
    /// `None` answers a count outside the `i32` range.
    #[must_use]
    pub fn poc_msb_present(&self, curr_poc: i32, max_lsb: u32) -> Option<i32> {
        let max = i64::from(max_lsb);
        let poc = i64::from(self.poc_lsb) + i64::from(curr_poc)
            - i64::from(self.msb_cycle) * max
            - i64::from(curr_poc).rem_euclid(max);
        i32::try_from(poc).ok()
    }
}

/// Short-term set provenance behind the DXVA hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RpsBits {
    /// True when the slice carries an explicit set.
    pub explicit: bool,
    /// Bit length of the set syntax.
    pub bits: u16,
    /// Delta POC count of the prediction source.
    pub ref_num_delta_pocs: u8,
    /// Sequence table index behind a referenced set.
    pub table_idx: Option<u8>,
}

/// Parsed slice segment header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceHeader {
    /// First slice segment in the picture.
    pub first: bool,
    /// Slice segment address in treeblocks.
    pub address: u32,
    /// Slice coding type.
    pub slice_type: SliceType,
    /// Picture output flag.
    pub pic_output: bool,
    /// Prior pictures dropped without output.
    pub no_output_prior: bool,
    /// Picture order count LSB, absent on IDR.
    pub poc_lsb: Option<u32>,
    /// Resolved short-term set, absent on IDR.
    pub rps: Option<ShortTermRps>,
    /// Short-term set provenance, absent on IDR.
    pub rps_bits: Option<RpsBits>,
    /// Long-term picture descriptions.
    pub lt: Vec<LtSpec>,
    /// Entry point offset count.
    pub entry_count: u32,
    /// Slice extension length in bytes.
    pub extension_length: u32,
}

/// Slice header prefix: first flag, prior-output flag, set id.
///
/// # Errors
///
/// Returns `Err` on malformed syntax.
///
/// # Arguments
///
/// * `reader` - bit reader over the slice header.
/// * `nal_type` - NAL unit type.
pub fn parse_slice_prefix<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    nal_type: u8,
) -> Result<(bool, bool, u8), E> {
    let first = reader.read_bit("first_slice_segment_in_pic_flag")?;
    let no_output_prior = if is_irap(nal_type) {
        reader.read_bit("no_output_of_prior_pics_flag")?
    } else {
        false
    };
    let pps_id = reader.read_ue("slice_pic_parameter_set_id")?;
    let pps_id = u8::try_from(pps_id).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "slice_pic_parameter_set_id",
        })
    })?;
    Ok((first, no_output_prior, pps_id))
}

/// Slice header against its parameter sets.
///
/// # Errors
///
/// Returns `Err` on malformed syntax.
///
/// # Arguments
///
/// * `reader` - bit reader over the slice header.
/// * `prefix` - first flag, prior-output flag, and set id.
/// * `nal_type` - NAL unit type.
/// * `sps` - sequence set behind the header.
/// * `pps` - picture set behind the header.
pub fn parse_slice_rest<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    prefix: (bool, bool, u8),
    nal_type: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, E> {
    let (first, no_output_prior, _) = prefix;
    let address = parse_slice_address(reader, first, sps, pps)?;
    for _ in 0..pps.num_extra_slice_header_bits {
        reader.read_bit("slice_reserved_flag")?;
    }
    let slice_type = SliceType::new(reader.read_ue("slice_type")?)?;
    if is_idr(nal_type) && !slice_type.is_intra() {
        return Err(E::parse(ParseKind::NonIntraSliceInIdr));
    }
    let pic_output = if pps.output_flag_present() {
        reader.read_bit("pic_output_flag")?
    } else {
        true
    };
    let sets = parse_slice_rps(reader, nal_type, sps)?;
    let temporal_mvp = !is_idr(nal_type)
        && sps.temporal_mvp()
        && reader.read_bit("slice_temporal_mvp_enabled_flag")?;
    let sao = parse_slice_sao(reader, sps)?;
    parse_slice_inter(
        reader,
        slice_type,
        temporal_mvp,
        pps,
        sets.rps.as_ref(),
        &sets.lt,
    )?;
    parse_slice_qp(reader, pps)?;
    let deblocking_disabled = parse_slice_deblocking(reader, pps)?;
    parse_slice_loop_across(reader, pps, sao, deblocking_disabled)?;
    let entry_count = parse_entry_points(reader, pps)?;
    let extension_length = parse_slice_extension(reader, pps)?;
    Ok(SliceHeader {
        first,
        address,
        slice_type,
        pic_output,
        no_output_prior,
        poc_lsb: sets.poc_lsb,
        rps: sets.rps,
        rps_bits: sets.rps_bits,
        lt: sets.lt,
        entry_count,
        extension_length,
    })
}

/// Slice segment address, declining dependent segments.
fn parse_slice_address<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    first: bool,
    sps: &Sps,
    pps: &Pps,
) -> Result<u32, E> {
    if first {
        return Ok(0);
    }
    if pps.dependent_slices() && reader.read_bit("dependent_slice_segment_flag")? {
        return Err(E::unsupported(UnsupportedKind::DependentSlices));
    }
    let ctbs = sps.width_in_ctbs() * sps.height_in_ctbs();
    if ctbs <= 1 {
        return Ok(0);
    }
    let width = 32 - (ctbs - 1).leading_zeros();
    let address = reader.read_bits(width, "slice_segment_address")?;
    if address >= ctbs {
        return Err(E::parse(ParseKind::Range {
            syntax: "slice_segment_address",
        }));
    }
    Ok(address)
}

/// Slice order count plus reference sets.
struct SliceRefSets {
    /// Picture order count LSB.
    poc_lsb: Option<u32>,
    /// Resolved short-term set.
    rps: Option<ShortTermRps>,
    /// Short-term set provenance.
    rps_bits: Option<RpsBits>,
    /// Long-term picture descriptions.
    lt: Vec<LtSpec>,
}

/// Slice POC LSB plus short-term and long-term reference sets.
fn parse_slice_rps<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    nal_type: u8,
    sps: &Sps,
) -> Result<SliceRefSets, E> {
    if is_idr(nal_type) {
        return Ok(SliceRefSets {
            poc_lsb: None,
            rps: None,
            rps_bits: None,
            lt: Vec::new(),
        });
    }
    if sps.separate_colour_plane {
        reader.read_bits(2, "colour_plane_id")?;
    }
    let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
    let poc_lsb = reader.read_bits(width, "slice_pic_order_cnt_lsb")?;
    let (rps, rps_bits) = parse_slice_short_term(reader, sps)?;
    let lt = parse_slice_long_term(reader, sps)?;
    Ok(SliceRefSets {
        poc_lsb: Some(poc_lsb),
        rps: Some(rps),
        rps_bits: Some(rps_bits),
        lt,
    })
}

/// Short-term set: explicit slice syntax or a table entry.
fn parse_slice_short_term<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<(ShortTermRps, RpsBits), E> {
    if !reader.read_bit("short_term_ref_pic_set_sps_flag")? {
        let start = reader.position();
        let num = sps.rps.len();
        let resolved = parse_short_term_rps(reader, num, num, &sps.rps)?;
        let bits = reader
            .position()
            .checked_sub(start)
            .and_then(|bits| u16::try_from(bits).ok())
            .ok_or(E::parse(ParseKind::ShortTermRefPicSetSize))?;
        return Ok((
            resolved.set,
            RpsBits {
                explicit: true,
                bits,
                ref_num_delta_pocs: resolved.ref_num_delta_pocs,
                table_idx: None,
            },
        ));
    }
    if sps.rps.is_empty() {
        return Err(E::parse(ParseKind::EmptySpsRps));
    }
    let index = if sps.rps.len() > 1 {
        let width = 32
            - u32::try_from(sps.rps.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        usize::try_from(reader.read_bits(width, "short_term_ref_pic_set_idx")?).map_err(|_| {
            E::parse(ParseKind::Range {
                syntax: "short_term_ref_pic_set_idx",
            })
        })?
    } else {
        0
    };
    let set = sps.rps.get(index).cloned().ok_or({
        E::parse(ParseKind::Range {
            syntax: "short_term_ref_pic_set_idx",
        })
    })?;
    Ok((
        set,
        RpsBits {
            explicit: false,
            bits: 0,
            ref_num_delta_pocs: 0,
            table_idx: u8::try_from(index).ok(),
        },
    ))
}

/// Long-term pictures: sequence entries plus slice entries.
fn parse_slice_long_term<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<Vec<LtSpec>, E> {
    if !sps.long_term_present() {
        return Ok(Vec::new());
    }
    let from_sps = if sps.lt_poc_lsb.is_empty() {
        0
    } else {
        reader.read_ue("num_long_term_sps")?
    };
    let from_slice = reader.read_ue("num_long_term_pics")?;
    let total = usize::try_from(from_sps)
        .unwrap_or(usize::MAX)
        .saturating_add(usize::try_from(from_slice).unwrap_or(usize::MAX));
    if total > MAX_LT_SPS {
        return Err(E::parse(ParseKind::LtTotalOversize));
    }
    if from_sps > u32::try_from(sps.lt_poc_lsb.len()).unwrap_or(u32::MAX) {
        return Err(E::parse(ParseKind::Range {
            syntax: "num_long_term_sps",
        }));
    }
    let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
    let from_sps_count = usize::try_from(from_sps).unwrap_or(usize::MAX);
    let mut lt = Vec::new();
    for index in 0..total {
        let (poc_lsb, used) = if index < from_sps_count {
            parse_lt_sps_entry(reader, sps)?
        } else {
            let poc_lsb = reader.read_bits(width, "poc_lsb_lt")?;
            let used = reader.read_bit("used_by_curr_pic_lt_flag")?;
            (poc_lsb, used)
        };
        let msb_present = reader.read_bit("delta_poc_msb_present_flag")?;
        let msb_cycle = if msb_present {
            reader.read_ue("delta_poc_msb_cycle_lt")?
        } else {
            0
        };
        lt.push(LtSpec {
            poc_lsb,
            used,
            msb_present,
            msb_cycle,
        });
    }
    Ok(lt)
}

/// Sequence long-term entry by table index.
fn parse_lt_sps_entry<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<(u32, bool), E> {
    let index = if sps.lt_poc_lsb.len() > 1 {
        let width = 32
            - u32::try_from(sps.lt_poc_lsb.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        usize::try_from(reader.read_bits(width, "lt_idx_sps")?).map_err(|_| {
            E::parse(ParseKind::Range {
                syntax: "lt_idx_sps",
            })
        })?
    } else {
        0
    };
    let poc_lsb = sps.lt_poc_lsb.get(index).copied().ok_or({
        E::parse(ParseKind::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    let used = sps.lt_used_by_curr.get(index).copied().ok_or({
        E::parse(ParseKind::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    Ok((poc_lsb, used))
}

/// Slice sample adaptive offset flags.
fn parse_slice_sao<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<(bool, bool), E> {
    if !sps.sao() {
        return Ok((false, false));
    }
    let luma = reader.read_bit("slice_sao_luma_flag")?;
    let chroma = reader.read_bit("slice_sao_chroma_flag")?;
    Ok((luma, chroma))
}

/// Inter section: counts, list modification, collocated, weights, merge.
fn parse_slice_inter<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    temporal_mvp: bool,
    pps: &Pps,
    rps: Option<&ShortTermRps>,
    lt: &[LtSpec],
) -> Result<(), E> {
    if slice_type.is_intra() {
        return Ok(());
    }
    let (num_l0, num_l1) = parse_ref_override(reader, slice_type, pps)?;
    let total_curr = poc_total_curr(rps, lt);
    if pps.lists_modification_present() && total_curr > 1 {
        parse_lists_modification(reader, slice_type, num_l0, num_l1, total_curr)?;
    }
    if slice_type.is_b() {
        reader.read_bit("mvd_l1_zero_flag")?;
    }
    if pps.cabac_init_present() {
        reader.read_bit("cabac_init_flag")?;
    }
    parse_collocated(reader, slice_type, temporal_mvp, num_l0, num_l1)?;
    if (pps.weighted_pred() && !slice_type.is_b()) || (pps.weighted_bipred() && slice_type.is_b()) {
        parse_weight_table(reader, slice_type, num_l0, num_l1)?;
    }
    if reader.read_ue("five_minus_max_num_merge_cand")? > 4 {
        return Err(E::parse(ParseKind::Range {
            syntax: "five_minus_max_num_merge_cand",
        }));
    }
    Ok(())
}

/// Current-picture reference count behind list modification gating.
fn poc_total_curr(rps: Option<&ShortTermRps>, lt: &[LtSpec]) -> usize {
    let short = rps.map_or(0, |rps| rps.used_negative() + rps.used_positive());
    short + lt.iter().filter(|pic| pic.used).count()
}

/// Active reference counts with slice override.
fn parse_ref_override<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    pps: &Pps,
) -> Result<(u8, u8), E> {
    if !reader.read_bit("num_ref_idx_active_override_flag")? {
        return Ok((pps.num_ref_l0, pps.num_ref_l1));
    }
    let num_l0 = parse_ref_count(reader, "num_ref_idx_l0_active_minus1")?;
    let num_l1 = if slice_type.is_b() {
        parse_ref_count(reader, "num_ref_idx_l1_active_minus1")?
    } else {
        pps.num_ref_l1
    };
    Ok((num_l0, num_l1))
}

/// Reference list modification entries.
fn parse_lists_modification<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
    total_curr: usize,
) -> Result<(), E> {
    let width = 32 - u32::try_from(total_curr - 1).unwrap_or(0).leading_zeros();
    if reader.read_bit("ref_pic_list_modification_flag_l0")? {
        for _ in 0..=num_l0 {
            let entry = reader.read_bits(width, "list_entry_l0")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(E::parse(ParseKind::Range {
                    syntax: "list_entry_l0",
                }));
            }
        }
    }
    if slice_type.is_b() && reader.read_bit("ref_pic_list_modification_flag_l1")? {
        for _ in 0..=num_l1 {
            let entry = reader.read_bits(width, "list_entry_l1")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(E::parse(ParseKind::Range {
                    syntax: "list_entry_l1",
                }));
            }
        }
    }
    Ok(())
}

/// Collocated picture description.
fn parse_collocated<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    temporal_mvp: bool,
    num_l0: u8,
    num_l1: u8,
) -> Result<(), E> {
    if !temporal_mvp {
        return Ok(());
    }
    let from_l0 = if slice_type.is_b() {
        reader.read_bit("collocated_from_l0_flag")?
    } else {
        true
    };
    let ceiling = if from_l0 { num_l0 } else { num_l1 };
    if ceiling > 0 {
        let index = reader.read_ue("collocated_ref_idx")?;
        if index > u32::from(ceiling) {
            return Err(E::parse(ParseKind::Range {
                syntax: "collocated_ref_idx",
            }));
        }
    }
    Ok(())
}

/// Prediction weight table.
fn parse_weight_table<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
) -> Result<(), E> {
    if reader.read_ue("luma_log2_weight_denom")? > 7 {
        return Err(E::parse(ParseKind::Range {
            syntax: "luma_log2_weight_denom",
        }));
    }
    reader.read_se("delta_chroma_log2_weight_denom")?;
    parse_weight_list(reader, num_l0)?;
    if slice_type.is_b() {
        parse_weight_list(reader, num_l1)?;
    }
    Ok(())
}

/// One reference weight list.
fn parse_weight_list<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    count_minus1: u8,
) -> Result<(), E> {
    let mut luma = Vec::new();
    let mut chroma = Vec::new();
    for _ in 0..=count_minus1 {
        luma.push(reader.read_bit("luma_weight_flag")?);
    }
    for _ in 0..=count_minus1 {
        chroma.push(reader.read_bit("chroma_weight_flag")?);
    }
    for index in 0..=usize::from(count_minus1) {
        if luma[index] {
            reader.read_se("delta_luma_weight")?;
            reader.read_se("luma_offset")?;
        }
        if chroma[index] {
            for _ in 0..2 {
                reader.read_se("delta_chroma_weight")?;
                reader.read_se("chroma_offset")?;
            }
        }
    }
    Ok(())
}

/// Slice quantization deltas.
fn parse_slice_qp<E: HevcError>(reader: &mut BitReader<'_, E>, pps: &Pps) -> Result<(), E> {
    reader.read_se("slice_qp_delta")?;
    if pps.chroma_offsets_present() {
        reader.read_se("slice_cb_qp_offset")?;
        reader.read_se("slice_cr_qp_offset")?;
    }
    Ok(())
}

/// Slice deblocking description, returning the disabled state.
fn parse_slice_deblocking<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    pps: &Pps,
) -> Result<bool, E> {
    if !pps.deblocking_override() {
        return Ok(pps.deblocking_disabled());
    }
    if !reader.read_bit("deblocking_filter_override_flag")? {
        return Ok(pps.deblocking_disabled());
    }
    if reader.read_bit("slice_deblocking_filter_disabled_flag")? {
        return Ok(true);
    }
    reader.read_se("slice_beta_offset_div2")?;
    reader.read_se("slice_tc_offset_div2")?;
    Ok(false)
}

/// Slice loop filter flag across slices.
fn parse_slice_loop_across<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    pps: &Pps,
    sao: (bool, bool),
    deblocking_disabled: bool,
) -> Result<(), E> {
    if pps.loop_across_slices() && (sao.0 || sao.1 || !deblocking_disabled) {
        reader.read_bit("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(())
}

/// Tile and wavefront entry point offsets, returning the offset count.
fn parse_entry_points<E: HevcError>(reader: &mut BitReader<'_, E>, pps: &Pps) -> Result<u32, E> {
    if !pps.tiles_enabled() && !pps.entropy_sync() {
        return Ok(0);
    }
    let count = reader.read_ue("num_entry_point_offsets")?;
    if count == 0 {
        return Ok(0);
    }
    let length_minus1 = reader.read_ue("offset_len_minus1")?;
    if length_minus1 > 31 {
        return Err(E::parse(ParseKind::Range {
            syntax: "offset_len_minus1",
        }));
    }
    for _ in 0..count {
        reader.read_bits(length_minus1 + 1, "entry_point_offset_minus1")?;
    }
    Ok(count)
}

/// Slice extension bytes, returning the extension length.
fn parse_slice_extension<E: HevcError>(reader: &mut BitReader<'_, E>, pps: &Pps) -> Result<u32, E> {
    if !pps.slice_ext_present() {
        return Ok(0);
    }
    let length = reader.read_ue("slice_segment_header_extension_length")?;
    for _ in 0..length {
        reader.read_bits(8, "slice_segment_header_extension_data_byte")?;
    }
    Ok(length)
}

/// Explicit weights for one reference.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WeightEntry {
    /// Luma delta weight and offset, absent for the default.
    pub luma: Option<(i32, i32)>,
    /// Chroma delta weights and offsets, absent for the default.
    pub chroma: Option<[(i32, i32); 2]>,
}

/// Prediction weight table for one slice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SliceWeights {
    /// Luma weight denominator.
    pub luma_denom: u8,
    /// Chroma denominator delta.
    pub delta_chroma_denom: i32,
    /// Explicit L0 weights by reference index.
    pub l0: Vec<WeightEntry>,
    /// Explicit L1 weights by reference index.
    pub l1: Vec<WeightEntry>,
}

/// Slice tail syntax behind tier submit: counts, flags, weights, and offsets.
///
/// One field per syntax element of 7.3.6.1; packing them would hide
/// the mapping reviewers check against the specification.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SliceTail {
    /// Temporal motion vectors enabled.
    pub temporal_mvp: bool,
    /// Sample adaptive offset on luma.
    pub sao_luma: bool,
    /// Sample adaptive offset on chroma.
    pub sao_chroma: bool,
    /// Colour plane id.
    pub colour_plane: u8,
    /// Active L0 reference count minus one.
    pub num_ref_l0: u8,
    /// Active L1 reference count minus one.
    pub num_ref_l1: u8,
    /// L0 list modification present.
    pub mod_flag_l0: bool,
    /// L0 list modification entries.
    pub mod_entries_l0: Vec<u32>,
    /// L1 list modification present.
    pub mod_flag_l1: bool,
    /// L1 list modification entries.
    pub mod_entries_l1: Vec<u32>,
    /// Zero L1 motion deltas.
    pub mvd_l1_zero: bool,
    /// CABAC init flag.
    pub cabac_init: bool,
    /// Collocated picture from L0.
    pub collocated_from_l0: bool,
    /// Collocated reference index.
    pub collocated_ref_idx: u8,
    /// Prediction weight table.
    pub weights: SliceWeights,
    /// Merge candidates below five.
    pub merge_cand_minus5: u8,
    /// Slice QP delta.
    pub qp_delta: i32,
    /// Slice Cb QP offset.
    pub cb_qp_offset: i32,
    /// Slice Cr QP offset.
    pub cr_qp_offset: i32,
    /// Deblocking filter disabled.
    pub deblocking_disabled: bool,
    /// Deblocking beta offset.
    pub beta_offset: i32,
    /// Deblocking tc offset.
    pub tc_offset: i32,
    /// Loop filter across slices.
    pub loop_filter_across: bool,
    /// Long-term pictures with accumulated MSB cycles.
    pub lt: Vec<LtSpec>,
    /// Slice header size in bits, NAL header included.
    pub bit_size: usize,
}

/// Slice tail over an RBSP `body` with the NAL header stripped.
///
/// Reads mirror `parse_slice_rest` exactly, collecting what that path
/// discards; keep the two in sync. Rejects dependent slices like the
/// header path.
///
/// # Errors
///
/// Returns `Err` on malformed syntax or out-of-range tail values.
///
/// # Arguments
///
/// * `body` - RBSP slice bytes past the NAL header.
/// * `nal_type` - NAL unit type.
/// * `sps` - sequence set behind the header.
/// * `pps` - picture set behind the header.
pub fn parse_slice_tail<E: HevcError>(
    body: &[u8],
    nal_type: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceTail, E> {
    let mut reader = BitReader::new(body);
    let prefix = parse_slice_prefix(&mut reader, nal_type)?;
    let (first, _, _) = prefix;
    let _ = parse_slice_address(&mut reader, first, sps, pps)?;
    for _ in 0..pps.num_extra_slice_header_bits {
        reader.read_bit("slice_reserved_flag")?;
    }
    let slice_type = SliceType::new(reader.read_ue("slice_type")?)?;
    if is_idr(nal_type) && !slice_type.is_intra() {
        return Err(E::parse(ParseKind::NonIntraSliceInIdr));
    }
    if pps.output_flag_present() {
        reader.read_bit("pic_output_flag")?;
    }
    let mut tail = SliceTail {
        collocated_from_l0: true,
        ..SliceTail::default()
    };
    if sps.separate_colour_plane {
        tail.colour_plane =
            u8::try_from(reader.read_bits(2, "colour_plane_id")?).map_err(|_| {
                E::parse(ParseKind::Range {
                    syntax: "colour_plane_id",
                })
            })?;
    }
    let mut short_used = 0;
    if !is_idr(nal_type) {
        short_used = parse_tail_rps(&mut reader, sps, &mut tail)?;
    }
    tail.temporal_mvp = !is_idr(nal_type)
        && sps.temporal_mvp()
        && reader.read_bit("slice_temporal_mvp_enabled_flag")?;
    let sao = parse_slice_sao(&mut reader, sps)?;
    tail.sao_luma = sao.0;
    tail.sao_chroma = sao.1;
    if !slice_type.is_intra() {
        parse_tail_inter(
            &mut reader,
            slice_type,
            tail.temporal_mvp,
            pps,
            short_used,
            &mut tail,
        )?;
    }
    parse_tail_qp(&mut reader, pps, sps, &mut tail)?;
    parse_tail_deblocking(&mut reader, pps, &mut tail)?;
    parse_tail_loop_across(&mut reader, pps, &mut tail)?;
    let _ = parse_entry_points(&mut reader, pps)?;
    let _ = parse_slice_extension(&mut reader, pps)?;
    reader.read_bit("alignment_bit")?;
    while !reader.position().is_multiple_of(8) {
        reader.read_bit("alignment_bit")?;
    }
    tail.bit_size = reader.position() + 16;
    Ok(tail)
}

/// Slice order count plus reference sets into `tail`, returning short-term used count.
fn parse_tail_rps<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
    tail: &mut SliceTail,
) -> Result<usize, E> {
    let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
    reader.read_bits(width, "slice_pic_order_cnt_lsb")?;
    let short_used = parse_tail_short_term(reader, sps)?;
    parse_tail_long_term(reader, sps, tail)?;
    Ok(short_used)
}

/// Short-term set, returning its used-by-current count.
fn parse_tail_short_term<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<usize, E> {
    if !reader.read_bit("short_term_ref_pic_set_sps_flag")? {
        let num = sps.rps.len();
        let resolved = parse_short_term_rps(reader, num, num, &sps.rps)?;
        return Ok(resolved.set.used_negative() + resolved.set.used_positive());
    }
    if sps.rps.is_empty() {
        return Err(E::parse(ParseKind::EmptySpsRps));
    }
    let index = if sps.rps.len() > 1 {
        let width = 32
            - u32::try_from(sps.rps.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        let index = usize::try_from(reader.read_bits(width, "short_term_ref_pic_set_idx")?)
            .map_err(|_| {
                E::parse(ParseKind::Range {
                    syntax: "short_term_ref_pic_set_idx",
                })
            })?;
        if index >= sps.rps.len() {
            return Err(E::parse(ParseKind::Range {
                syntax: "short_term_ref_pic_set_idx",
            }));
        }
        index
    } else {
        0
    };
    let set = sps.rps.get(index).ok_or({
        E::parse(ParseKind::Range {
            syntax: "short_term_ref_pic_set_idx",
        })
    })?;
    Ok(set.used_negative() + set.used_positive())
}

/// Long-term pictures with accumulated MSB cycles into `tail`.
fn parse_tail_long_term<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
    tail: &mut SliceTail,
) -> Result<(), E> {
    if !sps.long_term_present() {
        return Ok(());
    }
    let from_sps = if sps.lt_poc_lsb.is_empty() {
        0
    } else {
        reader.read_ue("num_long_term_sps")?
    };
    let from_slice = reader.read_ue("num_long_term_pics")?;
    let total = usize::try_from(from_sps)
        .unwrap_or(usize::MAX)
        .saturating_add(usize::try_from(from_slice).unwrap_or(usize::MAX));
    if total > MAX_LT_SPS {
        return Err(E::parse(ParseKind::LtTotalOversize));
    }
    if from_sps > u32::try_from(sps.lt_poc_lsb.len()).unwrap_or(u32::MAX) {
        return Err(E::parse(ParseKind::Range {
            syntax: "num_long_term_sps",
        }));
    }
    let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
    let from_sps_count = usize::try_from(from_sps).unwrap_or(usize::MAX);
    let mut lt = Vec::new();
    for index in 0..total {
        let (poc_lsb, used) = if index < from_sps_count {
            parse_tail_lt_sps_entry(reader, sps)?
        } else {
            let poc_lsb = reader.read_bits(width, "poc_lsb_lt")?;
            let used = reader.read_bit("used_by_curr_pic_lt_flag")?;
            (poc_lsb, used)
        };
        let msb_present = reader.read_bit("delta_poc_msb_present_flag")?;
        let mut msb_cycle = if msb_present {
            reader.read_ue("delta_poc_msb_cycle_lt")?
        } else {
            0
        };
        if index != 0 && index != from_sps_count {
            msb_cycle =
                msb_cycle.saturating_add(lt.get(index - 1).map_or(0, |pic: &LtSpec| pic.msb_cycle));
        }
        lt.push(LtSpec {
            poc_lsb,
            used,
            msb_present,
            msb_cycle,
        });
    }
    tail.lt = lt;
    Ok(())
}

/// Sequence long-term entry by table index.
fn parse_tail_lt_sps_entry<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    sps: &Sps,
) -> Result<(u32, bool), E> {
    let index = if sps.lt_poc_lsb.len() > 1 {
        let width = 32
            - u32::try_from(sps.lt_poc_lsb.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        usize::try_from(reader.read_bits(width, "lt_idx_sps")?).map_err(|_| {
            E::parse(ParseKind::Range {
                syntax: "lt_idx_sps",
            })
        })?
    } else {
        0
    };
    let poc_lsb = sps.lt_poc_lsb.get(index).copied().ok_or({
        E::parse(ParseKind::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    let used = sps.lt_used_by_curr.get(index).copied().ok_or({
        E::parse(ParseKind::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    Ok((poc_lsb, used))
}

/// Inter section into `tail`: counts, lists, collocated, weights, merge.
fn parse_tail_inter<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    temporal_mvp: bool,
    pps: &Pps,
    short_used: usize,
    tail: &mut SliceTail,
) -> Result<(), E> {
    let (num_l0, num_l1) = parse_tail_ref_override(reader, slice_type, pps)?;
    tail.num_ref_l0 = num_l0;
    tail.num_ref_l1 = num_l1;
    let total_curr = short_used + tail.lt.iter().filter(|pic| pic.used).count();
    if pps.lists_modification_present() && total_curr > 1 {
        parse_tail_lists_modification(reader, slice_type, num_l0, num_l1, total_curr, tail)?;
    }
    if slice_type.is_b() {
        tail.mvd_l1_zero = reader.read_bit("mvd_l1_zero_flag")?;
    }
    if pps.cabac_init_present() {
        tail.cabac_init = reader.read_bit("cabac_init_flag")?;
    }
    parse_tail_collocated(reader, slice_type, temporal_mvp, num_l0, num_l1, tail)?;
    if (pps.weighted_pred() && !slice_type.is_b()) || (pps.weighted_bipred() && slice_type.is_b()) {
        parse_tail_weight_table(reader, slice_type, num_l0, num_l1, &mut tail.weights)?;
    }
    let merge = reader.read_ue("five_minus_max_num_merge_cand")?;
    if merge > 4 {
        return Err(E::parse(ParseKind::Range {
            syntax: "five_minus_max_num_merge_cand",
        }));
    }
    tail.merge_cand_minus5 = u8::try_from(merge).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "five_minus_max_num_merge_cand",
        })
    })?;
    Ok(())
}

/// Active reference counts with slice override into `tail`.
fn parse_tail_ref_override<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    pps: &Pps,
) -> Result<(u8, u8), E> {
    if !reader.read_bit("num_ref_idx_active_override_flag")? {
        return Ok((pps.num_ref_l0, pps.num_ref_l1));
    }
    let num_l0 = parse_tail_ref_count(reader, "num_ref_idx_l0_active_minus1")?;
    let num_l1 = if slice_type.is_b() {
        parse_tail_ref_count(reader, "num_ref_idx_l1_active_minus1")?
    } else {
        pps.num_ref_l1
    };
    Ok((num_l0, num_l1))
}

/// Reference count field below the 15 ceiling.
fn parse_tail_ref_count<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if value > 14 {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

/// Reference list modification entries into `tail`.
fn parse_tail_lists_modification<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
    total_curr: usize,
    tail: &mut SliceTail,
) -> Result<(), E> {
    let width = 32 - u32::try_from(total_curr - 1).unwrap_or(0).leading_zeros();
    if reader.read_bit("ref_pic_list_modification_flag_l0")? {
        tail.mod_flag_l0 = true;
        for _ in 0..=num_l0 {
            let entry = reader.read_bits(width, "list_entry_l0")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(E::parse(ParseKind::Range {
                    syntax: "list_entry_l0",
                }));
            }
            tail.mod_entries_l0.push(entry);
        }
    }
    if slice_type.is_b() && reader.read_bit("ref_pic_list_modification_flag_l1")? {
        tail.mod_flag_l1 = true;
        for _ in 0..=num_l1 {
            let entry = reader.read_bits(width, "list_entry_l1")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(E::parse(ParseKind::Range {
                    syntax: "list_entry_l1",
                }));
            }
            tail.mod_entries_l1.push(entry);
        }
    }
    Ok(())
}

/// Collocated picture description into `tail`.
fn parse_tail_collocated<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    temporal_mvp: bool,
    num_l0: u8,
    num_l1: u8,
    tail: &mut SliceTail,
) -> Result<(), E> {
    if !temporal_mvp {
        return Ok(());
    }
    if slice_type.is_b() {
        tail.collocated_from_l0 = reader.read_bit("collocated_from_l0_flag")?;
    }
    let ceiling = if tail.collocated_from_l0 {
        num_l0
    } else {
        num_l1
    };
    if ceiling > 0 {
        let index = reader.read_ue("collocated_ref_idx")?;
        if index > u32::from(ceiling) {
            return Err(E::parse(ParseKind::Range {
                syntax: "collocated_ref_idx",
            }));
        }
        tail.collocated_ref_idx = u8::try_from(index).map_err(|_| {
            E::parse(ParseKind::Range {
                syntax: "collocated_ref_idx",
            })
        })?;
    }
    Ok(())
}

/// Prediction weight table into `tail`.
fn parse_tail_weight_table<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
    weights: &mut SliceWeights,
) -> Result<(), E> {
    let luma_denom = reader.read_ue("luma_log2_weight_denom")?;
    if luma_denom > 7 {
        return Err(E::parse(ParseKind::Range {
            syntax: "luma_log2_weight_denom",
        }));
    }
    weights.luma_denom = u8::try_from(luma_denom).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "luma_log2_weight_denom",
        })
    })?;
    weights.delta_chroma_denom = reader.read_se("delta_chroma_log2_weight_denom")?;
    parse_tail_weight_list(reader, num_l0, &mut weights.l0)?;
    if slice_type.is_b() {
        parse_tail_weight_list(reader, num_l1, &mut weights.l1)?;
    }
    Ok(())
}

/// One reference weight list into `out`.
fn parse_tail_weight_list<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    count_minus1: u8,
    out: &mut Vec<WeightEntry>,
) -> Result<(), E> {
    let mut luma = Vec::new();
    let mut chroma = Vec::new();
    for _ in 0..=count_minus1 {
        luma.push(reader.read_bit("luma_weight_flag")?);
    }
    for _ in 0..=count_minus1 {
        chroma.push(reader.read_bit("chroma_weight_flag")?);
    }
    for index in 0..=usize::from(count_minus1) {
        let luma = if luma[index] {
            Some((
                tail_bounded_se(reader, "delta_luma_weight", -128, 127)?,
                tail_bounded_se(reader, "luma_offset", -128, 127)?,
            ))
        } else {
            None
        };
        let chroma = if chroma[index] {
            Some([
                (
                    tail_bounded_se(reader, "delta_chroma_weight", -128, 127)?,
                    tail_bounded_se(reader, "chroma_offset", -512, 511)?,
                ),
                (
                    tail_bounded_se(reader, "delta_chroma_weight", -128, 127)?,
                    tail_bounded_se(reader, "chroma_offset", -512, 511)?,
                ),
            ])
        } else {
            None
        };
        out.push(WeightEntry { luma, chroma });
    }
    Ok(())
}

/// Signed value inside `min..=max`.
fn tail_bounded_se<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
    min: i32,
    max: i32,
) -> Result<i32, E> {
    let value = reader.read_se(syntax)?;
    if value < min || value > max {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    Ok(value)
}

/// Slice quantization deltas into `tail`.
fn parse_tail_qp<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    pps: &Pps,
    sps: &Sps,
    tail: &mut SliceTail,
) -> Result<(), E> {
    tail.qp_delta = tail_bounded_se(reader, "slice_qp_delta", -87, 77)?;
    let qp_y = 26 + i32::from(pps.pic_init_qp_minus26) + tail.qp_delta;
    let qp_bd_offset = 6 * i32::from(sps.bit_depth_luma);
    if qp_y < -qp_bd_offset || qp_y > 51 {
        return Err(E::parse(ParseKind::Range {
            syntax: "slice_qp_delta",
        }));
    }
    if pps.chroma_offsets_present() {
        tail.cb_qp_offset = tail_bounded_se(reader, "slice_cb_qp_offset", -12, 12)?;
        let sum = i32::from(pps.cb_qp_offset) + tail.cb_qp_offset;
        if !(-12..=12).contains(&sum) {
            return Err(E::parse(ParseKind::Range {
                syntax: "slice_cb_qp_offset",
            }));
        }
        tail.cr_qp_offset = tail_bounded_se(reader, "slice_cr_qp_offset", -12, 12)?;
        let sum = i32::from(pps.cr_qp_offset) + tail.cr_qp_offset;
        if !(-12..=12).contains(&sum) {
            return Err(E::parse(ParseKind::Range {
                syntax: "slice_cr_qp_offset",
            }));
        }
    }
    Ok(())
}

/// Slice deblocking description into `tail`.
fn parse_tail_deblocking<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    pps: &Pps,
    tail: &mut SliceTail,
) -> Result<(), E> {
    tail.deblocking_disabled = pps.deblocking_disabled();
    tail.beta_offset = i32::from(pps.beta_offset_div2);
    tail.tc_offset = i32::from(pps.tc_offset_div2);
    tail.loop_filter_across = pps.loop_across_slices();
    if !pps.deblocking_override() {
        return Ok(());
    }
    if !reader.read_bit("deblocking_filter_override_flag")? {
        return Ok(());
    }
    if reader.read_bit("slice_deblocking_filter_disabled_flag")? {
        tail.deblocking_disabled = true;
        return Ok(());
    }
    tail.deblocking_disabled = false;
    tail.beta_offset = tail_bounded_se(reader, "slice_beta_offset_div2", -6, 6)?;
    tail.tc_offset = tail_bounded_se(reader, "slice_tc_offset_div2", -6, 6)?;
    Ok(())
}

/// Slice loop filter flag across slices into `tail`.
fn parse_tail_loop_across<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    pps: &Pps,
    tail: &mut SliceTail,
) -> Result<(), E> {
    if pps.loop_across_slices() && (tail.sao_luma || tail.sao_chroma || !tail.deblocking_disabled) {
        tail.loop_filter_across =
            reader.read_bit("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(())
}
