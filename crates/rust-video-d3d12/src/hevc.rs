//! H.265 Annex B ingest: parameter sets plus slice headers.
//!
//! Progressive pictures; the HEVC DPB owns ordering and output.

use crate::bits::{rbsp, BitReader};
use crate::error::{D3d12Error, ParseReason, UnsupportedReason};
use crate::h264::Crop;

/// Sequence set count, H.265 7.4.3.2.1.
pub(crate) const MAX_SPS: usize = 16;

/// Picture set count, H.265 7.4.3.3.1.
pub(crate) const MAX_PPS: usize = 64;

/// Reference set count, H.265 7.4.8.
pub(crate) const MAX_RPS: usize = 64;

/// Pictures per reference set, H.265 7.4.8.
pub(crate) const MAX_RPS_PICS: usize = 16;

/// Pictures per resolved reference set.
const MAX_RESOLVED_RPS: u8 = 15;

/// First random access NAL type.
const IRAP_FIRST: u8 = 16;

/// Last random access NAL type.
const IRAP_LAST: u8 = 23;

/// First IDR NAL type.
const IDR_FIRST: u8 = 19;

/// Last IDR NAL type.
const IDR_LAST: u8 = 20;

/// First RASL NAL type.
const RASL_FIRST: u8 = 8;

/// Last RASL NAL type.
const RASL_LAST: u8 = 9;

/// CRA NAL type.
const NAL_CRA: u8 = 21;

/// True for random access NAL types.
fn is_irap(nal_type: u8) -> bool {
    (IRAP_FIRST..=IRAP_LAST).contains(&nal_type)
}

/// True for IDR NAL types.
fn is_idr(nal_type: u8) -> bool {
    (IDR_FIRST..=IDR_LAST).contains(&nal_type)
}

/// True for RASL NAL types.
fn is_rasl(nal_type: u8) -> bool {
    (RASL_FIRST..=RASL_LAST).contains(&nal_type)
}

/// True for CRA and BLA NAL types.
fn is_cra_bla(nal_type: u8) -> bool {
    matches!(nal_type, 16..=18 | NAL_CRA)
}

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

/// Short-term reference picture set with resolved delta POCs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ShortTermRps {
    /// Negative delta POCs, nearest first.
    pub(crate) negative: [i32; MAX_RPS_PICS],
    /// Negative used-by-current flags.
    pub(crate) negative_used: [bool; MAX_RPS_PICS],
    /// Negative picture count.
    pub(crate) negative_count: u8,
    /// Positive delta POCs, nearest first.
    pub(crate) positive: [i32; MAX_RPS_PICS],
    /// Positive used-by-current flags.
    pub(crate) positive_used: [bool; MAX_RPS_PICS],
    /// Positive picture count.
    pub(crate) positive_count: u8,
}

impl ShortTermRps {
    /// Total delta POC count.
    pub(crate) fn num_delta_pocs(&self) -> u8 {
        self.negative_count + self.positive_count
    }

    /// Used negative count behind list modification gating.
    pub(crate) fn used_negative(&self) -> usize {
        self.negative_used
            .iter()
            .take(usize::from(self.negative_count))
            .filter(|used| **used)
            .count()
    }

    /// Used positive count behind list modification gating.
    pub(crate) fn used_positive(&self) -> usize {
        self.positive_used
            .iter()
            .take(usize::from(self.positive_count))
            .filter(|used| **used)
            .count()
    }
}

/// Reference set with its prediction provenance.
struct ResolvedRps {
    /// Resolved set.
    set: ShortTermRps,
    /// Delta POC count of the prediction source.
    ref_num_delta_pocs: u8,
}

/// Short-term reference set `index` against `table` of `num_sps` entries.
fn parse_short_term_rps(
    reader: &mut BitReader,
    index: usize,
    num_sps: usize,
    table: &[ShortTermRps],
) -> Result<ResolvedRps, D3d12Error> {
    let predicted = index != 0 && reader.read_bit("inter_ref_pic_set_prediction_flag")?;
    if !predicted {
        return Ok(ResolvedRps {
            set: parse_explicit_rps(reader)?,
            ref_num_delta_pocs: 0,
        });
    }
    let ref_idx = predict_source(reader, index, num_sps)?;
    let reference = table.get(ref_idx).ok_or({
        D3d12Error::Parse(ParseReason::Range {
            syntax: "inter_ref_pic_set_prediction",
        })
    })?;
    let delta_rps = parse_delta_rps(reader)?;
    let flags = parse_predict_flags(reader, usize::from(reference.num_delta_pocs()))?;
    let set = apply_prediction(reference, delta_rps, &flags)?;
    Ok(ResolvedRps {
        set,
        ref_num_delta_pocs: reference.num_delta_pocs(),
    })
}

/// Prediction source index for set `index`.
fn predict_source(
    reader: &mut BitReader,
    index: usize,
    num_sps: usize,
) -> Result<usize, D3d12Error> {
    let delta = if index == num_sps {
        reader.read_ue("delta_idx_minus1")?
    } else {
        0
    };
    let delta = usize::try_from(delta).unwrap_or(usize::MAX);
    if delta >= index {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "inter_ref_pic_set_prediction",
        }));
    }
    Ok(index - delta - 1)
}

/// Delta RPS value behind prediction.
fn parse_delta_rps(reader: &mut BitReader) -> Result<i32, D3d12Error> {
    let sign = reader.read_bit("delta_rps_sign")?;
    let abs = reader.read_ue("abs_delta_rps_minus1")?;
    if abs > 32767 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "abs_delta_rps_minus1",
        }));
    }
    let magnitude = i32::try_from(abs + 1).map_err(|_| {
        D3d12Error::Parse(ParseReason::Range {
            syntax: "abs_delta_rps_minus1",
        })
    })?;
    Ok(if sign { -magnitude } else { magnitude })
}

/// Per-candidate used and delta flags behind prediction.
fn parse_predict_flags(
    reader: &mut BitReader,
    num_delta_pocs: usize,
) -> Result<Vec<(bool, bool)>, D3d12Error> {
    let mut flags = Vec::with_capacity(num_delta_pocs + 1);
    for _ in 0..=num_delta_pocs {
        let used = reader.read_bit("used_by_curr_pic_flag")?;
        let delta = if used {
            true
        } else {
            reader.read_bit("use_delta_flag")?
        };
        flags.push((used, delta));
    }
    Ok(flags)
}

