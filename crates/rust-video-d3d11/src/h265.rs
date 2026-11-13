//! H.265 Annex B ingest: parameter sets plus slice segment headers.
//!
//! Progressive pictures; the HEVC DPB owns ordering and output.

use crate::bits::BitReader;
use crate::error::{D3d11Error, ParseReason, UnsupportedReason};
use crate::h264::{rbsp, split_nals, Crop};

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

/// Sequence long-term picture count, H.265 7.4.3.2.1.
pub(crate) const MAX_LT_SPS: usize = 32;

/// Tile column ceiling behind the DXVA grid.
pub(crate) const MAX_TILE_COLUMNS: usize = 20;

/// Tile row ceiling behind the DXVA grid.
pub(crate) const MAX_TILE_ROWS: usize = 22;

/// Coded extent ceiling in luma samples, H.265 A.4.1.
pub(crate) const MAX_PIC_SIZE: u32 = 16384;

/// VPS NAL type, H.265 7.4.2.2.
const NAL_VPS: u8 = 32;

/// SPS NAL type.
const NAL_SPS: u8 = 33;

/// PPS NAL type.
const NAL_PPS: u8 = 34;

/// Access unit delimiter NAL type.
const NAL_AUD: u8 = 35;

/// End-of-sequence NAL type.
const NAL_EOS: u8 = 36;

/// End-of-bitstream NAL type.
const NAL_EOB: u8 = 37;

/// Filler NAL type.
const NAL_FD: u8 = 38;

/// Prefix SEI NAL type.
const NAL_PREFIX_SEI: u8 = 39;

/// Suffix SEI NAL type.
const NAL_SUFFIX_SEI: u8 = 40;

/// First VCL NAL type.
const VCL_FIRST: u8 = 0;

/// Last VCL NAL type.
const VCL_LAST: u8 = 31;

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

/// Default intra scaling list in diagonal scan order, H.265 7.4.5.
const DEFAULT_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];

/// Default inter scaling list in diagonal scan order, H.265 7.4.5.
const DEFAULT_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];

/// Scaling lists in coded diagonal scan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScalingLists {
    /// Size id 0: six 4x4 lists.
    pub(crate) size0: [[u8; 16]; 6],
    /// Size id 1: six 8x8 lists.
    pub(crate) size1: [[u8; 64]; 6],
    /// Size id 2: six 8x8 lists.
    pub(crate) size2: [[u8; 64]; 6],
    /// Size id 3: two 8x8 lists.
    pub(crate) size3: [[u8; 64]; 2],
    /// Size id 2 DC coefficients.
    pub(crate) dc2: [u8; 6],
    /// Size id 3 DC coefficients.
    pub(crate) dc3: [u8; 2],
}

impl Default for ScalingLists {
    /// Default lists, H.265 7.4.5 absent-data inference.
    fn default() -> Self {
        Self {
            size0: [[16; 16]; 6],
            size1: [
                DEFAULT_INTRA,
                DEFAULT_INTRA,
                DEFAULT_INTRA,
                DEFAULT_INTER,
                DEFAULT_INTER,
                DEFAULT_INTER,
            ],
            size2: [
                DEFAULT_INTRA,
                DEFAULT_INTRA,
                DEFAULT_INTRA,
                DEFAULT_INTER,
                DEFAULT_INTER,
                DEFAULT_INTER,
            ],
            size3: [DEFAULT_INTRA, DEFAULT_INTER],
            dc2: [16; 6],
            dc3: [16; 2],
        }
    }
}

/// Scaling list data, H.265 7.3.4.
fn parse_scaling_list_data(reader: &mut BitReader) -> Result<ScalingLists, D3d11Error> {
    let mut lists = ScalingLists::default();
    for size_id in 0..4 {
        let matrices = if size_id == 3 { 2 } else { 6 };
        for matrix_id in 0..matrices {
            parse_scaling_matrix(reader, &mut lists, size_id, matrix_id)?;
        }
    }
    Ok(lists)
}

/// One scaling matrix: default, sibling copy, or parsed deltas.
fn parse_scaling_matrix(
    reader: &mut BitReader,
    lists: &mut ScalingLists,
    size_id: usize,
    matrix_id: usize,
) -> Result<(), D3d11Error> {
    if !reader.read_bit("scaling_list_pred_mode_flag")? {
        let delta = reader.read_ue("scaling_list_pred_matrix_id_delta")?;
        if delta == 0 {
            return Ok(());
        }
        let delta = usize::try_from(delta).unwrap_or(usize::MAX);
        let reference = matrix_id.checked_sub(delta).ok_or({
            D3d11Error::Parse(ParseReason::Range {
                syntax: "scaling_list_pred_matrix_id",
            })
        })?;
        copy_scaling_matrix(lists, size_id, matrix_id, reference);
        return Ok(());
    }
    let mut next = 8i32;
    if size_id > 1 {
        let dc = reader.read_se("scaling_list_dc_coef_minus8")?;
        if !(-7..=247).contains(&dc) {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "scaling_list_dc_coef",
            }));
        }
        let dc = u8::try_from(dc + 8).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "scaling_list_dc_coef",
            })
        })?;
        set_scaling_dc(lists, size_id, matrix_id, dc);
        next = i32::from(dc);
    }
    let count = 1usize << (4 + (size_id << 1).min(2));
    for index in 0..count.min(64) {
        let delta = reader.read_se("scaling_list_delta_coef")?;
        let stepped = (i64::from(next) + i64::from(delta)).rem_euclid(256);
        let coef = u8::try_from(stepped).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "scaling_list_delta_coef",
            })
        })?;
        next = i32::from(coef);
        set_scaling_coef(lists, size_id, matrix_id, index, coef);
    }
    Ok(())
}

/// Sibling matrix copied into `matrix_id`.
fn copy_scaling_matrix(
    lists: &mut ScalingLists,
    size_id: usize,
    matrix_id: usize,
    reference: usize,
) {
    match size_id {
        0 => lists.size0[matrix_id] = lists.size0[reference],
        1 => lists.size1[matrix_id] = lists.size1[reference],
        2 => {
            lists.size2[matrix_id] = lists.size2[reference];
            lists.dc2[matrix_id] = lists.dc2[reference];
        }
        _ => {
            lists.size3[matrix_id] = lists.size3[reference];
            lists.dc3[matrix_id] = lists.dc3[reference];
        }
    }
}

/// DC coefficient stored at `matrix_id`.
fn set_scaling_dc(lists: &mut ScalingLists, size_id: usize, matrix_id: usize, dc: u8) {
    if size_id == 2 {
        lists.dc2[matrix_id] = dc;
    } else {
        lists.dc3[matrix_id] = dc;
    }
}

/// Coefficient stored at `index` of `matrix_id`.
fn set_scaling_coef(
    lists: &mut ScalingLists,
    size_id: usize,
    matrix_id: usize,
    index: usize,
    coef: u8,
) {
    match size_id {
        0 => lists.size0[matrix_id][index] = coef,
        1 => lists.size1[matrix_id][index] = coef,
        2 => lists.size2[matrix_id][index] = coef,
        _ => lists.size3[matrix_id][index] = coef,
    }
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
) -> Result<ResolvedRps, D3d11Error> {
    let predicted = index != 0 && reader.read_bit("inter_ref_pic_set_prediction_flag")?;
    if !predicted {
        return Ok(ResolvedRps {
            set: parse_explicit_rps(reader)?,
            ref_num_delta_pocs: 0,
        });
    }
    let ref_idx = predict_source(reader, index, num_sps)?;
    let reference = table.get(ref_idx).ok_or({
        D3d11Error::Parse(ParseReason::Range {
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
) -> Result<usize, D3d11Error> {
    let delta = if index == num_sps {
        reader.read_ue("delta_idx_minus1")?
    } else {
        0
    };
    let delta = usize::try_from(delta).unwrap_or(usize::MAX);
    if delta >= index {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "inter_ref_pic_set_prediction",
        }));
    }
    Ok(index - delta - 1)
}

/// Delta RPS value behind prediction.
fn parse_delta_rps(reader: &mut BitReader) -> Result<i32, D3d11Error> {
    let sign = reader.read_bit("delta_rps_sign")?;
    let abs = reader.read_ue("abs_delta_rps_minus1")?;
    if abs > 32767 {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "abs_delta_rps_minus1",
        }));
    }
    let magnitude = i32::try_from(abs + 1).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "abs_delta_rps_minus1",
        })
    })?;
    Ok(if sign { -magnitude } else { magnitude })
}

