//! Short-term reference picture sets.

use crate::bits::BitReader;
use crate::error::{HevcError, ParseKind};
use crate::nal::MAX_RPS_PICS;

/// Pictures per resolved reference set.
const MAX_RESOLVED_RPS: u8 = 15;

/// Short-term reference picture set with resolved delta POCs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShortTermRps {
    /// Negative delta POCs, nearest first.
    pub negative: [i32; MAX_RPS_PICS],
    /// Negative used-by-current flags.
    pub negative_used: [bool; MAX_RPS_PICS],
    /// Negative picture count.
    pub negative_count: u8,
    /// Positive delta POCs, nearest first.
    pub positive: [i32; MAX_RPS_PICS],
    /// Positive used-by-current flags.
    pub positive_used: [bool; MAX_RPS_PICS],
    /// Positive picture count.
    pub positive_count: u8,
    /// Inter-predicted from another set.
    pub predicted: bool,
    /// Prediction source index in the set table.
    pub ref_idx: usize,
    /// Reference set index delta minus one, slice sets only.
    pub delta_idx_minus1: u32,
    /// Per-candidate used and keep flags in reference order.
    pub predict_flags: Vec<(bool, bool)>,
    /// Delta RPS sign.
    pub delta_sign: bool,
    /// Absolute delta RPS minus one.
    pub abs_delta_minus1: u32,
    /// Used-by-current mask over the reference pictures.
    pub used_mask: u16,
    /// Use-delta mask over the reference pictures.
    pub use_mask: u16,
}

impl ShortTermRps {
    /// Total delta POC count.
    #[must_use]
    pub fn num_delta_pocs(&self) -> u8 {
        self.negative_count + self.positive_count
    }

    /// Used negative count behind list modification gating.
    #[must_use]
    pub fn used_negative(&self) -> usize {
        self.negative_used
            .iter()
            .take(usize::from(self.negative_count))
            .filter(|used| **used)
            .count()
    }

    /// Used positive count behind list modification gating.
    #[must_use]
    pub fn used_positive(&self) -> usize {
        self.positive_used
            .iter()
            .take(usize::from(self.positive_count))
            .filter(|used| **used)
            .count()
    }
}

/// Reference set with its prediction provenance.
pub struct ResolvedRps {
    /// Resolved set.
    pub set: ShortTermRps,
    /// Delta POC count of the prediction source.
    pub ref_num_delta_pocs: u8,
}

/// Short-term reference set `index` against `table` of `num_sps` entries.
///
/// # Errors
///
/// Returns `Err` on malformed syntax.
///
/// # Arguments
///
/// * `reader` - bit reader over the set syntax.
/// * `index` - set index behind prediction.
/// * `num_sps` - sequence table size behind prediction.
/// * `table` - decoded sets behind prediction.
pub fn parse_short_term_rps<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    index: usize,
    num_sps: usize,
    table: &[ShortTermRps],
) -> Result<ResolvedRps, E> {
    let predicted = index != 0 && reader.read_bit("inter_ref_pic_set_prediction_flag")?;
    if !predicted {
        return Ok(ResolvedRps {
            set: parse_explicit_rps(reader)?,
            ref_num_delta_pocs: 0,
        });
    }
    let (ref_idx, delta_idx_minus1) = predict_source(reader, index, num_sps)?;
    let reference = table.get(ref_idx).ok_or({
        E::parse(ParseKind::Range {
            syntax: "inter_ref_pic_set_prediction",
        })
    })?;
    let ref_pics = usize::from(reference.num_delta_pocs());
    let (delta_rps, delta_sign, abs_delta_minus1) = parse_delta_rps(reader)?;
    let flags = parse_predict_flags(reader, ref_pics)?;
    let mut set = apply_prediction(reference, delta_rps, &flags)?;
    set.predicted = true;
    set.ref_idx = ref_idx;
    set.delta_idx_minus1 = delta_idx_minus1;
    set.delta_sign = delta_sign;
    set.abs_delta_minus1 = abs_delta_minus1;
    set.predict_flags.clone_from(&flags);
    for (bit, (used, keep)) in flags.iter().take(ref_pics).enumerate().take(16) {
        let flag = 1u16 << bit;
        if *used {
            set.used_mask |= flag;
        }
        if *keep {
            set.use_mask |= flag;
        }
    }
    Ok(ResolvedRps {
        set,
        ref_num_delta_pocs: reference.num_delta_pocs(),
    })
}