/// Predicted set from `reference`, H.265 7.4.8.
fn apply_prediction(
    reference: &ShortTermRps,
    delta_rps: i32,
    flags: &[(bool, bool)],
) -> Result<ShortTermRps, D3d12Error> {
    let negative_count = usize::from(reference.negative_count);
    let positive_count = usize::from(reference.positive_count);
    let mut set = ShortTermRps::default();
    for (delta, flag) in reference.positive[..positive_count]
        .iter()
        .zip(flags[negative_count..].iter())
        .rev()
    {
        push_shifted(&mut set, *delta, delta_rps, *flag, true)?;
    }
    if delta_rps != 0 {
        let edge = flags[flags.len() - 1];
        push_shifted(&mut set, 0, delta_rps, edge, delta_rps < 0)?;
    }
    for (delta, flag) in reference.negative[..negative_count]
        .iter()
        .zip(flags.iter())
    {
        push_shifted(&mut set, *delta, delta_rps, *flag, true)?;
    }
    for (delta, flag) in reference.negative[..negative_count]
        .iter()
        .zip(flags.iter())
        .rev()
    {
        push_shifted(&mut set, *delta, delta_rps, *flag, false)?;
    }
    for (delta, flag) in reference.positive[..positive_count]
        .iter()
        .zip(flags[negative_count..].iter())
    {
        push_shifted(&mut set, *delta, delta_rps, *flag, false)?;
    }
    Ok(set)
}

/// Shifted candidate into the negative or positive list when kept.
fn push_shifted(
    set: &mut ShortTermRps,
    delta: i32,
    shift: i32,
    flag: (bool, bool),
    negative: bool,
) -> Result<(), D3d12Error> {
    let (used, keep) = flag;
    if !keep {
        return Ok(());
    }
    let poc = delta.checked_add(shift).ok_or({
        D3d12Error::Parse(ParseReason::Range {
            syntax: "delta_poc",
        })
    })?;
    if negative && poc >= 0 || !negative && poc <= 0 {
        return Ok(());
    }
    if set.num_delta_pocs() >= MAX_RESOLVED_RPS {
        return Err(D3d12Error::Parse(ParseReason::ShortTermRefPicSetSize));
    }
    if negative {
        let at = usize::from(set.negative_count);
        set.negative[at] = poc;
        set.negative_used[at] = used;
        set.negative_count += 1;
    } else {
        let at = usize::from(set.positive_count);
        set.positive[at] = poc;
        set.positive_used[at] = used;
        set.positive_count += 1;
    }
    Ok(())
}

/// Explicitly coded reference set.
fn parse_explicit_rps(reader: &mut BitReader) -> Result<ShortTermRps, D3d12Error> {
    let negative = parse_rps_count(reader, "num_negative_pics")?;
    let positive = parse_rps_count(reader, "num_positive_pics")?;
    if usize::from(negative) + usize::from(positive) >= MAX_RPS_PICS {
        return Err(D3d12Error::Parse(ParseReason::ShortTermRefPicSetSize));
    }
    let mut set = ShortTermRps {
        negative_count: negative,
        positive_count: positive,
        ..ShortTermRps::default()
    };
    let mut poc = 0i32;
    for index in 0..usize::from(negative) {
        poc = parse_rps_step(reader, poc, false, "delta_poc_s0_minus1")?;
        set.negative[index] = poc;
        set.negative_used[index] = reader.read_bit("used_by_curr_pic_s0_flag")?;
    }
    poc = 0;
    for index in 0..usize::from(positive) {
        poc = parse_rps_step(reader, poc, true, "delta_poc_s1_minus1")?;
        set.positive[index] = poc;
        set.positive_used[index] = reader.read_bit("used_by_curr_pic_s1_flag")?;
    }
    Ok(set)
}

/// Reference set picture count below the set ceiling.
fn parse_rps_count(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if usize::try_from(value).is_ok_and(|count| count >= MAX_RPS_PICS) {
        return Err(D3d12Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))
}