/// Per-candidate used and delta flags behind prediction.
fn parse_predict_flags(
    reader: &mut BitReader,
    num_delta_pocs: usize,
) -> Result<Vec<(bool, bool)>, D3d11Error> {
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
) -> Result<ShortTermRps, D3d11Error> {
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
) -> Result<(), D3d11Error> {
    let (used, keep) = flag;
    if !keep {
        return Ok(());
    }
    let poc = delta.checked_add(shift).ok_or({
        D3d11Error::Parse(ParseReason::Range {
            syntax: "delta_poc",
        })
    })?;
    if negative && poc >= 0 || !negative && poc <= 0 {
        return Ok(());
    }
    if set.num_delta_pocs() >= MAX_RESOLVED_RPS {
        return Err(D3d11Error::Parse(ParseReason::ShortTermRefPicSetSize));
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
fn parse_explicit_rps(reader: &mut BitReader) -> Result<ShortTermRps, D3d11Error> {
    let negative = parse_rps_count(reader, "num_negative_pics")?;
    let positive = parse_rps_count(reader, "num_positive_pics")?;
    if usize::from(negative) + usize::from(positive) >= MAX_RPS_PICS {
        return Err(D3d11Error::Parse(ParseReason::ShortTermRefPicSetSize));
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
fn parse_rps_count(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d11Error> {
    let value = reader.read_ue(syntax)?;
    if usize::try_from(value).is_ok_and(|count| count >= MAX_RPS_PICS) {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Next explicit delta POC from a minus-one step.
fn parse_rps_step(
    reader: &mut BitReader,
    poc: i32,
    positive: bool,
    syntax: &'static str,
) -> Result<i32, D3d11Error> {
    let step = reader.read_ue(syntax)?;
    if step > 32767 {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    let step =
        i32::try_from(step + 1).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))?;
    if positive {
        poc.checked_add(step)
    } else {
        poc.checked_sub(step)
    }
    .ok_or({
        D3d11Error::Parse(ParseReason::Range {
            syntax: "delta_poc",
        })
    })
}

/// Temporal nesting flag bit.
pub(crate) const SPS_NESTING: u16 = 1;
/// Sublayer ordering info flag bit.
pub(crate) const SPS_ORDERING_INFO: u16 = 1 << 1;
/// Scaling lists enabled flag bit.
pub(crate) const SPS_SCALING: u16 = 1 << 2;
/// Asymmetric motion partitions flag bit.
pub(crate) const SPS_AMP: u16 = 1 << 3;
/// Sample adaptive offset flag bit.
pub(crate) const SPS_SAO: u16 = 1 << 4;
/// Pulse code modulation flag bit.
pub(crate) const SPS_PCM: u16 = 1 << 5;
/// PCM loop filter disabled flag bit.
pub(crate) const SPS_PCM_LOOP_DISABLED: u16 = 1 << 6;
/// Long-term pictures present flag bit.
pub(crate) const SPS_LONG_TERM: u16 = 1 << 7;
/// Temporal motion vectors flag bit.
pub(crate) const SPS_TEMPORAL_MVP: u16 = 1 << 8;
/// Strong intra smoothing flag bit.
pub(crate) const SPS_SMOOTHING: u16 = 1 << 9;

/// Parsed sequence parameter set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Sps {
    /// Set id.
    pub(crate) id: u8,
    /// Packed tool flags.
    pub(crate) flags: u16,
    /// Sublayer count.
    pub(crate) max_sub_layers: u8,
    /// Coded video sequence level.
    pub(crate) level_idc: u8,
    /// Coded width in luma samples.
    pub(crate) width: u32,
    /// Coded height in luma samples.
    pub(crate) height: u32,
    /// Visible rectangle, absent when uncropped.
    pub(crate) crop: Option<Crop>,
    /// POC LSB width minus 4.
    pub(crate) log2_max_poc_lsb_minus4: u8,
    /// Maximum buffered pictures minus one, top sublayer.
    pub(crate) max_dec_pic_buffering_minus1: u8,
    /// Maximum reorder pictures over sublayers.
    pub(crate) max_num_reorder_pics: u8,
    /// Minimum luma block size minus 3.
    pub(crate) log2_min_cb_size_minus3: u8,
    /// Luma block size range.
    pub(crate) log2_diff_max_min_cb_size: u8,
    /// Minimum transform size minus 2.
    pub(crate) log2_min_tb_size_minus2: u8,
    /// Transform size range.
    pub(crate) log2_diff_max_min_tb_size: u8,
    /// Maximum inter transform depth.
    pub(crate) max_transform_depth_inter: u8,
    /// Maximum intra transform depth.
    pub(crate) max_transform_depth_intra: u8,
    /// Sequence scaling lists.
    pub(crate) scaling: ScalingLists,
    /// PCM luma depth minus one.
    pub(crate) pcm_depth_luma: u8,
    /// PCM chroma depth minus one.
    pub(crate) pcm_depth_chroma: u8,
    /// PCM minimum block size minus 3.
    pub(crate) pcm_log2_min_minus3: u8,
    /// PCM block size range.
    pub(crate) pcm_log2_diff: u8,
    /// Short-term reference set table.
    pub(crate) rps: Vec<ShortTermRps>,
    /// Sequence long-term POC LSBs.
    pub(crate) lt_poc_lsb: Vec<u32>,
    /// Sequence long-term used flags.
    pub(crate) lt_used_by_curr: Vec<bool>,
}

impl Sps {
    /// Scaling lists enabled.
    pub(crate) const fn scaling_enabled(&self) -> bool {
        self.flags & SPS_SCALING != 0
    }

    /// Asymmetric motion partitions.
    pub(crate) const fn amp(&self) -> bool {
        self.flags & SPS_AMP != 0
    }

    /// Sample adaptive offset.
    pub(crate) const fn sao(&self) -> bool {
        self.flags & SPS_SAO != 0
    }

    /// Pulse code modulation.
    pub(crate) const fn pcm(&self) -> bool {
        self.flags & SPS_PCM != 0
    }

    /// PCM loop filter disabled.
    pub(crate) const fn pcm_loop_disabled(&self) -> bool {
        self.flags & SPS_PCM_LOOP_DISABLED != 0
    }

    /// Long-term pictures present.
    pub(crate) const fn long_term_present(&self) -> bool {
        self.flags & SPS_LONG_TERM != 0
    }

    /// Temporal motion vectors.
    pub(crate) const fn temporal_mvp(&self) -> bool {
        self.flags & SPS_TEMPORAL_MVP != 0
    }

    /// Strong intra smoothing.
    pub(crate) const fn smoothing(&self) -> bool {
        self.flags & SPS_SMOOTHING != 0
    }

    /// Maximum POC LSB.
    pub(crate) const fn max_poc_lsb(&self) -> u32 {
        1 << (self.log2_max_poc_lsb_minus4 + 4)
    }

    /// Minimum luma block width in samples.
    pub(crate) const fn min_cb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3)
    }

    /// Luma coding tree block width in samples.
    pub(crate) const fn ctb_size(&self) -> u32 {
        1 << (self.log2_min_cb_size_minus3 + 3 + self.log2_diff_max_min_cb_size)
    }

    /// Picture width in minimum coding blocks.
    pub(crate) fn width_in_min_cbs(&self) -> u32 {
        self.width.div_ceil(self.min_cb_size())
    }

    /// Picture height in minimum coding blocks.
    pub(crate) fn height_in_min_cbs(&self) -> u32 {
        self.height.div_ceil(self.min_cb_size())
    }

    /// Luma coding tree block columns.
    pub(crate) fn width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(self.ctb_size())
    }

    /// Luma coding tree block rows.
    pub(crate) fn height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(self.ctb_size())
    }

    /// Visible width in luma samples.
    pub(crate) fn visible_width(&self) -> u32 {
        self.crop
            .map_or(self.width, |crop| self.width - crop.left - crop.right)
    }

    /// Visible height in luma samples.
    pub(crate) fn visible_height(&self) -> u32 {
        self.crop
            .map_or(self.height, |crop| self.height - crop.top - crop.bottom)
    }
}

/// Sequence parameter set from RBSP `rbsp`.
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, D3d11Error> {
    let mut reader = BitReader::new(rbsp);
    reader.read_bits(4, "sps_video_parameter_set_id")?;
    let max_sub_layers = parse_sub_layers(&mut reader)?;
    let mut flags = 0u16;
    if reader.read_bit("sps_temporal_id_nesting_flag")? {
        flags |= SPS_NESTING;
    }
    let level_idc = parse_profile_tier_level(&mut reader, max_sub_layers)?;
    let id = parse_sps_id(&mut reader)?;
    parse_chroma(&mut reader)?;
    let (width, height) = parse_pic_size(&mut reader)?;
    let crop = parse_conformance(&mut reader, width, height)?;
    parse_depths(&mut reader)?;
    let log2_max_poc_lsb_minus4 = parse_log2(&mut reader, "log2_max_pic_order_cnt_lsb_minus4")?;
    let (buffering, reorder) = parse_ordering(&mut reader, max_sub_layers, &mut flags)?;
    let blocks = parse_block_sizes(&mut reader)?;
    let scaling = parse_sps_scaling(&mut reader, &mut flags)?;
    if reader.read_bit("amp_enabled_flag")? {
        flags |= SPS_AMP;
    }
    if reader.read_bit("sample_adaptive_offset_enabled_flag")? {
        flags |= SPS_SAO;
    }
    let pcm = parse_pcm(&mut reader, &mut flags)?;
    let rps = parse_rps_table(&mut reader)?;
    let (lt_poc_lsb, lt_used_by_curr) =
        parse_long_term_sps(&mut reader, log2_max_poc_lsb_minus4, &mut flags)?;
    if reader.read_bit("sps_temporal_mvp_enabled_flag")? {
        flags |= SPS_TEMPORAL_MVP;
    }
    if reader.read_bit("strong_intra_smoothing_enabled_flag")? {
        flags |= SPS_SMOOTHING;
    }
    if reader.read_bit("vui_parameters_present_flag")? && parse_vui(&mut reader, max_sub_layers)? {
        return Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced));
    }
    parse_sps_extension(&mut reader)?;
    Ok(Sps {
        id,
        flags,
        max_sub_layers,
        level_idc,
        width,
        height,
        crop,
        log2_max_poc_lsb_minus4,
        max_dec_pic_buffering_minus1: buffering,
        max_num_reorder_pics: reorder,
        log2_min_cb_size_minus3: blocks.0,
        log2_diff_max_min_cb_size: blocks.1,
        log2_min_tb_size_minus2: blocks.2,
        log2_diff_max_min_tb_size: blocks.3,
        max_transform_depth_inter: blocks.4,
        max_transform_depth_intra: blocks.5,
        scaling,
        pcm_depth_luma: pcm.0,
        pcm_depth_chroma: pcm.1,
        pcm_log2_min_minus3: pcm.2,
        pcm_log2_diff: pcm.3,
        rps,
        lt_poc_lsb,
        lt_used_by_curr,
    })
}

/// Sublayer count from the minus-one field.
fn parse_sub_layers(reader: &mut BitReader) -> Result<u8, D3d11Error> {
    let minus1 = reader.read_bits(3, "sps_max_sub_layers_minus1")?;
    u8::try_from(minus1 + 1).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "sub layers",
        })
    })
}

/// Set id below the set count.
fn parse_sps_id(reader: &mut BitReader) -> Result<u8, D3d11Error> {
    let id = reader.read_ue("seq_parameter_set_id")?;
    let id = u8::try_from(id).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "seq_parameter_set_id",
        })
    })?;
    if usize::from(id) >= MAX_SPS {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "seq_parameter_set_id",
        }));
    }
    Ok(id)
}

/// Chroma format gated to 4:2:0.
fn parse_chroma(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if reader.read_ue("chroma_format_idc")? != 1 {
        return Err(D3d11Error::Unsupported(UnsupportedReason::ChromaOutside420));
    }
    Ok(())
}

/// Coded picture size in luma samples.
fn parse_pic_size(reader: &mut BitReader) -> Result<(u32, u32), D3d11Error> {
    let width = reader.read_ue("pic_width_in_luma_samples")?;
    let height = reader.read_ue("pic_height_in_luma_samples")?;
    if width == 0 || height == 0 || width > MAX_PIC_SIZE || height > MAX_PIC_SIZE {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax: "pic size" }));
    }
    Ok((width, height))
}