/// Prediction source index for set `index` with its coded delta.
fn predict_source<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    index: usize,
    num_sps: usize,
) -> Result<(usize, u32), E> {
    let coded = if index == num_sps {
        reader.read_ue("delta_idx_minus1")?
    } else {
        0
    };
    let delta = usize::try_from(coded).unwrap_or(usize::MAX);
    if delta >= index {
        return Err(E::parse(ParseKind::Range {
            syntax: "inter_ref_pic_set_prediction",
        }));
    }
    Ok((index - delta - 1, coded))
}

/// Delta RPS value behind prediction with its coded sign and magnitude.
fn parse_delta_rps<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<(i32, bool, u32), E> {
    let sign = reader.read_bit("delta_rps_sign")?;
    let abs = reader.read_ue("abs_delta_rps_minus1")?;
    if abs > 32767 {
        return Err(E::parse(ParseKind::Range {
            syntax: "abs_delta_rps_minus1",
        }));
    }
    let magnitude = i32::try_from(abs + 1).map_err(|_| {
        E::parse(ParseKind::Range {
            syntax: "abs_delta_rps_minus1",
        })
    })?;
    Ok((if sign { -magnitude } else { magnitude }, sign, abs))
}

/// Per-candidate used and delta flags behind prediction.
fn parse_predict_flags<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    num_delta_pocs: usize,
) -> Result<Vec<(bool, bool)>, E> {
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
fn apply_prediction<E: HevcError>(
    reference: &ShortTermRps,
    delta_rps: i32,
    flags: &[(bool, bool)],
) -> Result<ShortTermRps, E> {
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
fn push_shifted<E: HevcError>(
    set: &mut ShortTermRps,
    delta: i32,
    shift: i32,
    flag: (bool, bool),
    negative: bool,
) -> Result<(), E> {
    let (used, keep) = flag;
    if !keep {
        return Ok(());
    }
    let poc = delta.checked_add(shift).ok_or({
        E::parse(ParseKind::Range {
            syntax: "delta_poc",
        })
    })?;
    if negative && poc >= 0 || !negative && poc <= 0 {
        return Ok(());
    }
    if set.num_delta_pocs() >= MAX_RESOLVED_RPS {
        return Err(E::parse(ParseKind::ShortTermRefPicSetSize));
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
fn parse_explicit_rps<E: HevcError>(reader: &mut BitReader<'_, E>) -> Result<ShortTermRps, E> {
    let negative = parse_rps_count(reader, "num_negative_pics")?;
    let positive = parse_rps_count(reader, "num_positive_pics")?;
    if usize::from(negative) + usize::from(positive) >= MAX_RPS_PICS {
        return Err(E::parse(ParseKind::ShortTermRefPicSetSize));
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
fn parse_rps_count<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    syntax: &'static str,
) -> Result<u8, E> {
    let value = reader.read_ue(syntax)?;
    if usize::try_from(value).is_ok_and(|count| count >= MAX_RPS_PICS) {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    u8::try_from(value).map_err(|_| E::parse(ParseKind::Range { syntax }))
}

/// Next explicit delta POC from a minus-one step.
fn parse_rps_step<E: HevcError>(
    reader: &mut BitReader<'_, E>,
    poc: i32,
    positive: bool,
    syntax: &'static str,
) -> Result<i32, E> {
    let step = reader.read_ue(syntax)?;
    if step > 32767 {
        return Err(E::parse(ParseKind::Range { syntax }));
    }
    let step = i32::try_from(step + 1).map_err(|_| E::parse(ParseKind::Range { syntax }))?;
    if positive {
        poc.checked_add(step)
    } else {
        poc.checked_sub(step)
    }
    .ok_or({
        E::parse(ParseKind::Range {
            syntax: "delta_poc",
        })
    })
}