/// Next explicit delta POC from a minus-one step.
fn parse_rps_step(
    reader: &mut BitReader,
    poc: i32,
    positive: bool,
    syntax: &'static str,
) -> Result<i32, D3d12Error> {
    let step = reader.read_ue(syntax)?;
    if step > 32767 {
        return Err(D3d12Error::Parse(ParseReason::Range { syntax }));
    }
    let step =
        i32::try_from(step + 1).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))?;
    if positive {
        poc.checked_add(step)
    } else {
        poc.checked_sub(step)
    }
    .ok_or({
        D3d12Error::Parse(ParseReason::Range {
            syntax: "delta_poc",
        })
    })
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
    /// Maximum reorder pictures.
    pub(crate) max_num_reorder_pics: u8,
    /// Short-term reference set table.
    pub(crate) rps: Vec<ShortTermRps>,
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
pub(crate) const FLAG_AMP: u8 = 1;
/// Sample adaptive offset in the sequence flags.
pub(crate) const FLAG_SAO: u8 = 1 << 1;
/// Temporal motion vectors in the sequence flags.
pub(crate) const FLAG_TEMPORAL_MVP: u8 = 1 << 2;
/// Strong intra smoothing in the sequence flags.
pub(crate) const FLAG_STRONG_INTRA: u8 = 1 << 3;

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

    /// Maximum POC LSB.
    pub(crate) fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// Luma coding tree block edge in samples.
    pub(crate) fn ctb_size(&self) -> u32 {
        self.min_cb() << self.log2_diff_max_min_cb
    }

    /// Luma coding tree block columns.
    pub(crate) fn width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(self.ctb_size())
    }

    /// Luma coding tree block rows.
    pub(crate) fn height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(self.ctb_size())
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
        return Err(D3d12Error::Unsupported(UnsupportedReason::Sublayers));
    }
    reader.read_bit("sps_temporal_id_nesting_flag")?;
    parse_profile_tier(&mut reader)?;
    let sps_id = parse_id(&mut reader, "seq_parameter_set_id", MAX_SPS)?;
    if reader.read_ue("chroma_format_idc")? != 1 {
        return Err(D3d12Error::Unsupported(UnsupportedReason::ChromaOutside420));
    }
    let width = reader.read_ue("pic_width_in_luma_samples")?;
    let height = reader.read_ue("pic_height_in_luma_samples")?;
    if width == 0 || height == 0 {
        return Err(D3d12Error::Parse(ParseReason::EmptyCodedSize));
    }
    let crop = parse_window(&mut reader, width, height)?;
    if reader.read_ue("bit_depth_luma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::LumaDepthAbove8Bit,
        ));
    }
    if reader.read_ue("bit_depth_chroma_minus8")? != 0 {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ChromaDepthAbove8Bit,
        ));
    }
    let log2_max_poc_lsb_minus4 = parse_small(&mut reader, "log2_max_pic_order_cnt_lsb")?;
    if log2_max_poc_lsb_minus4 > 12 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "log2_max_pic_order_cnt_lsb",
        }));
    }
    let (max_dec_pic_buffering_minus1, max_num_reorder_pics) = parse_buffering(&mut reader)?;
    let coding_block_min = parse_small(&mut reader, "log2_min_luma_coding_block_size")?;
    let coding_block_range = parse_small(&mut reader, "log2_diff_max_min_luma_coding_block")?;
    let transform_min = parse_small(&mut reader, "log2_min_luma_transform_block_size")?;
    let transform_range = parse_small(&mut reader, "log2_diff_max_min_luma_transform")?;
    let max_depth_inter = parse_small(&mut reader, "max_transform_hierarchy_depth_inter")?;
    let max_depth_intra = parse_small(&mut reader, "max_transform_hierarchy_depth_intra")?;
    if reader.read_bit("scaling_list_enabled_flag")? {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ScalingListsPending,
        ));
    }
    let mut flags = 0;
    set_flag(&mut flags, FLAG_AMP, reader.read_bit("amp_enabled_flag")?);
    set_flag(
        &mut flags,
        FLAG_SAO,
        reader.read_bit("sample_adaptive_offset_enabled_flag")?,
    );
    if reader.read_bit("pcm_enabled_flag")? {
        return Err(D3d12Error::Unsupported(UnsupportedReason::PcmPending));
    }
    let rps = parse_rps_table(&mut reader)?;
    if reader.read_bit("long_term_ref_pics_present_flag")? {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::LongTermRefsPending,
        ));
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
        skip_vui(&mut reader)?;
    }
    if reader.read_bit("sps_extension_present_flag")? {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::SpsExtensionPending,
        ));
    }
    Ok(Sps {
        seq_parameter_set_id: sps_id,
        width,
        height,
        crop,
        log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1,
        max_num_reorder_pics,
        rps,
        log2_min_cb_minus3: coding_block_min,
        log2_diff_max_min_cb: coding_block_range,
        log2_min_tb_minus2: transform_min,
        log2_diff_max_min_tb: transform_range,
        max_depth_inter,
        max_depth_intra,
        flags,
    })
}

/// Buffering and reorder limits, single sublayer.
fn parse_buffering(reader: &mut BitReader) -> Result<(u8, u8), D3d12Error> {
    reader.read_bit("sps_sub_layer_ordering_info_present_flag")?;
    let buffering = parse_small(&mut *reader, "sps_max_dec_pic_buffering_minus1")?;
    if buffering > 15 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "sps_max_dec_pic_buffering_minus1",
        }));
    }
    let reorder = parse_small(&mut *reader, "sps_max_num_reorder_pics")?;
    if reorder > 16 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "sps_max_num_reorder_pics",
        }));
    }
    reader.read_ue("sps_max_latency_increase_plus1")?;
    Ok((buffering, reorder))
}

/// Short-term reference set table.
fn parse_rps_table(reader: &mut BitReader) -> Result<Vec<ShortTermRps>, D3d12Error> {
    let count = reader.read_ue("num_short_term_ref_pic_sets")?;
    if usize::try_from(count).is_ok_and(|count| count > MAX_RPS) {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "num_short_term_ref_pic_sets",
        }));
    }
    let mut table = Vec::new();
    for index in 0..count {
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        let total = usize::try_from(count).unwrap_or(usize::MAX);
        let resolved = parse_short_term_rps(reader, index, total, &table)?;
        table.push(resolved.set);
    }
    Ok(table)
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
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ProfileOutsideMain,
        ));
    }
    if interlaced && !progressive {
        return Err(D3d12Error::Unsupported(UnsupportedReason::Interlaced));
    }
    let level = reader.read_bits(8, "general_level_idc")?;
    if !matches!(
        level,
        30 | 60 | 63 | 90 | 93 | 120 | 123 | 150 | 153 | 156 | 180 | 183 | 186 | 210
    ) {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "general_level_idc",
        }));
    }
    Ok(())
}

/// Set id below `max`.
fn parse_id(reader: &mut BitReader, syntax: &'static str, max: usize) -> Result<u8, D3d12Error> {
    let id = reader.read_ue(syntax)?;
    let id = u8::try_from(id).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))?;
    if usize::from(id) >= max {
        return Err(D3d12Error::Parse(ParseReason::Range { syntax }));
    }
    Ok(id)
}

/// Small unsigned field below 32.
fn parse_small(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if value > 31 {
        return Err(D3d12Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))
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
        _ => Err(D3d12Error::Parse(ParseReason::ConfWinOutsideCodedSize)),
    }
}