/// Conformance crop rectangle in luma samples.
fn parse_conformance(
    reader: &mut BitReader,
    width: u32,
    height: u32,
) -> Result<Option<Crop>, D3d11Error> {
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
        _ => Err(D3d11Error::Parse(
            ParseReason::ConformanceWindowOutsideCodedSize,
        )),
    }
}

/// Bit depths gated to 8 bit.
fn parse_depths(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if reader.read_ue("bit_depth_luma_minus8")? != 0 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::LumaDepthAbove8Bit,
        ));
    }
    if reader.read_ue("bit_depth_chroma_minus8")? != 0 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::ChromaDepthAbove8Bit,
        ));
    }
    Ok(())
}

/// Log2 width field below the 12 ceiling.
fn parse_log2(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d11Error> {
    let value = reader.read_ue(syntax)?;
    if value > 12 {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Buffering and reorder limits over sublayers.
fn parse_ordering(
    reader: &mut BitReader,
    max_sub_layers: u8,
    flags: &mut u16,
) -> Result<(u8, u8), D3d11Error> {
    if reader.read_bit("sps_sub_layer_ordering_info_present_flag")? {
        *flags |= SPS_ORDERING_INFO;
    }
    let first = if *flags & SPS_ORDERING_INFO != 0 {
        0
    } else {
        max_sub_layers - 1
    };
    let mut buffering = 0u8;
    let mut reorder = 0u8;
    for _ in first..max_sub_layers {
        let buffered = reader.read_ue("sps_max_dec_pic_buffering_minus1")?;
        if buffered > 15 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "sps_max_dec_pic_buffering_minus1",
            }));
        }
        buffering = u8::try_from(buffered).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "sps_max_dec_pic_buffering_minus1",
            })
        })?;
        let reordered = reader.read_ue("sps_max_num_reorder_pics")?;
        if reordered > 16 {
            return Err(D3d11Error::Parse(ParseReason::Range {
                syntax: "sps_max_num_reorder_pics",
            }));
        }
        reorder = reorder.max(u8::try_from(reordered).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "sps_max_num_reorder_pics",
            })
        })?);
        reader.read_ue("sps_max_latency_increase_plus1")?;
    }
    Ok((buffering, reorder))
}

/// Luma block sizes plus transform depths.
fn parse_block_sizes(reader: &mut BitReader) -> Result<(u8, u8, u8, u8, u8, u8), D3d11Error> {
    let cb_min = parse_bounded_ue(reader, "log2_min_luma_coding_block_size_minus3", 3)?;
    let cb_range = parse_bounded_ue(reader, "log2_diff_max_min_luma_coding_block_size", 3)?;
    let tb_min = parse_bounded_ue(reader, "log2_min_luma_transform_block_size_minus2", 2)?;
    let tb_range = parse_bounded_ue(reader, "log2_diff_max_min_luma_transform_block_size", 3)?;
    let depth_inter = parse_bounded_ue(reader, "max_transform_hierarchy_depth_inter", 7)?;
    let depth_intra = parse_bounded_ue(reader, "max_transform_hierarchy_depth_intra", 7)?;
    Ok((cb_min, cb_range, tb_min, tb_range, depth_inter, depth_intra))
}

/// Unsigned field below `ceiling`.
fn parse_bounded_ue(
    reader: &mut BitReader,
    syntax: &'static str,
    ceiling: u32,
) -> Result<u8, D3d11Error> {
    let value = reader.read_ue(syntax)?;
    if value > ceiling {
        return Err(D3d11Error::Parse(ParseReason::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Sequence scaling lists, flat without data.
fn parse_sps_scaling(reader: &mut BitReader, flags: &mut u16) -> Result<ScalingLists, D3d11Error> {
    if !reader.read_bit("scaling_list_enabled_flag")? {
        return Ok(ScalingLists::default());
    }
    *flags |= SPS_SCALING;
    if reader.read_bit("sps_scaling_list_data_present_flag")? {
        return parse_scaling_list_data(reader);
    }
    Ok(ScalingLists::default())
}

/// PCM parameters, zeroed without PCM.
fn parse_pcm(reader: &mut BitReader, flags: &mut u16) -> Result<(u8, u8, u8, u8), D3d11Error> {
    if !reader.read_bit("pcm_enabled_flag")? {
        return Ok((0, 0, 0, 0));
    }
    *flags |= SPS_PCM;
    let luma = reader.read_bits(4, "pcm_sample_bit_depth_luma_minus1")?;
    let chroma = reader.read_bits(4, "pcm_sample_bit_depth_chroma_minus1")?;
    if luma > 7 || chroma > 7 {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::PcmDepthAbove8Bit,
        ));
    }
    let min = parse_bounded_ue(reader, "log2_min_pcm_luma_coding_block_size_minus3", 2)?;
    let diff = parse_bounded_ue(reader, "log2_diff_max_min_pcm_luma_coding_block_size", 2)?;
    if reader.read_bit("pcm_loop_filter_disabled_flag")? {
        *flags |= SPS_PCM_LOOP_DISABLED;
    }
    let luma = u8::try_from(luma).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "pcm depth",
        })
    })?;
    let chroma = u8::try_from(chroma).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "pcm depth",
        })
    })?;
    Ok((luma, chroma, min, diff))
}

/// Short-term reference set table.
fn parse_rps_table(reader: &mut BitReader) -> Result<Vec<ShortTermRps>, D3d11Error> {
    let count = reader.read_ue("num_short_term_ref_pic_sets")?;
    if usize::try_from(count).is_ok_and(|count| count > MAX_RPS) {
        return Err(D3d11Error::Parse(ParseReason::Range {
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

/// Sequence long-term pictures.
fn parse_long_term_sps(
    reader: &mut BitReader,
    log2_max_poc_lsb_minus4: u8,
    flags: &mut u16,
) -> Result<(Vec<u32>, Vec<bool>), D3d11Error> {
    if !reader.read_bit("long_term_ref_pics_present_flag")? {
        return Ok((Vec::new(), Vec::new()));
    }
    *flags |= SPS_LONG_TERM;
    let count = reader.read_ue("num_long_term_ref_pics_sps")?;
    if usize::try_from(count).is_ok_and(|count| count > MAX_LT_SPS) {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "num_long_term_ref_pics_sps",
        }));
    }
    let width = u32::from(log2_max_poc_lsb_minus4) + 4;
    let mut poc_lsb = Vec::new();
    let mut used = Vec::new();
    for _ in 0..count {
        poc_lsb.push(reader.read_bits(width, "lt_ref_pic_poc_lsb_sps")?);
        used.push(reader.read_bit("used_by_curr_pic_lt_sps_flag")?);
    }
    Ok((poc_lsb, used))
}

/// Sequence extension flags, declining any extension.
fn parse_sps_extension(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("sps_extension_present_flag")? {
        return Ok(());
    }
    let mut extended = false;
    for syntax in [
        "sps_range_extension_flag",
        "sps_multilayer_extension_flag",
        "sps_3d_extension_flag",
        "sps_scc_extension_flag",
    ] {
        extended |= reader.read_bit(syntax)?;
    }
    reader.read_bits(4, "sps_extension_4bits")?;
    if extended {
        return Err(D3d11Error::Unsupported(UnsupportedReason::SpsExtension));
    }
    Ok(())
}

/// Profile, tier, and level, returning the level idc, H.265 7.3.3.
fn parse_profile_tier_level(reader: &mut BitReader, max_sub_layers: u8) -> Result<u8, D3d11Error> {
    parse_ptl_layer(reader)?;
    let level_idc = reader.read_bits(8, "general_level_idc")?;
    let mut profile_present = Vec::new();
    let mut level_present = Vec::new();
    for _ in 1..max_sub_layers {
        profile_present.push(reader.read_bit("sub_layer_profile_present_flag")?);
        level_present.push(reader.read_bit("sub_layer_level_present_flag")?);
    }
    if max_sub_layers > 1 {
        for _ in max_sub_layers..9 {
            reader.read_bits(2, "reserved_zero_2bits")?;
        }
    }
    for index in 0..profile_present.len() {
        if profile_present[index] {
            parse_ptl_layer(reader)?;
        }
        if level_present[index] {
            reader.read_bits(8, "sub_layer_level_idc")?;
        }
    }
    u8::try_from(level_idc).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "general_level_idc",
        })
    })
}

/// One profile-tier layer: compatibility plus constraint flags.
fn parse_ptl_layer(reader: &mut BitReader) -> Result<(), D3d11Error> {
    reader.read_bits(2, "general_profile_space")?;
    reader.read_bit("general_tier_flag")?;
    let idc = reader.read_bits(5, "general_profile_idc")?;
    let mut compat = [false; 32];
    for flag in &mut compat {
        *flag = reader.read_bit("general_profile_compatibility_flag")?;
    }
    let progressive = reader.read_bit("general_progressive_source_flag")?;
    let interlaced = reader.read_bit("general_interlaced_source_flag")?;
    reader.read_bit("general_non_packed_constraint_flag")?;
    reader.read_bit("general_frame_only_constraint_flag")?;
    if interlaced && !progressive {
        return Err(D3d11Error::Unsupported(UnsupportedReason::Interlaced));
    }
    let idc = usize::try_from(idc).unwrap_or(usize::MAX);
    parse_ptl_constraints(reader, idc, &compat)
}

/// Constraint flags for profile `idc` with compatibility `compat`.
fn parse_ptl_constraints(
    reader: &mut BitReader,
    idc: usize,
    compat: &[bool; 32],
) -> Result<(), D3d11Error> {
    let matches = |profile: usize| idc == profile || compat[profile];
    if [4, 5, 6, 7, 8, 9, 10, 11]
        .iter()
        .any(|profile| matches(*profile))
    {
        for _ in 0..9 {
            reader.read_bit("general_constraint_flag")?;
        }
        if [5, 9, 10, 11].iter().any(|profile| matches(*profile)) {
            reader.read_bit("general_max_14bit_constraint_flag")?;
            reader.read_bits(24, "general_reserved_zero_33bits")?;
            reader.read_bits(9, "general_reserved_zero_33bits")?;
        } else {
            reader.read_bits(24, "general_reserved_zero_34bits")?;
            reader.read_bits(10, "general_reserved_zero_34bits")?;
        }
    } else if matches(2) {
        reader.read_bits(7, "general_reserved_zero_7bits")?;
        reader.read_bit("general_one_picture_only_constraint_flag")?;
        reader.read_bits(24, "general_reserved_zero_35bits")?;
        reader.read_bits(11, "general_reserved_zero_35bits")?;
    } else {
        reader.read_bits(24, "general_reserved_zero_43bits")?;
        reader.read_bits(19, "general_reserved_zero_43bits")?;
    }
    if [1, 2, 3, 4, 5, 9, 11]
        .iter()
        .any(|profile| matches(*profile))
    {
        reader.read_bit("general_inbld_flag")?;
    } else {
        reader.read_bits(1, "general_reserved_zero_bit")?;
    }
    Ok(())
}