/// Video usability information, H.265 E.2.1.
fn skip_vui(reader: &mut BitReader) -> Result<(), D3d12Error> {
    if reader.read_bit("aspect_ratio_info_present_flag")?
        && reader.read_bits(8, "aspect_ratio_idc")? == 255
    {
        reader.read_bits(16, "sar_width")?;
        reader.read_bits(16, "sar_height")?;
    }
    if reader.read_bit("overscan_info_present_flag")? {
        reader.read_bit("overscan_appropriate_flag")?;
    }
    if reader.read_bit("video_signal_type_present_flag")? {
        reader.read_bits(3, "video_format")?;
        reader.read_bit("video_full_range_flag")?;
        if reader.read_bit("colour_description_present_flag")? {
            reader.read_bits(8, "colour_primaries")?;
            reader.read_bits(8, "transfer_characteristics")?;
            reader.read_bits(8, "matrix_coeffs")?;
        }
    }
    if reader.read_bit("chroma_loc_info_present_flag")? {
        reader.read_ue("chroma_sample_loc_type_top_field")?;
        reader.read_ue("chroma_sample_loc_type_bottom_field")?;
    }
    reader.read_bit("neutral_chroma_indication_flag")?;
    reader.read_bit("field_seq_flag")?;
    reader.read_bit("frame_field_info_present_flag")?;
    if reader.read_bit("default_display_window_flag")? {
        reader.read_ue("def_disp_win_left_offset")?;
        reader.read_ue("def_disp_win_right_offset")?;
        reader.read_ue("def_disp_win_top_offset")?;
        reader.read_ue("def_disp_win_bottom_offset")?;
    }
    if reader.read_bit("vui_timing_info_present_flag")? {
        reader.read_bits(32, "vui_num_units_in_tick")?;
        reader.read_bits(32, "vui_time_scale")?;
        if reader.read_bit("vui_poc_proportional_to_timing_flag")? {
            reader.read_ue("vui_num_ticks_poc_diff_one_minus1")?;
        }
        if reader.read_bit("vui_hrd_parameters_present_flag")? {
            parse_hrd(reader)?;
        }
    }
    if reader.read_bit("bitstream_restriction_flag")? {
        reader.read_bit("tiles_fixed_structure_flag")?;
        reader.read_bit("motion_vectors_over_pic_boundaries_flag")?;
        reader.read_bit("restricted_ref_pic_lists_flag")?;
        reader.read_ue("min_spatial_segmentation_idc")?;
        reader.read_ue("max_bytes_per_pic_denom")?;
        reader.read_ue("max_bits_per_min_cu_denom")?;
        reader.read_ue("log2_max_mv_length_horizontal")?;
        reader.read_ue("log2_max_mv_length_vertical")?;
    }
    Ok(())
}

/// Hypothetical reference decoder parameters, H.265 E.2.2, single sublayer.
fn parse_hrd(reader: &mut BitReader) -> Result<(), D3d12Error> {
    let nal = reader.read_bit("nal_hrd_parameters_present_flag")?;
    let vcl = reader.read_bit("vcl_hrd_parameters_present_flag")?;
    let mut sub_pic = false;
    if nal || vcl {
        sub_pic = parse_hrd_common(reader)?;
    }
    let count = parse_hrd_layer(reader)?;
    for _ in 0..=count {
        if nal {
            parse_hrd_sublayer(reader, sub_pic)?;
        }
        if vcl {
            parse_hrd_sublayer(reader, sub_pic)?;
        }
    }
    Ok(())
}

/// HRD common information, returning the sub-picture flag.
fn parse_hrd_common(reader: &mut BitReader) -> Result<bool, D3d12Error> {
    let sub_pic = reader.read_bit("sub_pic_hrd_params_present_flag")?;
    if sub_pic {
        reader.read_bits(8, "tick_divisor_minus2")?;
        reader.read_bits(5, "du_cpb_removal_delay_increment_length_minus1")?;
        reader.read_bit("sub_pic_cpb_params_in_pic_timing_sei_flag")?;
        reader.read_bits(5, "dpb_output_delay_du_length_minus1")?;
    }
    reader.read_bits(4, "bit_rate_scale")?;
    reader.read_bits(4, "cpb_size_scale")?;
    if sub_pic {
        reader.read_bits(4, "cpb_size_du_scale")?;
    }
    reader.read_bits(5, "initial_cpb_removal_delay_length_minus1")?;
    reader.read_bits(5, "au_cpb_removal_delay_length_minus1")?;
    reader.read_bits(5, "dpb_output_delay_length_minus1")?;
    Ok(sub_pic)
}

/// HRD sublayer schedule, returning the CPB count minus one.
fn parse_hrd_layer(reader: &mut BitReader) -> Result<u32, D3d12Error> {
    let fixed_general = reader.read_bit("fixed_pic_rate_general_flag")?;
    let fixed_within = if fixed_general {
        true
    } else {
        reader.read_bit("fixed_pic_rate_within_cvs_flag")?
    };
    let low_delay = if fixed_within {
        reader.read_ue("elemental_duration_in_tc_minus1")?;
        false
    } else {
        reader.read_bit("low_delay_hrd_flag")?
    };
    if low_delay {
        return Ok(0);
    }
    let count = reader.read_ue("cpb_cnt_minus1")?;
    if count > 31 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "cpb_cnt_minus1",
        }));
    }
    Ok(count)
}

/// One HRD coded picture buffer description.
fn parse_hrd_sublayer(reader: &mut BitReader, sub_pic: bool) -> Result<(), D3d12Error> {
    reader.read_ue("bit_rate_value_minus1")?;
    reader.read_ue("cpb_size_value_minus1")?;
    if sub_pic {
        reader.read_ue("cpb_size_du_value_minus1")?;
        reader.read_ue("bit_rate_du_value_minus1")?;
    }
    reader.read_bit("cbr_flag")?;
    Ok(())
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
    let sps_id = u8::try_from(raw_sps).map_err(|_| {
        D3d12Error::Parse(ParseReason::Range {
            syntax: "pps_seq_parameter_set_id",
        })
    })?;
    if sps_id != sps.seq_parameter_set_id {
        return Err(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps));
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
        u8::try_from(reader.read_bits(3, "num_extra_slice_header_bits")?).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "num_extra_slice_header_bits",
            })
        })?;
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
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ScalingListsPending,
        ));
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
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "log2_parallel_merge_level",
        }));
    }
    pps.log2_parallel_merge_level_minus2 = merge_level;
    set_wide_flag(
        &mut pps.flags,
        P_SLICE_EXT,
        reader.read_bit("slice_segment_header_extension_present_flag")?,
    );
    if reader.read_bit("pps_extension_present_flag")? {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::PpsExtensionPending,
        ));
    }
    Ok(pps)
}

/// QP delta field.
fn parse_qp(reader: &mut BitReader, syntax: &'static str) -> Result<i8, D3d12Error> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))
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
        return Err(D3d12Error::Unsupported(UnsupportedReason::TileGridPending));
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
    u16::try_from(value).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))
}