/// Video usability information, returning the field sequence flag.
fn parse_vui(reader: &mut BitReader, max_sub_layers: u8) -> Result<bool, D3d11Error> {
    parse_vui_aspect(reader)?;
    parse_vui_overscan(reader)?;
    parse_vui_signal(reader)?;
    parse_vui_chroma_loc(reader)?;
    reader.read_bit("neutral_chroma_indication_flag")?;
    let field_seq = reader.read_bit("field_seq_flag")?;
    reader.read_bit("frame_field_info_present_flag")?;
    parse_vui_window(reader)?;
    parse_vui_timing(reader, max_sub_layers)?;
    parse_vui_restriction(reader)?;
    Ok(field_seq)
}

/// VUI aspect ratio section.
fn parse_vui_aspect(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("aspect_ratio_info_present_flag")? {
        return Ok(());
    }
    if reader.read_bits(8, "aspect_ratio_idc")? == 255 {
        reader.read_bits(16, "sar_width")?;
        reader.read_bits(16, "sar_height")?;
    }
    Ok(())
}

/// VUI overscan section.
fn parse_vui_overscan(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if reader.read_bit("overscan_info_present_flag")? {
        reader.read_bit("overscan_appropriate_flag")?;
    }
    Ok(())
}

/// VUI video signal section.
fn parse_vui_signal(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("video_signal_type_present_flag")? {
        return Ok(());
    }
    reader.read_bits(3, "video_format")?;
    reader.read_bit("video_full_range_flag")?;
    if reader.read_bit("colour_description_present_flag")? {
        reader.read_bits(8, "colour_primaries")?;
        reader.read_bits(8, "transfer_characteristics")?;
        reader.read_bits(8, "matrix_coefficients")?;
    }
    Ok(())
}

/// VUI chroma location section.
fn parse_vui_chroma_loc(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("chroma_loc_info_present_flag")? {
        return Ok(());
    }
    reader.read_ue("chroma_sample_loc_type_top_field")?;
    reader.read_ue("chroma_sample_loc_type_bottom_field")?;
    Ok(())
}

/// VUI default display window section.
fn parse_vui_window(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("default_display_window_flag")? {
        return Ok(());
    }
    for syntax in [
        "def_disp_win_left_offset",
        "def_disp_win_right_offset",
        "def_disp_win_top_offset",
        "def_disp_win_bottom_offset",
    ] {
        reader.read_ue(syntax)?;
    }
    Ok(())
}

/// VUI timing and hypothetical reference decoder section.
fn parse_vui_timing(reader: &mut BitReader, max_sub_layers: u8) -> Result<(), D3d11Error> {
    if !reader.read_bit("vui_timing_info_present_flag")? {
        return Ok(());
    }
    reader.read_bits(32, "vui_num_units_in_tick")?;
    reader.read_bits(32, "vui_time_scale")?;
    if reader.read_bit("vui_poc_proportional_to_timing_flag")? {
        reader.read_ue("vui_num_ticks_poc_diff_one_minus1")?;
    }
    if reader.read_bit("vui_hrd_parameters_present_flag")? {
        parse_hrd(reader, max_sub_layers)?;
    }
    Ok(())
}

/// VUI bitstream restriction section.
fn parse_vui_restriction(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("bitstream_restriction_flag")? {
        return Ok(());
    }
    reader.read_bit("tiles_fixed_structure_flag")?;
    reader.read_bit("motion_vectors_over_pic_boundaries_flag")?;
    reader.read_bit("restricted_ref_pic_lists_flag")?;
    for syntax in [
        "min_spatial_segmentation_idc",
        "max_bytes_per_pic_denom",
        "max_bits_per_min_cu_denom",
        "log2_max_mv_length_horizontal",
        "log2_max_mv_length_vertical",
    ] {
        reader.read_ue(syntax)?;
    }
    Ok(())
}

/// Hypothetical reference decoder parameters, H.265 E.2.2.
fn parse_hrd(reader: &mut BitReader, max_sub_layers: u8) -> Result<(), D3d11Error> {
    let nal = reader.read_bit("nal_hrd_parameters_present_flag")?;
    let vcl = reader.read_bit("vcl_hrd_parameters_present_flag")?;
    let mut sub_pic = false;
    if nal || vcl {
        sub_pic = parse_hrd_common(reader)?;
    }
    for _ in 0..max_sub_layers {
        let count = parse_hrd_layer(reader)?;
        for _ in 0..=count {
            if nal {
                parse_hrd_sublayer(reader, sub_pic)?;
            }
            if vcl {
                parse_hrd_sublayer(reader, sub_pic)?;
            }
        }
    }
    Ok(())
}

/// HRD common information, returning the sub-picture flag.
fn parse_hrd_common(reader: &mut BitReader) -> Result<bool, D3d11Error> {
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
fn parse_hrd_layer(reader: &mut BitReader) -> Result<u32, D3d11Error> {
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
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "cpb_cnt_minus1",
        }));
    }
    Ok(count)
}

/// One HRD coded picture buffer description.
fn parse_hrd_sublayer(reader: &mut BitReader, sub_pic: bool) -> Result<(), D3d11Error> {
    reader.read_ue("bit_rate_value_minus1")?;
    reader.read_ue("cpb_size_value_minus1")?;
    if sub_pic {
        reader.read_ue("cpb_size_du_value_minus1")?;
        reader.read_ue("bit_rate_du_value_minus1")?;
    }
    reader.read_bit("cbr_flag")?;
    Ok(())
}

/// Dependent slice segments flag bit.
pub(crate) const PPS_DEPENDENT: u32 = 1;
/// Output flag present flag bit.
pub(crate) const PPS_OUTPUT_FLAG: u32 = 1 << 1;
/// Sign data hiding flag bit.
pub(crate) const PPS_SIGN_HIDING: u32 = 1 << 2;
/// CABAC init present flag bit.
pub(crate) const PPS_CABAC_INIT: u32 = 1 << 3;
/// Constrained intra prediction flag bit.
pub(crate) const PPS_CONSTRAINED_INTRA: u32 = 1 << 4;
/// Transform skip flag bit.
pub(crate) const PPS_TRANSFORM_SKIP: u32 = 1 << 5;
/// CU QP delta flag bit.
pub(crate) const PPS_CU_QP_DELTA: u32 = 1 << 6;
/// Chroma QP offsets present flag bit.
pub(crate) const PPS_CHROMA_OFFSETS: u32 = 1 << 7;
/// Weighted prediction flag bit.
pub(crate) const PPS_WEIGHTED_PRED: u32 = 1 << 8;
/// Weighted biprediction flag bit.
pub(crate) const PPS_WEIGHTED_BIPRED: u32 = 1 << 9;
/// Transquant bypass flag bit.
pub(crate) const PPS_TRANSQUANT_BYPASS: u32 = 1 << 10;
/// Tiles enabled flag bit.
pub(crate) const PPS_TILES: u32 = 1 << 11;
/// Uniform tile spacing flag bit.
pub(crate) const PPS_UNIFORM_SPACING: u32 = 1 << 12;
/// Loop filter across tiles flag bit.
pub(crate) const PPS_LOOP_ACROSS_TILES: u32 = 1 << 13;
/// Wavefront sync flag bit.
pub(crate) const PPS_SYNC: u32 = 1 << 14;
/// Loop filter across slices flag bit.
pub(crate) const PPS_LOOP_ACROSS_SLICES: u32 = 1 << 15;
/// Deblocking override flag bit.
pub(crate) const PPS_DEBLOCK_OVERRIDE: u32 = 1 << 16;
/// Deblocking disabled flag bit.
pub(crate) const PPS_DEBLOCK_DISABLED: u32 = 1 << 17;
/// Scaling lists present flag bit.
pub(crate) const PPS_SCALING: u32 = 1 << 18;
/// Reference list modification flag bit.
pub(crate) const PPS_LISTS_MOD: u32 = 1 << 19;
/// Slice extension present flag bit.
pub(crate) const PPS_SLICE_EXT: u32 = 1 << 20;

/// Parsed picture parameter set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Pps {
    /// Set id.
    pub(crate) id: u8,
    /// Sequence set id.
    pub(crate) sps_id: u8,
    /// Packed tool flags.
    pub(crate) flags: u32,
    /// Reserved slice header bit count.
    pub(crate) num_extra_slice_header_bits: u8,
    /// Default L0 reference count minus one.
    pub(crate) num_ref_l0: u8,
    /// Default L1 reference count minus one.
    pub(crate) num_ref_l1: u8,
    /// Luma QP init delta.
    pub(crate) pic_init_qp_minus26: i8,
    /// CU QP delta depth.
    pub(crate) diff_cu_qp_delta_depth: u8,
    /// Cb QP offset.
    pub(crate) cb_qp_offset: i8,
    /// Cr QP offset.
    pub(crate) cr_qp_offset: i8,
    /// Tile column count.
    pub(crate) num_tile_columns: u8,
    /// Tile row count.
    pub(crate) num_tile_rows: u8,
    /// Tile column widths in treeblocks, derived last.
    pub(crate) column_width: Vec<u16>,
    /// Tile row heights in treeblocks, derived last.
    pub(crate) row_height: Vec<u16>,
    /// Deblocking beta offset over two.
    pub(crate) beta_offset_div2: i8,
    /// Deblocking tc offset over two.
    pub(crate) tc_offset_div2: i8,
    /// Resolved picture scaling lists.
    pub(crate) scaling: ScalingLists,
    /// Parallel merge level minus 2.
    pub(crate) log2_parallel_merge_minus2: u8,
}

impl Pps {
    /// Dependent slice segments.
    pub(crate) const fn dependent_slices(&self) -> bool {
        self.flags & PPS_DEPENDENT != 0
    }

    /// Output flag present in slices.
    pub(crate) const fn output_flag_present(&self) -> bool {
        self.flags & PPS_OUTPUT_FLAG != 0
    }

    /// Sign data hiding.
    pub(crate) const fn sign_hiding(&self) -> bool {
        self.flags & PPS_SIGN_HIDING != 0
    }

    /// CABAC init present in slices.
    pub(crate) const fn cabac_init_present(&self) -> bool {
        self.flags & PPS_CABAC_INIT != 0
    }