/// Uniform tile widths and heights from the coding tree grid.
fn uniform_tiles(sps: &Sps, pps: &mut Pps) -> Result<(), D3d12Error> {
    let ctb = sps.min_cb() << sps.log2_diff_max_min_cb;
    let ctbs_w = sps.width.div_ceil(ctb).max(1);
    let ctbs_h = sps.height.div_ceil(ctb).max(1);
    let columns = u32::from(pps.tile_columns_minus1) + 1;
    let rows = u32::from(pps.tile_rows_minus1) + 1;
    for (index, slot) in pps.column_width_minus1.iter_mut().enumerate() {
        let i = u32::try_from(index).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "tile column",
            })
        })?;
        if i >= columns {
            break;
        }
        let width = (i + 1) * ctbs_w / columns - i * ctbs_w / columns;
        *slot = u16::try_from(width.saturating_sub(1)).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "tile column",
            })
        })?;
    }
    for (index, slot) in pps.row_height_minus1.iter_mut().enumerate() {
        let i = u32::try_from(index)
            .map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax: "tile row" }))?;
        if i >= rows {
            break;
        }
        let height = (i + 1) * ctbs_h / rows - i * ctbs_h / rows;
        *slot = u16::try_from(height.saturating_sub(1))
            .map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax: "tile row" }))?;
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
            _ => Err(D3d12Error::Parse(ParseReason::Range {
                syntax: "slice_type",
            })),
        }
    }

    /// True for intra slices.
    pub(crate) const fn is_intra(self) -> bool {
        matches!(self, Self::I)
    }

    /// True for bi-predictive slices.
    pub(crate) const fn is_b(self) -> bool {
        matches!(self, Self::B)
    }
}

/// Short-term set provenance behind the DXVA hints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RpsBits {
    /// True when the slice carries an explicit set.
    pub(crate) explicit: bool,
    /// Bit length of the set syntax.
    pub(crate) bits: u16,
    /// Delta POC count of the prediction source.
    pub(crate) ref_num_delta_pocs: u8,
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
    /// Picture output flag.
    pub(crate) pic_output: bool,
    /// Prior pictures dropped without output.
    pub(crate) no_output_prior: bool,
    /// Picture order count LSB, absent on IDR.
    pub(crate) poc_lsb: Option<u32>,
    /// Resolved short-term set, absent on IDR.
    pub(crate) rps: Option<ShortTermRps>,
    /// Short-term set provenance, absent on IDR.
    pub(crate) rps_bits: Option<RpsBits>,
}

/// Slice header prefix: first flag, prior-output flag, set id.
fn parse_slice_prefix(
    reader: &mut BitReader,
    nal_type: u8,
) -> Result<(bool, bool, u8), D3d12Error> {
    let first = reader.read_bit("first_slice_segment_in_pic_flag")?;
    let no_output_prior = if is_irap(nal_type) {
        reader.read_bit("no_output_of_prior_pics_flag")?
    } else {
        false
    };
    let pps_id = reader.read_ue("slice_pic_parameter_set_id")?;
    let pps_id = u8::try_from(pps_id).map_err(|_| {
        D3d12Error::Parse(ParseReason::Range {
            syntax: "slice_pic_parameter_set_id",
        })
    })?;
    Ok((first, no_output_prior, pps_id))
}

/// Slice header against its parameter sets.
fn parse_slice_rest(
    reader: &mut BitReader,
    prefix: (bool, bool, u8),
    nal_type: u8,
    sps: &Sps,
    pps: &Pps,
) -> Result<SliceHeader, D3d12Error> {
    let (first, no_output_prior, pps_id) = prefix;
    let address = parse_slice_address(reader, first, sps, pps)?;
    for _ in 0..pps.num_extra_slice_header_bits {
        reader.read_bit("slice_reserved_flag")?;
    }
    let slice_type = SliceType::new(reader.read_ue("slice_type")?)?;
    if is_idr(nal_type) && !slice_type.is_intra() {
        return Err(D3d12Error::Parse(ParseReason::NonIntraSliceInIdr));
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
    parse_slice_inter(reader, slice_type, temporal_mvp, pps, sets.rps.as_ref())?;
    parse_slice_qp(reader, pps)?;
    let deblocking_disabled = parse_slice_deblocking(reader, pps)?;
    parse_slice_loop_across(reader, pps, sao, deblocking_disabled)?;
    parse_entry_points(reader, pps)?;
    parse_slice_extension(reader, pps)?;
    Ok(SliceHeader {
        pps_id,
        slice_type,
        address,
        first,
        pic_output,
        no_output_prior,
        poc_lsb: sets.poc_lsb,
        rps: sets.rps,
        rps_bits: sets.rps_bits,
    })
}

/// Slice segment address, declining dependent segments.
fn parse_slice_address(
    reader: &mut BitReader,
    first: bool,
    sps: &Sps,
    pps: &Pps,
) -> Result<u32, D3d12Error> {
    if first {
        return Ok(0);
    }
    if pps.dependent_slices() && reader.read_bit("dependent_slice_segment_flag")? {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::DependentSlicesPending,
        ));
    }
    let ctbs = sps.width_in_ctbs() * sps.height_in_ctbs();
    if ctbs <= 1 {
        return Ok(0);
    }
    let width = 32 - (ctbs - 1).leading_zeros();
    let address = reader.read_bits(width, "slice_segment_address")?;
    if address >= ctbs {
        return Err(D3d12Error::Parse(ParseReason::Range {
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
}

/// Slice POC LSB plus the short-term reference set.
fn parse_slice_rps(
    reader: &mut BitReader,
    nal_type: u8,
    sps: &Sps,
) -> Result<SliceRefSets, D3d12Error> {
    if is_idr(nal_type) {
        return Ok(SliceRefSets {
            poc_lsb: None,
            rps: None,
            rps_bits: None,
        });
    }
    let width = u32::from(sps.log2_max_poc_lsb_minus4) + 4;
    let poc_lsb = reader.read_bits(width, "slice_pic_order_cnt_lsb")?;
    let (rps, rps_bits) = parse_slice_short_term(reader, sps)?;
    Ok(SliceRefSets {
        poc_lsb: Some(poc_lsb),
        rps: Some(rps),
        rps_bits: Some(rps_bits),
    })
}

/// Short-term set: explicit slice syntax or a table entry.
fn parse_slice_short_term(
    reader: &mut BitReader,
    sps: &Sps,
) -> Result<(ShortTermRps, RpsBits), D3d12Error> {
    if !reader.read_bit("short_term_ref_pic_set_sps_flag")? {
        let start = reader.position();
        let num = sps.rps.len();
        let resolved = parse_short_term_rps(reader, num, num, &sps.rps)?;
        let bits = reader
            .position()
            .checked_sub(start)
            .and_then(|bits| u16::try_from(bits).ok())
            .ok_or(D3d12Error::Parse(ParseReason::ShortTermRefPicSetSize))?;
        return Ok((
            resolved.set,
            RpsBits {
                explicit: true,
                bits,
                ref_num_delta_pocs: resolved.ref_num_delta_pocs,
            },
        ));
    }
    let index = if sps.rps.len() > 1 {
        let width = 32
            - u32::try_from(sps.rps.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        usize::try_from(reader.read_bits(width, "short_term_ref_pic_set_idx")?).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "short_term_ref_pic_set_idx",
            })
        })?
    } else {
        0
    };
    let set = sps.rps.get(index).cloned().ok_or({
        D3d12Error::Parse(ParseReason::Range {
            syntax: "short_term_ref_pic_set_idx",
        })
    })?;
    Ok((
        set,
        RpsBits {
            explicit: false,
            bits: 0,
            ref_num_delta_pocs: 0,
        },
    ))
}

/// Slice sample adaptive offset flags.
fn parse_slice_sao(reader: &mut BitReader, sps: &Sps) -> Result<(bool, bool), D3d12Error> {
    if !sps.sao() {
        return Ok((false, false));
    }
    let luma = reader.read_bit("slice_sao_luma_flag")?;
    let chroma = reader.read_bit("slice_sao_chroma_flag")?;
    Ok((luma, chroma))
}

/// Inter section: counts, list modification, collocated, weights, merge.
fn parse_slice_inter(
    reader: &mut BitReader,
    slice_type: SliceType,
    temporal_mvp: bool,
    pps: &Pps,
    rps: Option<&ShortTermRps>,
) -> Result<(), D3d12Error> {
    if slice_type.is_intra() {
        return Ok(());
    }
    let (num_l0, num_l1) = parse_ref_override(reader, slice_type, pps)?;
    let total_curr = poc_total_curr(rps);
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
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "five_minus_max_num_merge_cand",
        }));
    }
    Ok(())
}

/// Current-picture reference count behind list modification gating.
fn poc_total_curr(rps: Option<&ShortTermRps>) -> usize {
    rps.map_or(0, |rps| rps.used_negative() + rps.used_positive())
}

/// Reference count field below the 14 ceiling.
fn parse_ref_count(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d12Error> {
    let value = reader.read_ue(syntax)?;
    if value > 14 {
        return Err(D3d12Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d12Error::Parse(ParseReason::Range { syntax }))
}

/// Active reference counts with slice override.
fn parse_ref_override(
    reader: &mut BitReader,
    slice_type: SliceType,
    pps: &Pps,
) -> Result<(u8, u8), D3d12Error> {
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
fn parse_lists_modification(
    reader: &mut BitReader,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
    total_curr: usize,
) -> Result<(), D3d12Error> {
    let width = 32 - u32::try_from(total_curr - 1).unwrap_or(0).leading_zeros();
    if reader.read_bit("ref_pic_list_modification_flag_l0")? {
        for _ in 0..=num_l0 {
            let entry = reader.read_bits(width, "list_entry_l0")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(D3d12Error::Parse(ParseReason::Range {
                    syntax: "list_entry_l0",
                }));
            }
        }
    }
    if slice_type.is_b() && reader.read_bit("ref_pic_list_modification_flag_l1")? {
        for _ in 0..=num_l1 {
            let entry = reader.read_bits(width, "list_entry_l1")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(D3d12Error::Parse(ParseReason::Range {
                    syntax: "list_entry_l1",
                }));
            }
        }
    }
    Ok(())
}

/// Collocated picture description.
fn parse_collocated(
    reader: &mut BitReader,
    slice_type: SliceType,
    temporal_mvp: bool,
    num_l0: u8,
    num_l1: u8,
) -> Result<(), D3d12Error> {
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
            return Err(D3d12Error::Parse(ParseReason::Range {
                syntax: "collocated_ref_idx",
            }));
        }
    }
    Ok(())
}