    /// Constrained intra prediction.
    pub(crate) const fn constrained_intra(&self) -> bool {
        self.flags & PPS_CONSTRAINED_INTRA != 0
    }

    /// Transform skip.
    pub(crate) const fn transform_skip(&self) -> bool {
        self.flags & PPS_TRANSFORM_SKIP != 0
    }

    /// CU QP deltas.
    pub(crate) const fn cu_qp_delta(&self) -> bool {
        self.flags & PPS_CU_QP_DELTA != 0
    }

    /// Slice chroma QP offsets.
    pub(crate) const fn chroma_offsets_present(&self) -> bool {
        self.flags & PPS_CHROMA_OFFSETS != 0
    }

    /// Weighted prediction.
    pub(crate) const fn weighted_pred(&self) -> bool {
        self.flags & PPS_WEIGHTED_PRED != 0
    }

    /// Weighted biprediction.
    pub(crate) const fn weighted_bipred(&self) -> bool {
        self.flags & PPS_WEIGHTED_BIPRED != 0
    }

    /// Transquant bypass.
    pub(crate) const fn transquant_bypass(&self) -> bool {
        self.flags & PPS_TRANSQUANT_BYPASS != 0
    }

    /// Tiles.
    pub(crate) const fn tiles_enabled(&self) -> bool {
        self.flags & PPS_TILES != 0
    }

    /// Uniform tile spacing.
    pub(crate) const fn uniform_spacing(&self) -> bool {
        self.flags & PPS_UNIFORM_SPACING != 0
    }

    /// Loop filter across tiles.
    pub(crate) const fn loop_across_tiles(&self) -> bool {
        self.flags & PPS_LOOP_ACROSS_TILES != 0
    }

    /// Wavefront sync.
    pub(crate) const fn entropy_sync(&self) -> bool {
        self.flags & PPS_SYNC != 0
    }

    /// Loop filter across slices.
    pub(crate) const fn loop_across_slices(&self) -> bool {
        self.flags & PPS_LOOP_ACROSS_SLICES != 0
    }

    /// Deblocking override.
    pub(crate) const fn deblocking_override(&self) -> bool {
        self.flags & PPS_DEBLOCK_OVERRIDE != 0
    }

    /// Deblocking disabled.
    pub(crate) const fn deblocking_disabled(&self) -> bool {
        self.flags & PPS_DEBLOCK_DISABLED != 0
    }

    /// Reference list modification.
    pub(crate) const fn lists_modification_present(&self) -> bool {
        self.flags & PPS_LISTS_MOD != 0
    }

    /// Slice extension present.
    pub(crate) const fn slice_ext_present(&self) -> bool {
        self.flags & PPS_SLICE_EXT != 0
    }
}

/// Picture parameter set from RBSP `rbsp` against sequence set `sps`.
pub(crate) fn parse_pps(rbsp: &[u8], sps: &Sps) -> Result<Pps, D3d11Error> {
    let mut reader = BitReader::new(rbsp);
    let id = parse_pps_id(&mut reader)?;
    let sps_id = parse_pps_sps_id(&mut reader, sps)?;
    let mut flags = 0u32;
    if reader.read_bit("dependent_slice_segments_enabled_flag")? {
        flags |= PPS_DEPENDENT;
    }
    if reader.read_bit("output_flag_present_flag")? {
        flags |= PPS_OUTPUT_FLAG;
    }
    let num_extra_slice_header_bits = reader.read_bits(3, "num_extra_slice_header_bits")?;
    let num_extra_slice_header_bits = u8::try_from(num_extra_slice_header_bits).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
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
    if reader.read_bit("entropy_coding_sync_enabled_flag")? {
        flags |= PPS_SYNC;
    }
    if reader.read_bit("pps_loop_filter_across_slices_enabled_flag")? {
        flags |= PPS_LOOP_ACROSS_SLICES;
    }
    let (beta_offset_div2, tc_offset_div2) = parse_pps_deblocking(&mut reader, &mut flags)?;
    let scaling = parse_pps_scaling(&mut reader, sps, &mut flags)?;
    if reader.read_bit("lists_modification_present_flag")? {
        flags |= PPS_LISTS_MOD;
    }
    let log2_parallel_merge_minus2 =
        parse_bounded_ue(&mut reader, "log2_parallel_merge_level_minus2", 4)?;
    if reader.read_bit("slice_segment_header_extension_present_flag")? {
        flags |= PPS_SLICE_EXT;
    }
    parse_pps_extension(&mut reader)?;
    Ok(Pps {
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
        num_tile_columns: tiles.0,
        num_tile_rows: tiles.1,
        column_width: tiles.2,
        row_height: tiles.3,
        beta_offset_div2,
        tc_offset_div2,
        scaling,
        log2_parallel_merge_minus2,
    })
}

/// Set id below the set count.
fn parse_pps_id(reader: &mut BitReader) -> Result<u8, D3d11Error> {
    let id = reader.read_ue("pic_parameter_set_id")?;
    let id = u8::try_from(id).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "pic_parameter_set_id",
        })
    })?;
    if usize::from(id) >= MAX_PPS {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "pic_parameter_set_id",
        }));
    }
    Ok(id)
}

/// Sequence id matching the supplied sequence set.
fn parse_pps_sps_id(reader: &mut BitReader, sps: &Sps) -> Result<u8, D3d11Error> {
    let id = reader.read_ue("seq_parameter_set_id")?;
    let id = u8::try_from(id).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "seq_parameter_set_id",
        })
    })?;
    if id != sps.id {
        return Err(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps));
    }
    Ok(id)
}

/// Reference count field below the 14 ceiling.
fn parse_ref_count(reader: &mut BitReader, syntax: &'static str) -> Result<u8, D3d11Error> {
    parse_bounded_ue(reader, syntax, 14)
}

/// QP delta field.
fn parse_qp(reader: &mut BitReader, syntax: &'static str) -> Result<i8, D3d11Error> {
    let value = reader.read_se(syntax)?;
    i8::try_from(value).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// CU QP delta depth, zeroed without deltas.
fn parse_cu_qp_depth(reader: &mut BitReader, flags: &mut u32) -> Result<u8, D3d11Error> {
    if !reader.read_bit("cu_qp_delta_enabled_flag")? {
        return Ok(0);
    }
    *flags |= PPS_CU_QP_DELTA;
    let value = reader.read_ue("diff_cu_qp_delta_depth")?;
    u8::try_from(value).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
            syntax: "diff_cu_qp_delta_depth",
        })
    })
}

/// Tile grid: counts plus explicit widths with derived last entries.
fn parse_tiles(
    reader: &mut BitReader,
    sps: &Sps,
    flags: &mut u32,
) -> Result<(u8, u8, Vec<u16>, Vec<u16>), D3d11Error> {
    if !reader.read_bit("tiles_enabled_flag")? {
        return Ok((1, 1, Vec::new(), Vec::new()));
    }
    *flags |= PPS_TILES;
    let columns = parse_tile_count(reader, "num_tile_columns_minus1", MAX_TILE_COLUMNS)?;
    let rows = parse_tile_count(reader, "num_tile_rows_minus1", MAX_TILE_ROWS)?;
    if reader.read_bit("uniform_spacing_flag")? {
        *flags |= PPS_UNIFORM_SPACING;
        return Ok((columns, rows, Vec::new(), Vec::new()));
    }
    let widths = parse_tile_widths(reader, columns, sps.width_in_ctbs(), "column_width_minus1")?;
    let heights = parse_tile_widths(reader, rows, sps.height_in_ctbs(), "row_height_minus1")?;
    if reader.read_bit("loop_filter_across_tiles_enabled_flag")? {
        *flags |= PPS_LOOP_ACROSS_TILES;
    }
    Ok((columns, rows, widths, heights))
}

/// Tile count below the DXVA grid ceiling.
fn parse_tile_count(
    reader: &mut BitReader,
    syntax: &'static str,
    ceiling: usize,
) -> Result<u8, D3d11Error> {
    let minus1 = reader.read_ue(syntax)?;
    let count = minus1 + 1;
    if usize::try_from(count).is_ok_and(|count| count > ceiling) {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::TileGridAboveDriverGrid,
        ));
    }
    u8::try_from(count).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))
}

/// Explicit tile extents with the last entry derived from `total`.
fn parse_tile_widths(
    reader: &mut BitReader,
    count: u8,
    total: u32,
    syntax: &'static str,
) -> Result<Vec<u16>, D3d11Error> {
    let mut widths = Vec::with_capacity(usize::from(count));
    let mut consumed = 0u32;
    for _ in 1..count {
        let minus1 = reader.read_ue(syntax)?;
        let width = minus1 + 1;
        consumed = consumed
            .checked_add(width)
            .filter(|sum| *sum < total)
            .ok_or(D3d11Error::Parse(ParseReason::Range { syntax }))?;
        widths.push(
            u16::try_from(width).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))?,
        );
    }
    let last = total - consumed;
    widths.push(u16::try_from(last).map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax }))?);
    Ok(widths)
}

/// Picture deblocking offsets, zeroed without override.
fn parse_pps_deblocking(reader: &mut BitReader, flags: &mut u32) -> Result<(i8, i8), D3d11Error> {
    if !reader.read_bit("deblocking_filter_override_enabled_flag")? {
        return Ok((0, 0));
    }
    *flags |= PPS_DEBLOCK_OVERRIDE;
    if reader.read_bit("pps_deblocking_filter_disabled_flag")? {
        *flags |= PPS_DEBLOCK_DISABLED;
        return Ok((0, 0));
    }
    let beta = parse_qp(reader, "pps_beta_offset_div2")?;
    let tc = parse_qp(reader, "pps_tc_offset_div2")?;
    Ok((beta, tc))
}

/// Picture scaling lists, copied from the sequence set without data.
fn parse_pps_scaling(
    reader: &mut BitReader,
    sps: &Sps,
    flags: &mut u32,
) -> Result<ScalingLists, D3d11Error> {
    if !reader.read_bit("pps_scaling_list_data_present_flag")? {
        return Ok(sps.scaling.clone());
    }
    *flags |= PPS_SCALING;
    parse_scaling_list_data(reader)
}