/// Prediction weight table.
fn parse_weight_table(
    reader: &mut BitReader,
    slice_type: SliceType,
    num_l0: u8,
    num_l1: u8,
) -> Result<(), D3d12Error> {
    if reader.read_ue("luma_log2_weight_denom")? > 7 {
        return Err(D3d12Error::Parse(ParseReason::Range {
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
fn parse_weight_list(reader: &mut BitReader, count_minus1: u8) -> Result<(), D3d12Error> {
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
fn parse_slice_qp(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d12Error> {
    reader.read_se("slice_qp_delta")?;
    if pps.slice_chroma_qp_offsets_present() {
        reader.read_se("slice_cb_qp_offset")?;
        reader.read_se("slice_cr_qp_offset")?;
    }
    Ok(())
}

/// Slice deblocking description, returning the disabled state.
fn parse_slice_deblocking(reader: &mut BitReader, pps: &Pps) -> Result<bool, D3d12Error> {
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
fn parse_slice_loop_across(
    reader: &mut BitReader,
    pps: &Pps,
    sao: (bool, bool),
    deblocking_disabled: bool,
) -> Result<(), D3d12Error> {
    if pps.loop_across_slices() && (sao.0 || sao.1 || !deblocking_disabled) {
        reader.read_bit("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(())
}

/// Tile and wavefront entry point offsets.
fn parse_entry_points(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d12Error> {
    if !pps.tiles() && !pps.entropy_sync() {
        return Ok(());
    }
    let count = reader.read_ue("num_entry_point_offsets")?;
    if count == 0 {
        return Ok(());
    }
    let length_minus1 = reader.read_ue("offset_len_minus1")?;
    if length_minus1 > 31 {
        return Err(D3d12Error::Parse(ParseReason::Range {
            syntax: "offset_len_minus1",
        }));
    }
    for _ in 0..count {
        reader.read_bits(length_minus1 + 1, "entry_point_offset_minus1")?;
    }
    Ok(())
}

/// Slice extension bytes.
fn parse_slice_extension(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d12Error> {
    if !pps.slice_ext_present() {
        return Ok(());
    }
    let length = reader.read_ue("slice_segment_header_extension_length")?;
    for _ in 0..length {
        reader.read_bits(8, "slice_segment_header_extension_data_byte")?;
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
    /// NAL unit type.
    pub(crate) nal_type: u8,
    /// Temporal id.
    pub(crate) temporal_id: u8,
}

/// One coded picture: slices sharing the set and NAL type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// Picture slices in bitstream order.
    pub(crate) slices: Vec<PendingSlice>,
    /// True for IDR pictures.
    pub(crate) idr: bool,
    /// True for random access pictures.
    pub(crate) irap: bool,
    /// True for RASL pictures.
    pub(crate) rasl: bool,
}

impl Picture {
    /// Picture set id.
    pub(crate) fn pps_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.header.pps_id)
    }

    /// Temporal id.
    pub(crate) fn temporal_id(&self) -> u8 {
        self.slices.first().map_or(0, |slice| slice.temporal_id)
    }

    /// True when prior pictures drop without output.
    pub(crate) fn no_output_prior(&self) -> bool {
        self.slices
            .first()
            .is_some_and(|slice| slice.header.no_output_prior)
    }

    /// Picture order count LSB, absent on IDR.
    pub(crate) fn poc_lsb(&self) -> Option<u32> {
        self.slices.first()?.header.poc_lsb
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
    /// Completed pictures in decode order.
    pictures: u64,
    /// Leading pictures of the open random access point undecodable.
    rasl_undecodable: bool,
}

impl Ingest {
    /// Empty ingest.
    pub(crate) fn new() -> Self {
        Self {
            sps: core::array::from_fn(|_| None),
            pps: core::array::from_fn(|_| None),
            pending: Vec::new(),
            pictures: 0,
            rasl_undecodable: false,
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
            return Err(D3d12Error::Parse(ParseReason::NalForbiddenBit));
        }
        let nal_type = (header[0] >> 1) & 0x3F;
        let layer_id = ((u16::from(header[0]) & 1) << 5) | (u16::from(header[1]) >> 3);
        if layer_id != 0 {
            return Err(D3d12Error::Unsupported(UnsupportedReason::Multilayer));
        }
        let temporal_plus1 = header[1] & 7;
        if temporal_plus1 == 0 {
            return Err(D3d12Error::Parse(ParseReason::NalTemporalId));
        }
        let temporal_id = temporal_plus1 - 1;
        match nal_type {
            0..=21 => self.ingest_vcl(nal, nal_type, temporal_id, pictures),
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
        temporal_id: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), D3d12Error> {
        if (10..=15).contains(&nal_type) {
            return Err(D3d12Error::Unsupported(
                UnsupportedReason::ReservedSliceType,
            ));
        }
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d12Error::Parse(ParseReason::SliceWithoutHeader));
        };
        let pps_id = peek_pps_id(body, nal_type)?;
        let pps = self
            .pps(pps_id)
            .ok_or(D3d12Error::Parse(ParseReason::SliceCitesUnknownPps))?
            .clone();
        let sps = self
            .sps(pps.seq_parameter_set_id)
            .ok_or(D3d12Error::Parse(ParseReason::SliceCitesUnknownSps))?
            .clone();
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix(&mut reader, nal_type)?;
        let parsed = parse_slice_rest(&mut reader, prefix, nal_type, &sps, &pps)?;
        if parsed.first && !self.pending.is_empty() {
            if let Some(picture) = self.complete_picture()? {
                pictures.push(picture);
            }
        }
        self.pending.push(PendingSlice {
            header: parsed,
            nal: nal.to_vec(),
            nal_type,
            temporal_id,
        });
        Ok(())
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), D3d12Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d12Error::Parse(ParseReason::SpsWithoutBody));
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
            return Err(D3d12Error::Parse(ParseReason::PpsWithoutBody));
        };
        let mut probe = BitReader::new(body);
        probe.read_ue("pic_parameter_set_id")?;
        let sps_id = probe.read_ue("seq_parameter_set_id")?;
        let sps_id = u8::try_from(sps_id).map_err(|_| {
            D3d12Error::Parse(ParseReason::Range {
                syntax: "seq_parameter_set_id",
            })
        })?;
        let sps = self
            .sps(sps_id)
            .ok_or(D3d12Error::Parse(ParseReason::PpsCitesUnknownSps))?
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
            return Err(D3d12Error::Parse(ParseReason::PictureWithoutFirstSlice));
        }
        for slice in &slices {
            if slice.header.pps_id != first.header.pps_id || slice.nal_type != first.nal_type {
                return Err(D3d12Error::Parse(ParseReason::PictureSlicesDisagree));
            }
        }
        let mut previous = 0;
        for slice in slices.iter().skip(1) {
            if slice.header.first || slice.header.address <= previous {
                return Err(D3d12Error::Unsupported(
                    UnsupportedReason::ArbitrarySliceOrder,
                ));
            }
            previous = slice.header.address;
        }
        let nal_type = first.nal_type;
        let picture = Picture {
            idr: is_idr(nal_type),
            irap: is_irap(nal_type),
            rasl: is_rasl(nal_type),
            slices,
        };
        if is_cra_bla(nal_type) {
            self.rasl_undecodable = self.pictures == 0;
        } else if is_idr(nal_type) {
            self.rasl_undecodable = false;
        }
        self.pictures += 1;
        if picture.rasl && self.rasl_undecodable {
            return Ok(None);
        }
        Ok(Some(picture))
    }
}

/// Picture set id behind a slice body prefix.
fn peek_pps_id(body: &[u8], nal_type: u8) -> Result<u8, D3d12Error> {
    let mut reader = BitReader::new(body);
    reader.read_bit("first_slice_segment_in_pic_flag")?;
    if is_irap(nal_type) {
        reader.read_bit("no_output_of_prior_pics_flag")?;
    }
    let id = reader.read_ue("slice_pic_parameter_set_id")?;
    u8::try_from(id).map_err(|_| {
        D3d12Error::Parse(ParseReason::Range {
            syntax: "slice_pic_parameter_set_id",
        })
    })
}

/// Synthetic inputs behind submission tests.
#[cfg(test)]
pub(crate) mod fixtures {
    /// Test bit writer.
    pub(crate) struct BitWriter {
        /// Finished bytes.
        bytes: Vec<u8>,
        /// Open byte under fill.
        current: u8,
        /// Bits filled in the open byte.
        filled: u8,
    }

    impl BitWriter {
        /// Empty writer.
        pub(crate) fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                filled: 0,
            }
        }

        /// Single bit.
        pub(crate) fn bit(&mut self, set: bool) {
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
        pub(crate) fn bits(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bit(value >> index & 1 != 0);
            }
        }

        /// Unsigned Exp-Golomb code.
        pub(crate) fn ue(&mut self, value: u32) {
            let code = value + 1;
            let width = 32 - code.leading_zeros();
            for _ in 1..width {
                self.bit(false);
            }
            self.bits(code, width);
        }

        /// Signed Exp-Golomb code.
        pub(crate) fn se(&mut self, value: i32) {
            let code = if value > 0 {
                u32::try_from(value).unwrap_or(u32::MAX) * 2 - 1
            } else {
                u32::try_from(-value).unwrap_or(u32::MAX) * 2
            };
            self.ue(code);
        }

        /// Finished RBSP with emulation prevention inserted.
        pub(crate) fn finish(mut self) -> Vec<u8> {
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
        sps_body_inner(false)
    }

    /// Main-profile 64x48 sequence set body with one reference set.
    pub(crate) fn sps_body_rps() -> Vec<u8> {
        sps_body_inner(true)
    }

    /// Sequence set body, with a `[-2]` used set when `with_rps`.
    fn sps_body_inner(with_rps: bool) -> Vec<u8> {
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
        if with_rps {
            writer.ue(1);
            writer.ue(1);
            writer.ue(0);
            writer.ue(1);
            writer.bit(true);
        } else {
            writer.ue(0);
        }
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

    /// P slice body over the test sets: POC 2 with an explicit `[-2]` used set.
    pub(crate) fn p_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.bits(2, 4);
        writer.bit(false);
        writer.ue(1);
        writer.ue(0);
        writer.ue(1);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.finish()
    }

    /// P slice body citing table set zero at POC 2.
    pub(crate) fn p_body_table() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.bits(2, 4);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.finish()
    }

    /// B slice body over the test sets: POC 1 with explicit `[-1]` and `[1]` used sets.
    pub(crate) fn b_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.bits(1, 4);
        writer.bit(false);
        writer.ue(1);
        writer.ue(1);
        writer.ue(0);
        writer.bit(true);
        writer.ue(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
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
    use super::fixtures::{
        b_body, idr_body, nal, p_body, p_body_table, packet, pps_body, pps_body_merge, sps_body,
        sps_body_rps,
    };
    use super::skip_vui;
    use super::{parse_pps, parse_sps, rbsp, split_nals, Ingest, SliceType};
    use crate::bits::BitReader;

    #[test]
    fn skip_vui_skips_hrd_payload() {
        let mut writer = super::fixtures::BitWriter::new();
        for _ in 0..8 {
            writer.bit(false);
        }
        writer.bit(true);
        writer.bits(1, 32);
        writer.bits(1, 32);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 4);
        writer.bits(0, 4);
        writer.bits(0, 5);
        writer.bits(0, 5);
        writer.bits(0, 5);
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        let body = rbsp(&writer.finish());
        let mut hrd = BitReader::new(&body);
        skip_vui(&mut hrd).expect("hrd skipped");
        let mut bare = BitReader::new(&[0x00, 0x10]);
        skip_vui(&mut bare).expect("vui without timing");
    }

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
        assert_eq!(sps.max_num_reorder_pics, 0);
        assert_eq!(sps.rps.len(), 0);
        assert_eq!(sps.max_poc_lsb(), 16);
    }

    #[test]
    fn sps_parses_rps_table() {
        let sps = parse_sps(&rbsp(&sps_body_rps())).expect("valid sps");
        assert_eq!(sps.rps.len(), 1);
        let set = &sps.rps[0];
        assert_eq!(set.negative_count, 1);
        assert_eq!(set.positive_count, 0);
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
    }

    #[test]
    fn bench_vector_parses_rps_table_and_idr() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rust-video-harness/vectors/bench/h265-1080p-nvenc.h265");
        let data = std::fs::read(path).expect("bench vector readable");
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("bench ingest");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].idr);
        let sps = ingest.sps(0).expect("bench sps");
        assert_eq!(sps.rps.len(), 1);
        assert_eq!((sps.visible_width(), sps.visible_height()), (1920, 1080));
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
        assert!(pictures[0].irap);
        assert!(!pictures[0].rasl);
        assert_eq!(pictures[0].poc_lsb(), None);
    }

    #[test]
    fn ingest_assembles_p_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &p_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let picture = &pictures[1];
        assert!(!picture.idr);
        assert!(!picture.irap);
        assert_eq!(picture.poc_lsb(), Some(2));
        let header = &picture.slices[0].header;
        assert_eq!(header.slice_type, SliceType::P);
        let set = header.rps.as_ref().expect("explicit set");
        assert_eq!((set.negative_count, set.positive_count), (1, 0));
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
        let bits = header.rps_bits.expect("explicit provenance");
        assert!(bits.explicit);
        assert!(bits.bits > 0);
    }

    #[test]
    fn ingest_assembles_b_picture() {
        let data = packet(&[
            nal(33, &sps_body()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &b_body()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let picture = &pictures[1];
        assert_eq!(picture.poc_lsb(), Some(1));
        let header = &picture.slices[0].header;
        assert_eq!(header.slice_type, SliceType::B);
        let set = header.rps.as_ref().expect("explicit set");
        assert_eq!((set.negative[0], set.positive[0]), (-1, 1));
        assert!(set.negative_used[0] && set.positive_used[0]);
    }

    #[test]
    fn ingest_cites_rps_table() {
        let data = packet(&[
            nal(33, &sps_body_rps()),
            nal(34, &pps_body()),
            nal(19, &idr_body()),
            nal(1, &p_body_table()),
        ]);
        let mut ingest = Ingest::new();
        let pictures = ingest.ingest_packet(&data).expect("ingest");
        assert_eq!(pictures.len(), 2);
        let header = &pictures[1].slices[0].header;
        let set = header.rps.as_ref().expect("table set");
        assert_eq!(set.negative[0], -2);
        assert!(set.negative_used[0]);
        let bits = header.rps_bits.expect("table provenance");
        assert!(!bits.explicit);
    }

    #[test]
    fn ingest_rejects_garbage() {
        let mut ingest = Ingest::new();
        assert!(ingest
            .ingest_packet(&[0, 0, 0, 1, 0xFF, 0xFF, 0x00])
            .is_err());
    }
}