/// Picture extension flags, declining any extension.
fn parse_pps_extension(reader: &mut BitReader) -> Result<(), D3d11Error> {
    if !reader.read_bit("pps_extension_present_flag")? {
        return Ok(());
    }
    let mut extended = false;
    for syntax in [
        "pps_range_extension_flag",
        "pps_multilayer_extension_flag",
        "pps_3d_extension_flag",
        "pps_scc_extension_flag",
    ] {
        extended |= reader.read_bit(syntax)?;
    }
    reader.read_bits(4, "pps_extension_4bits")?;
    if extended {
        return Err(D3d11Error::Unsupported(UnsupportedReason::PpsExtension));
    }
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
    fn new(value: u32) -> Result<Self, D3d11Error> {
        match value {
            0 => Ok(Self::B),
            1 => Ok(Self::P),
            2 => Ok(Self::I),
            _ => Err(D3d11Error::Parse(ParseReason::Range {
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

/// Long-term picture description behind POC derivation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LtSpec {
    /// Long-term POC LSB.
    pub(crate) poc_lsb: u32,
    /// Used by the current picture.
    pub(crate) used: bool,
    /// MSB cycle present.
    pub(crate) msb_present: bool,
    /// MSB cycle delta.
    pub(crate) msb_cycle: u32,
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SliceHeader {
    /// First slice segment in the picture.
    pub(crate) first: bool,
    /// Slice segment address in treeblocks.
    pub(crate) address: u32,
    /// Slice coding type.
    pub(crate) slice_type: SliceType,
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
    /// Long-term picture descriptions.
    pub(crate) lt: Vec<LtSpec>,
}

/// Slice header prefix: first flag, prior-output flag, set id.
fn parse_slice_prefix(
    reader: &mut BitReader,
    nal_type: u8,
) -> Result<(bool, bool, u8), D3d11Error> {
    let first = reader.read_bit("first_slice_segment_in_pic_flag")?;
    let no_output_prior = if is_irap(nal_type) {
        reader.read_bit("no_output_of_prior_pics_flag")?
    } else {
        false
    };
    let pps_id = reader.read_ue("slice_pic_parameter_set_id")?;
    let pps_id = u8::try_from(pps_id).map_err(|_| {
        D3d11Error::Parse(ParseReason::Range {
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
) -> Result<SliceHeader, D3d11Error> {
    let (first, no_output_prior, _) = prefix;
    let address = parse_slice_address(reader, first, sps, pps)?;
    for _ in 0..pps.num_extra_slice_header_bits {
        reader.read_bit("slice_reserved_flag")?;
    }
    let slice_type = SliceType::new(reader.read_ue("slice_type")?)?;
    if is_idr(nal_type) && !slice_type.is_intra() {
        return Err(D3d11Error::Parse(ParseReason::NonIntraSliceInIdr));
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
    parse_entry_points(reader, pps)?;
    parse_slice_extension(reader, pps)?;
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
    })
}

/// Slice segment address, declining dependent segments.
fn parse_slice_address(
    reader: &mut BitReader,
    first: bool,
    sps: &Sps,
    pps: &Pps,
) -> Result<u32, D3d11Error> {
    if first {
        return Ok(0);
    }
    if pps.dependent_slices() && reader.read_bit("dependent_slice_segment_flag")? {
        return Err(D3d11Error::Unsupported(
            UnsupportedReason::DependentSliceSegments,
        ));
    }
    let ctbs = sps.width_in_ctbs() * sps.height_in_ctbs();
    if ctbs <= 1 {
        return Ok(0);
    }
    let width = 32 - (ctbs - 1).leading_zeros();
    let address = reader.read_bits(width, "slice_segment_address")?;
    if address >= ctbs {
        return Err(D3d11Error::Parse(ParseReason::Range {
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
fn parse_slice_rps(
    reader: &mut BitReader,
    nal_type: u8,
    sps: &Sps,
) -> Result<SliceRefSets, D3d11Error> {
    if is_idr(nal_type) {
        return Ok(SliceRefSets {
            poc_lsb: None,
            rps: None,
            rps_bits: None,
            lt: Vec::new(),
        });
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
fn parse_slice_short_term(
    reader: &mut BitReader,
    sps: &Sps,
) -> Result<(ShortTermRps, RpsBits), D3d11Error> {
    if !reader.read_bit("short_term_ref_pic_set_sps_flag")? {
        let start = reader.position();
        let num = sps.rps.len();
        let resolved = parse_short_term_rps(reader, num, num, &sps.rps)?;
        let bits = reader
            .position()
            .checked_sub(start)
            .and_then(|bits| u16::try_from(bits).ok())
            .ok_or(D3d11Error::Parse(ParseReason::ShortTermRefPicSetSize))?;
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
            D3d11Error::Parse(ParseReason::Range {
                syntax: "short_term_ref_pic_set_idx",
            })
        })?
    } else {
        0
    };
    let set = sps.rps.get(index).cloned().ok_or({
        D3d11Error::Parse(ParseReason::Range {
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

/// Long-term pictures: sequence entries plus slice entries.
fn parse_slice_long_term(reader: &mut BitReader, sps: &Sps) -> Result<Vec<LtSpec>, D3d11Error> {
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
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "num_long_term_pics",
        }));
    }
    if from_sps > u32::try_from(sps.lt_poc_lsb.len()).unwrap_or(u32::MAX) {
        return Err(D3d11Error::Parse(ParseReason::Range {
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
fn parse_lt_sps_entry(reader: &mut BitReader, sps: &Sps) -> Result<(u32, bool), D3d11Error> {
    let index = if sps.lt_poc_lsb.len() > 1 {
        let width = 32
            - u32::try_from(sps.lt_poc_lsb.len() - 1)
                .unwrap_or(u32::MAX)
                .leading_zeros();
        usize::try_from(reader.read_bits(width, "lt_idx_sps")?).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "lt_idx_sps",
            })
        })?
    } else {
        0
    };
    let poc_lsb = sps.lt_poc_lsb.get(index).copied().ok_or({
        D3d11Error::Parse(ParseReason::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    let used = sps.lt_used_by_curr.get(index).copied().ok_or({
        D3d11Error::Parse(ParseReason::Range {
            syntax: "lt_idx_sps",
        })
    })?;
    Ok((poc_lsb, used))
}

/// Slice sample adaptive offset flags.
fn parse_slice_sao(reader: &mut BitReader, sps: &Sps) -> Result<(bool, bool), D3d11Error> {
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
    lt: &[LtSpec],
) -> Result<(), D3d11Error> {
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
        return Err(D3d11Error::Parse(ParseReason::Range {
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
fn parse_ref_override(
    reader: &mut BitReader,
    slice_type: SliceType,
    pps: &Pps,
) -> Result<(u8, u8), D3d11Error> {
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
) -> Result<(), D3d11Error> {
    let width = 32 - u32::try_from(total_curr - 1).unwrap_or(0).leading_zeros();
    if reader.read_bit("ref_pic_list_modification_flag_l0")? {
        for _ in 0..=num_l0 {
            let entry = reader.read_bits(width, "list_entry_l0")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(D3d11Error::Parse(ParseReason::Range {
                    syntax: "list_entry_l0",
                }));
            }
        }
    }
    if slice_type.is_b() && reader.read_bit("ref_pic_list_modification_flag_l1")? {
        for _ in 0..=num_l1 {
            let entry = reader.read_bits(width, "list_entry_l1")?;
            if usize::try_from(entry).is_ok_and(|entry| entry >= total_curr) {
                return Err(D3d11Error::Parse(ParseReason::Range {
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
) -> Result<(), D3d11Error> {
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
            return Err(D3d11Error::Parse(ParseReason::Range {
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
) -> Result<(), D3d11Error> {
    if reader.read_ue("luma_log2_weight_denom")? > 7 {
        return Err(D3d11Error::Parse(ParseReason::Range {
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
fn parse_weight_list(reader: &mut BitReader, count_minus1: u8) -> Result<(), D3d11Error> {
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
fn parse_slice_qp(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d11Error> {
    reader.read_se("slice_qp_delta")?;
    if pps.chroma_offsets_present() {
        reader.read_se("slice_cb_qp_offset")?;
        reader.read_se("slice_cr_qp_offset")?;
    }
    Ok(())
}

/// Slice deblocking description, returning the disabled state.
fn parse_slice_deblocking(reader: &mut BitReader, pps: &Pps) -> Result<bool, D3d11Error> {
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
) -> Result<(), D3d11Error> {
    if pps.loop_across_slices() && (sao.0 || sao.1 || !deblocking_disabled) {
        reader.read_bit("slice_loop_filter_across_slices_enabled_flag")?;
    }
    Ok(())
}

/// Tile and wavefront entry point offsets.
fn parse_entry_points(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d11Error> {
    if !pps.tiles_enabled() && !pps.entropy_sync() {
        return Ok(());
    }
    let count = reader.read_ue("num_entry_point_offsets")?;
    if count == 0 {
        return Ok(());
    }
    let length_minus1 = reader.read_ue("offset_len_minus1")?;
    if length_minus1 > 31 {
        return Err(D3d11Error::Parse(ParseReason::Range {
            syntax: "offset_len_minus1",
        }));
    }
    for _ in 0..count {
        reader.read_bits(length_minus1 + 1, "entry_point_offset_minus1")?;
    }
    Ok(())
}

/// Slice extension bytes.
fn parse_slice_extension(reader: &mut BitReader, pps: &Pps) -> Result<(), D3d11Error> {
    if !pps.slice_ext_present() {
        return Ok(());
    }
    let length = reader.read_ue("slice_segment_header_extension_length")?;
    for _ in 0..length {
        reader.read_bits(8, "slice_segment_header_extension_data_byte")?;
    }
    Ok(())
}

/// One coded slice segment: header plus its NAL bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingSlice {
    /// Parsed slice header.
    pub(crate) header: SliceHeader,
    /// NAL bytes without the start code.
    pub(crate) nal: Vec<u8>,
    /// NAL unit type.
    pub(crate) nal_type: u8,
    /// Temporal id.
    pub(crate) temporal_id: u8,
    /// Picture set id.
    pub(crate) pps_id: u8,
}

/// One coded picture: segments sharing set and type.
#[derive(Clone, Debug, Eq, PartialEq)]
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
        self.slices.first().map_or(0, |slice| slice.pps_id)
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
    /// Seen video sets by id.
    vps: [bool; MAX_SPS],
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
            vps: [false; MAX_SPS],
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
    pub(crate) fn ingest_packet(&mut self, data: &[u8]) -> Result<Vec<Picture>, D3d11Error> {
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
    pub(crate) fn finish(&mut self) -> Result<Option<Picture>, D3d11Error> {
        self.complete_picture()
    }

    /// One NAL unit into the ingest state.
    fn ingest_nal(&mut self, nal: &[u8], pictures: &mut Vec<Picture>) -> Result<(), D3d11Error> {
        let Some(header) = nal.get(0..2) else {
            return Ok(());
        };
        if header[0] >> 7 != 0 {
            return Err(D3d11Error::Parse(ParseReason::NalForbiddenBit));
        }
        let nal_type = (header[0] >> 1) & 0x3F;
        let layer_id = ((header[0] & 1) << 2) | (header[1] >> 6);
        if layer_id != 0 {
            return Err(D3d11Error::Unsupported(UnsupportedReason::Multilayer));
        }
        let temporal_plus1 = header[1] & 7;
        if temporal_plus1 == 0 {
            return Err(D3d11Error::Parse(ParseReason::NalTemporalId));
        }
        let temporal_id = temporal_plus1 - 1;
        match nal_type {
            VCL_FIRST..=VCL_LAST => self.ingest_vcl(nal, nal_type, temporal_id, pictures),
            NAL_VPS => self.ingest_vps(nal),
            NAL_SPS => self.ingest_sps(nal),
            NAL_PPS => self.ingest_pps(nal),
            NAL_AUD | NAL_EOS | NAL_EOB => {
                if let Some(picture) = self.complete_picture()? {
                    pictures.push(picture);
                }
                Ok(())
            }
            NAL_FD | NAL_PREFIX_SEI | NAL_SUFFIX_SEI | 41..=u8::MAX => Ok(()),
        }
    }

    /// Coded slice segment into the open picture.
    fn ingest_vcl(
        &mut self,
        nal: &[u8],
        nal_type: u8,
        temporal_id: u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), D3d11Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d11Error::Parse(ParseReason::SliceWithoutHeader));
        };
        let mut reader = BitReader::new(body);
        let prefix = parse_slice_prefix(&mut reader, nal_type)?;
        let pps_id = prefix.2;
        let pps = self
            .pps(pps_id)
            .ok_or(D3d11Error::Parse(ParseReason::SliceCitesUnknownPps))?;
        let sps = self
            .sps(pps.sps_id)
            .ok_or(D3d11Error::Parse(ParseReason::SliceCitesUnknownSps))?;
        let parsed = parse_slice_rest(&mut reader, prefix, nal_type, sps, pps)?;
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
            pps_id,
        });
        Ok(())
    }

    /// Video set id into the store.
    fn ingest_vps(&mut self, nal: &[u8]) -> Result<(), D3d11Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d11Error::Parse(ParseReason::VpsWithoutBody));
        };
        let mut reader = BitReader::new(body);
        let id = reader.read_bits(4, "vps_video_parameter_set_id")?;
        let id = usize::try_from(id)
            .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "vps id" }))?;
        if id < MAX_SPS {
            self.vps[id] = true;
        }
        Ok(())
    }

    /// Sequence set into the store.
    fn ingest_sps(&mut self, nal: &[u8]) -> Result<(), D3d11Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d11Error::Parse(ParseReason::SpsWithoutBody));
        };
        let mut probe = BitReader::new(body);
        let vps_id = probe.read_bits(4, "sps_video_parameter_set_id")?;
        let vps_id = usize::try_from(vps_id)
            .map_err(|_| D3d11Error::Parse(ParseReason::Range { syntax: "vps id" }))?;
        if !self.vps.get(vps_id).is_some_and(|seen| *seen) {
            return Err(D3d11Error::Parse(ParseReason::SpsCitesUnknownVps));
        }
        let sps = parse_sps(body)?;
        let id = sps.id;
        self.sps[usize::from(id)] = Some(sps);
        Ok(())
    }

    /// Picture set into the store.
    fn ingest_pps(&mut self, nal: &[u8]) -> Result<(), D3d11Error> {
        let bytes = rbsp(nal);
        let Some(body) = bytes.get(2..) else {
            return Err(D3d11Error::Parse(ParseReason::PpsWithoutBody));
        };
        let mut probe = BitReader::new(body);
        probe.read_ue("pic_parameter_set_id")?;
        let sps_id = probe.read_ue("seq_parameter_set_id")?;
        let sps_id = u8::try_from(sps_id).map_err(|_| {
            D3d11Error::Parse(ParseReason::Range {
                syntax: "seq_parameter_set_id",
            })
        })?;
        let sps = self
            .sps(sps_id)
            .ok_or(D3d11Error::Parse(ParseReason::PpsCitesUnknownSps))?;
        let pps = parse_pps(body, sps)?;
        let id = pps.id;
        self.pps[usize::from(id)] = Some(pps);
        Ok(())
    }

    /// Open slices as one validated picture.
    fn complete_picture(&mut self) -> Result<Option<Picture>, D3d11Error> {
        if self.pending.is_empty() {
            return Ok(None);
        }
        let slices = core::mem::take(&mut self.pending);
        let first = &slices[0];
        for slice in &slices {
            if slice.pps_id != first.pps_id || slice.nal_type != first.nal_type {
                return Err(D3d11Error::Parse(ParseReason::PictureSlicesDisagree));
            }
        }
        let mut previous: Option<u32> = None;
        for slice in &slices {
            match previous {
                None => previous = Some(slice.header.address),
                Some(prev) if slice.header.address > prev => {
                    previous = Some(slice.header.address);
                }
                _ => {
                    return Err(D3d11Error::Unsupported(
                        UnsupportedReason::ArbitrarySliceOrder,
                    ))
                }
            }
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

#[cfg(test)]
mod tests {
    use super::{parse_pps, parse_scaling_list_data, parse_sps, rbsp, Ingest, SliceType, Sps};

    /// Test bit writer with emulation prevention.
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

        /// Profile-tier-level with Main profile compatibility.
        fn ptl(&mut self, interlaced: bool) {
            self.bits(0, 2);
            self.bit(false);
            self.bits(1, 5);
            for index in 0..32 {
                self.bit(index == 1);
            }
            self.bit(!interlaced);
            self.bit(interlaced);
            self.bit(true);
            self.bit(true);
            self.bits(0, 24);
            self.bits(0, 19);
            self.bit(false);
            self.bits(120, 8);
        }

        /// Stop bit plus byte alignment.
        fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.filled != 0 {
                self.bit(false);
            }
            self.bytes
        }

        /// Finished RBSP with emulation prevention inserted.
        fn finish_ebsp(self) -> Vec<u8> {
            let mut out = Vec::new();
            let mut zeros = 0;
            for byte in self.finish() {
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

    /// NAL unit with two-byte header over the EBSP body.
    fn nal(nal_type: u8, temporal_id: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![nal_type << 1, temporal_id + 1];
        nal.extend_from_slice(body);
        nal
    }

    /// Packet of NAL units with four-byte start codes.
    fn packet(nals: &[Vec<u8>]) -> Vec<u8> {
        let mut packet = Vec::new();
        for nal in nals {
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(nal);
        }
        packet
    }

    /// 320x240 sequence set body with one explicit reference set.
    fn sps_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.ptl(false);
        writer.ue(0);
        writer.ue(1);
        writer.ue(320);
        writer.ue(240);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.ue(4);
        writer.ue(2);
        writer.ue(5);
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        writer.ue(3);
        writer.ue(1);
        writer.ue(1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.ue(1);
        writer.ue(1);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.finish_ebsp()
    }

    /// Sequence set used across tests.
    fn sps() -> Sps {
        parse_sps(&rbsp(&sps_body())).expect("valid sps")
    }

    /// Picture set body over sequence set zero.
    fn pps_body() -> Vec<u8> {
        pps_body_opts(false)
    }

    /// Picture set body with dependent slice selection.
    fn pps_body_opts(dependent: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.ue(0);
        writer.ue(0);
        writer.bit(dependent);
        writer.bit(false);
        writer.bits(0, 3);
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.se(0);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.ue(1);
        writer.se(0);
        writer.se(0);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.finish_ebsp()
    }

    /// IDR slice header body over the test sets.
    fn idr_slice_body() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(2);
        writer.bit(true);
        writer.bit(true);
        writer.se(0);
        writer.bit(true);
        writer.ue(0);
        writer.finish_ebsp()
    }

    /// P slice header body with `lsb` over the test sets.
    fn p_slice_body(lsb: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(1);
        writer.bits(lsb, 4);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.se(0);
        writer.bit(true);
        writer.ue(0);
        writer.finish_ebsp()
    }

    /// B slice header body with an explicit reference set.
    fn b_slice_body(lsb: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.bits(lsb, 4);
        writer.bit(false);
        writer.bit(false);
        writer.ue(1);
        writer.ue(1);
        writer.ue(1);
        writer.bit(true);
        writer.ue(0);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.ue(0);
        writer.se(0);
        writer.bit(true);
        writer.ue(0);
        writer.finish_ebsp()
    }

    /// CRA I slice header body with `lsb` over the test sets.
    fn cra_slice_body(lsb: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bit(false);
        writer.ue(0);
        writer.ue(2);
        writer.bits(lsb, 4);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.bit(true);
        writer.se(0);
        writer.bit(true);
        writer.ue(0);
        writer.finish_ebsp()
    }

    /// Parameter set NALs: video, sequence, picture.
    fn param_nals() -> Vec<Vec<u8>> {
        vec![
            nal(32, 0, &[0x00, 0x80]),
            nal(33, 0, &sps_body()),
            nal(34, 0, &pps_body()),
        ]
    }

    #[test]
    fn sps_reports_coded_size() {
        let sps = sps();
        assert_eq!(sps.id, 0);
        assert_eq!(sps.level_idc, 120);
        assert_eq!((sps.width, sps.height), (320, 240));
        assert_eq!(sps.min_cb_size(), 8);
        assert_eq!(sps.ctb_size(), 64);
        assert_eq!(sps.width_in_ctbs(), 5);
        assert_eq!(sps.height_in_ctbs(), 4);
        assert_eq!(sps.max_poc_lsb(), 16);
        assert_eq!(sps.max_dec_pic_buffering_minus1, 4);
        assert_eq!(sps.max_num_reorder_pics, 2);
        assert!(sps.sao());
        assert!(sps.temporal_mvp());
        assert!(!sps.scaling_enabled());
        assert_eq!(sps.rps.len(), 1);
        assert_eq!(sps.rps[0].negative_count, 1);
        assert_eq!(sps.rps[0].negative[0], -1);
        assert!(sps.rps[0].negative_used[0]);
        assert_eq!(sps.visible_width(), 320);
        assert_eq!(sps.visible_height(), 240);
    }

    #[test]
    fn sps_refuses_depth_and_interlace() {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.ptl(false);
        writer.ue(0);
        writer.ue(1);
        writer.ue(320);
        writer.ue(240);
        writer.bit(false);
        writer.ue(2);
        assert!(parse_sps(&rbsp(&writer.finish_ebsp())).is_err());

        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.ptl(true);
        writer.ue(0);
        assert!(parse_sps(&rbsp(&writer.finish_ebsp())).is_err());
    }

    #[test]
    fn sps_parses_predicted_rps() {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.ptl(false);
        writer.ue(0);
        writer.ue(1);
        writer.ue(320);
        writer.ue(240);
        writer.bit(false);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.ue(4);
        writer.ue(2);
        writer.ue(5);
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        writer.ue(3);
        writer.ue(1);
        writer.ue(1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(2);
        writer.ue(2);
        writer.ue(1);
        writer.ue(0);
        writer.bit(true);
        writer.ue(1);
        writer.bit(true);
        writer.ue(1);
        writer.bit(false);
        writer.bit(true);
        writer.bit(true);
        writer.ue(0);
        writer.bit(true);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        let sps = parse_sps(&rbsp(&writer.finish_ebsp())).expect("predicted rps");
        assert_eq!(sps.rps.len(), 2);
        let predicted = &sps.rps[1];
        assert_eq!(predicted.negative_count, 3);
        assert_eq!(predicted.negative[..3], [-1, -2, -4]);
        assert_eq!(predicted.negative_used[..3], [true, true, false]);
        assert_eq!(predicted.positive_count, 0);
    }

    #[test]
    fn pps_reports_flags_and_defaults() {
        let pps = parse_pps(&rbsp(&pps_body()), &sps()).expect("valid pps");
        assert_eq!(pps.id, 0);
        assert_eq!(pps.sps_id, 0);
        assert!(pps.sign_hiding());
        assert!(pps.weighted_pred());
        assert!(!pps.weighted_bipred());
        assert!(!pps.tiles_enabled());
        assert!(pps.entropy_sync());
        assert!(pps.loop_across_slices());
        assert!(pps.cu_qp_delta());
        assert_eq!(pps.diff_cu_qp_delta_depth, 1);
        assert_eq!(pps.log2_parallel_merge_minus2, 0);
        assert_eq!((pps.num_tile_columns, pps.num_tile_rows), (1, 1));
        assert!(
            pps.column_width.is_empty(),
            "untiled picture stores no widths"
        );
    }

    #[test]
    fn pps_parses_explicit_tiles() {
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
        writer.bit(true);
        writer.ue(1);
        writer.ue(0);
        writer.bit(false);
        writer.ue(1);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        let pps = parse_pps(&rbsp(&writer.finish_ebsp()), &sps()).expect("tiled pps");
        assert!(pps.tiles_enabled());
        assert!(!pps.uniform_spacing());
        assert!(pps.loop_across_tiles());
        assert_eq!(pps.column_width, vec![2, 3]);
        assert_eq!(pps.row_height, vec![4]);
    }

    #[test]
    fn pps_refuses_huge_grid() {
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
        writer.bit(true);
        writer.ue(20);
        let error = parse_pps(&rbsp(&writer.finish_ebsp()), &sps()).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
    }

    #[test]
    fn slice_idr_parses() {
        let mut ingest = Ingest::new();
        let nals = param_nals();
        let mut all = nals;
        all.push(nal(20, 0, &idr_slice_body()));
        let pictures = ingest.ingest_packet(&packet(&all)).expect("idr packet");
        assert_eq!(pictures.len(), 1);
        let picture = &pictures[0];
        assert!(picture.idr);
        assert!(picture.irap);
        assert!(!picture.rasl);
        assert_eq!(picture.slices.len(), 1);
        let header = &picture.slices[0].header;
        assert!(header.first);
        assert_eq!(header.slice_type, SliceType::I);
        assert_eq!(header.poc_lsb, None);
        assert!(header.rps.is_none());
        assert!(header.pic_output);
        assert!(!header.no_output_prior);
    }

    #[test]
    fn slice_p_parses_with_table_rps() {
        let mut ingest = Ingest::new();
        let mut all = param_nals();
        all.push(nal(1, 0, &p_slice_body(3)));
        let pictures = ingest.ingest_packet(&packet(&all)).expect("p packet");
        assert_eq!(pictures.len(), 1);
        let header = &pictures[0].slices[0].header;
        assert_eq!(header.slice_type, SliceType::P);
        assert_eq!(header.poc_lsb, Some(3));
        let rps = header.rps.as_ref().expect("table rps");
        assert_eq!(rps.negative[0], -1);
        let bits = header.rps_bits.expect("rps provenance");
        assert!(!bits.explicit);
        assert!(!pictures[0].idr);
    }

    #[test]
    fn slice_b_parses_explicit_rps() {
        let mut ingest = Ingest::new();
        let mut all = param_nals();
        all.push(nal(1, 0, &b_slice_body(2)));
        let pictures = ingest.ingest_packet(&packet(&all)).expect("b packet");
        assert_eq!(pictures.len(), 1);
        let header = &pictures[0].slices[0].header;
        assert_eq!(header.slice_type, SliceType::B);
        assert_eq!(header.poc_lsb, Some(2));
        let rps = header.rps.as_ref().expect("explicit rps");
        assert_eq!(rps.negative[0], -2);
        assert!(rps.negative_used[0]);
        assert_eq!(rps.positive[0], 1);
        assert!(!rps.positive_used[0]);
        let bits = header.rps_bits.expect("rps provenance");
        assert!(bits.explicit);
        assert!(bits.bits > 0);
    }

    #[test]
    fn ingest_packet_groups_pictures() {
        let mut ingest = Ingest::new();
        let mut all = param_nals();
        all.push(nal(20, 0, &idr_slice_body()));
        all.push(nal(1, 0, &p_slice_body(1)));
        let pictures = ingest.ingest_packet(&packet(&all)).expect("two pictures");
        assert_eq!(pictures.len(), 2);
        assert!(pictures[0].idr);
        assert_eq!(pictures[1].poc_lsb(), Some(1));
        assert_eq!(pictures[1].pps_id(), 0);
    }

    #[test]
    fn ingest_drops_stream_start_rasl() {
        let mut ingest = Ingest::new();
        let mut all = param_nals();
        all.push(nal(21, 0, &cra_slice_body(8)));
        all.push(nal(8, 0, &p_slice_body(7)));
        let pictures = ingest.ingest_packet(&packet(&all)).expect("cra packet");
        assert_eq!(pictures.len(), 1);
        assert!(pictures[0].irap);

        let next = [nal(21, 0, &cra_slice_body(8)), nal(8, 0, &p_slice_body(7))];
        let pictures = ingest.ingest_packet(&packet(&next)).expect("midstream cra");
        assert_eq!(pictures.len(), 2);
        assert!(pictures[1].rasl);
    }

    #[test]
    fn ingest_declines_dependent_multilayer_and_extension() {
        let mut ingest = Ingest::new();
        let mut second = BitWriter::new();
        second.bit(false);
        second.bit(true);
        let all = vec![
            nal(32, 0, &[0x00, 0x80]),
            nal(33, 0, &sps_body()),
            nal(34, 0, &pps_body_opts(true)),
            nal(1, 0, &p_slice_body(1)),
            nal(1, 0, &second.finish_ebsp()),
        ];
        let error = ingest.ingest_packet(&packet(&all)).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));

        let mut ingest = Ingest::new();
        let error = ingest
            .ingest_packet(&packet(&[vec![0x02, 0x41, 0x80]]))
            .unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));

        let mut writer = BitWriter::new();
        writer.bit(true);
        writer.bit(true);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bits(0, 4);
        let body = writer.finish();
        let mut reader = crate::bits::BitReader::new(&body);
        let error = super::parse_sps_extension(&mut reader).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Unsupported(_)));
    }

    #[test]
    fn ingest_rejects_unknown_sets() {
        let mut ingest = Ingest::new();
        let error = ingest
            .ingest_packet(&packet(&[nal(33, 0, &sps_body())]))
            .unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Parse(_)));

        let mut ingest = Ingest::new();
        let mut all = param_nals();
        let mut bad = BitWriter::new();
        bad.bit(true);
        bad.bit(false);
        bad.ue(3);
        all.push(nal(20, 0, &bad.finish_ebsp()));
        let error = ingest.ingest_packet(&packet(&all)).unwrap_err();
        assert!(matches!(error, crate::error::D3d11Error::Parse(_)));
    }

    #[test]
    fn scaling_list_data_parses_modes() {
        let mut writer = BitWriter::new();
        for size_id in 0..4 {
            let matrices = if size_id == 3 { 2 } else { 6 };
            for matrix_id in 0..matrices {
                if matrix_id == 0 {
                    writer.bit(true);
                    if size_id > 1 {
                        writer.se(0);
                    }
                    let count = 1usize << (4 + (size_id << 1).min(2));
                    for _ in 0..count.min(64) {
                        writer.se(0);
                    }
                } else {
                    writer.bit(false);
                    writer.ue(0);
                }
            }
        }
        let lists = parse_scaling_list_data(&mut crate::bits::BitReader::new(&writer.finish()))
            .expect("lists");
        assert_eq!(lists.size0[0], [8; 16]);
        assert_eq!(lists.size0[1], [16; 16]);
        assert_eq!(lists.size1[0][0], 8);
        assert_eq!(lists.size1[0][63], 8);
        assert_eq!(lists.size1[1][63], 115);
        assert_eq!(lists.size1[4][63], 91);
        assert_eq!(lists.dc2[0], 8);
        assert_eq!(lists.dc2[1], 16);
        assert_eq!(lists.dc3, [8, 16]);
    }

    #[test]
    fn conformance_crop_insets_visible() {
        let mut writer = BitWriter::new();
        writer.bits(0, 4);
        writer.bits(0, 3);
        writer.bit(true);
        writer.ptl(false);
        writer.ue(0);
        writer.ue(1);
        writer.ue(320);
        writer.ue(248);
        writer.bit(true);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.ue(4);
        writer.ue(0);
        writer.ue(0);
        writer.ue(0);
        writer.bit(true);
        writer.ue(4);
        writer.ue(2);
        writer.ue(5);
        writer.ue(0);
        writer.ue(3);
        writer.ue(0);
        writer.ue(3);
        writer.ue(1);
        writer.ue(1);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.ue(0);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        writer.bit(false);
        let sps = parse_sps(&rbsp(&writer.finish_ebsp())).expect("cropped sps");
        assert_eq!(sps.visible_width(), 320);
        assert_eq!(sps.visible_height(), 240);
    }
}
