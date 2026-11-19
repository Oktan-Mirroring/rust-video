//! Derived DPB inputs: order counts, reference lists, marking targets.
//!
//! Pure H.264 8.2 derivations over caller-held DPB state; tiers keep
//! their storage, pinning, and missing-reference policy.

use crate::slice::{RefMod, SliceType};

/// Absolute frame number behind type-1 order counts, H.264 8.2.1.2.
#[must_use]
pub fn abs_frame_num(offset: i64, frame_num: u32, is_reference: bool) -> i64 {
    let abs = offset.saturating_add(i64::from(frame_num));
    if !is_reference && abs > 0 {
        abs - 1
    } else {
        abs
    }
}

/// Type-0 MSB over an explicit previous pair, H.264 8.2.1.1.
#[must_use]
pub fn poc_msb(lsb: i64, max_lsb: i64, prev: (i64, i64)) -> i64 {
    let half = max_lsb / 2;
    if lsb < prev.1 && prev.1 - lsb >= half {
        prev.0 + max_lsb
    } else if lsb > prev.1 && lsb - prev.1 > half {
        prev.0 - max_lsb
    } else {
        prev.0
    }
}

/// Type-2 picture order count, H.264 8.2.1.3.
///
/// `None` answers a count outside the `i32` range.
#[must_use]
pub fn poc_type2(offset: i64, frame_num: u32, is_reference: bool) -> Option<i32> {
    let base = offset.checked_add(i64::from(frame_num))?;
    let doubled = base.checked_mul(2)?;
    let poc = if is_reference {
        doubled
    } else {
        doubled.checked_sub(1)?
    };
    i32::try_from(poc).ok()
}

/// Wrapped picture number of `frame_num` against `curr`, H.264 8.2.4.1.
#[must_use]
pub fn frame_num_wrap(frame_num: u32, curr: u32, max_frame_num: u32) -> i32 {
    if frame_num > curr {
        frame_num
            .cast_signed()
            .saturating_sub(max_frame_num.cast_signed())
    } else {
        frame_num.cast_signed()
    }
}

/// Short-term picture number named by MMCO difference `diff`, H.264 8.2.5.4.
///
/// Callers match the result against wrapped picture numbers; no match
/// answers an unknown picture.
#[must_use]
pub fn mmco_short_target(diff: u32, frame_num: u32) -> i64 {
    i64::from(frame_num) - i64::from(diff) - 1
}

/// Reference candidate behind initial reference lists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefCandidate {
    /// Entry number behind modification matching.
    pub frame_num: u32,
    /// Wrapped picture number for short-term entries.
    pub pic_num: i32,
    /// Picture order count.
    pub poc: i32,
    /// Long-term index, absent for short-term entries.
    pub long_term_idx: Option<u32>,
}

/// Initial reference lists as candidate indices, H.264 8.2.4.2.
///
/// P slices return `(l0, [])`; B slices return `(l0, l1)` with the
/// identical-list B1 swap applied; I slices return `([], [])`. Ties
/// keep input order; tiers pass candidates in their display or slot
/// order to keep their historical tiebreak.
#[must_use]
pub fn initial_ref_lists(
    cands: &[RefCandidate],
    slice_type: SliceType,
    curr_poc: i32,
) -> (Vec<usize>, Vec<usize>) {
    if slice_type.is_intra() {
        return (Vec::new(), Vec::new());
    }
    let mut shorts: Vec<usize> = cands
        .iter()
        .enumerate()
        .filter(|(_, cand)| cand.long_term_idx.is_none())
        .map(|(index, _)| index)
        .collect();
    let mut longs: Vec<usize> = cands
        .iter()
        .enumerate()
        .filter(|(_, cand)| cand.long_term_idx.is_some())
        .map(|(index, _)| index)
        .collect();
    longs.sort_by_key(|index| cands[*index].long_term_idx);
    if !slice_type.is_b() {
        shorts.sort_by_key(|index| std::cmp::Reverse(cands[*index].pic_num));
        let mut l0 = shorts;
        l0.extend(longs);
        return (l0, Vec::new());
    }
    let mut before: Vec<usize> = shorts.clone();
    before.sort_by_key(|index| l0_key(cands[*index].poc, curr_poc));
    let mut after: Vec<usize> = shorts;
    after.sort_by_key(|index| l1_key(cands[*index].poc, curr_poc));
    let mut l0 = before;
    l0.extend(longs.iter().copied());
    let mut l1 = after;
    l1.extend(longs);
    if l1.len() > 1 && l0 == l1 {
        l1.swap(0, 1);
    }
    (l0, l1)
}

/// L0 sort key: past pictures by descending POC, then future ascending.
fn l0_key(entry: i32, poc: i32) -> (u8, i64) {
    if entry < poc {
        (0, -i64::from(entry))
    } else {
        (1, i64::from(entry))
    }
}

/// L1 sort key: future pictures by ascending POC, then past descending.
fn l1_key(entry: i32, poc: i32) -> (u8, i64) {
    if entry > poc {
        (0, i64::from(entry))
    } else {
        (1, -i64::from(entry))
    }
}

/// Reference-list modifications applied to `order`, H.264 8.2.4.3.
///
/// The predictor wraps modulo the frame-number modulus: encoders emit
/// differences spanning the wrap, so a raw comparison would miss them.
/// `false` answers an unknown target; the tier resyncs or errors.
pub fn apply_ref_mods(
    order: &mut Vec<usize>,
    cands: &[RefCandidate],
    curr_frame_num: u32,
    max_frame_num: u32,
    mods: &[RefMod],
) -> bool {
    let Ok(max) = i32::try_from(max_frame_num) else {
        return false;
    };
    let Ok(mut pred) = i32::try_from(curr_frame_num) else {
        return false;
    };
    for (cursor, modification) in mods.iter().enumerate() {
        let position = match *modification {
            RefMod::ShortBelow(diff) => {
                let Ok(gap) = i32::try_from(diff) else {
                    return false;
                };
                let Some(step) = gap.checked_add(1) else {
                    return false;
                };
                let Some(next) = pred.checked_sub(step) else {
                    return false;
                };
                pred = next.rem_euclid(max);
                find_short(order, cands, pred)
            }
            RefMod::ShortAbove(diff) => {
                let Ok(gap) = i32::try_from(diff) else {
                    return false;
                };
                let Some(step) = gap.checked_add(1) else {
                    return false;
                };
                let Some(next) = pred.checked_add(step) else {
                    return false;
                };
                pred = next.rem_euclid(max);
                find_short(order, cands, pred)
            }
            RefMod::Long(index) => order
                .iter()
                .position(|candidate| cands[*candidate].long_term_idx == Some(index)),
        };
        let Some(position) = position else {
            return false;
        };
        let entry = order.remove(position);
        order.insert(cursor.min(order.len()), entry);
    }
    true
}

/// Position in `order` with raw frame number `target`.
fn find_short(order: &[usize], cands: &[RefCandidate], target: i32) -> Option<usize> {
    order.iter().position(|candidate| {
        let cand = &cands[*candidate];
        cand.long_term_idx.is_none() && i32::try_from(cand.frame_num).ok() == Some(target)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        abs_frame_num, apply_ref_mods, frame_num_wrap, initial_ref_lists, mmco_short_target,
        poc_msb, poc_type2, RefCandidate,
    };
    use crate::slice::{RefMod, SliceType};

    /// Candidate with `pic_num`, `poc`, and long-term `term`.
    fn cand(pic_num: i32, poc: i32, term: Option<u32>) -> RefCandidate {
        RefCandidate {
            frame_num: u32::try_from(pic_num).unwrap_or(0),
            pic_num,
            poc,
            long_term_idx: term,
        }
    }

    #[test]
    fn abs_frame_num_decrements_non_reference() {
        assert_eq!(abs_frame_num(16, 3, true), 19);
        assert_eq!(abs_frame_num(16, 3, false), 18);
        assert_eq!(abs_frame_num(0, 0, false), 0);
    }

    #[test]
    fn msb_wraps_past_half_range() {
        assert_eq!(poc_msb(2, 32, (32, 28)), 64);
        assert_eq!(poc_msb(28, 32, (0, 2)), -32);
        assert_eq!(poc_msb(4, 32, (0, 0)), 0);
    }

    #[test]
    fn type2_doubles_base_minus_non_reference() {
        assert_eq!(poc_type2(4, 1, true), Some(10));
        assert_eq!(poc_type2(4, 1, false), Some(9));
        assert_eq!(poc_type2(1_073_741_823, 0, true), Some(2_147_483_646));
        assert_eq!(poc_type2(1_073_741_824, 0, true), None);
    }

    #[test]
    fn wrap_subtracts_modulus_past_current() {
        assert_eq!(frame_num_wrap(14, 2, 16), -2);
        assert_eq!(frame_num_wrap(2, 2, 16), 2);
        assert_eq!(frame_num_wrap(0, 2, 16), 0);
    }

    #[test]
    fn mmco_target_subtracts_difference_plus_one() {
        assert_eq!(mmco_short_target(5, 2), -4);
        assert_eq!(mmco_short_target(0, 7), 6);
    }

    #[test]
    fn p_list_sorts_wrapped_pic_num_descending() {
        let cands = [cand(-4, 0, None), cand(1, 2, None), cand(0, 4, None)];
        assert_eq!(
            initial_ref_lists(&cands, SliceType::P, 6),
            (vec![1, 2, 0], Vec::new())
        );
    }

    #[test]
    fn b_lists_split_past_and_future() {
        let cands = [cand(0, 0, None), cand(1, 4, None), cand(2, 8, None)];
        let (l0, l1) = initial_ref_lists(&cands, SliceType::B, 4);
        assert_eq!(l0, vec![0, 1, 2]);
        assert_eq!(l1, vec![2, 1, 0]);
    }

    #[test]
    fn b1_swap_fires_on_identical_lists() {
        let cands = [cand(0, 4, None), cand(1, 4, None)];
        let (l0, l1) = initial_ref_lists(&cands, SliceType::B, 4);
        assert_eq!(l0, vec![0, 1]);
        assert_eq!(l1, vec![1, 0]);
    }

    #[test]
    fn ties_keep_input_order() {
        let cands = [cand(5, 0, None), cand(5, 1, None)];
        assert_eq!(
            initial_ref_lists(&cands, SliceType::P, 2),
            (vec![0, 1], Vec::new())
        );
    }

    #[test]
    fn longs_trail_by_index() {
        let cands = [cand(0, 0, Some(2)), cand(1, 2, None), cand(2, 4, Some(0))];
        assert_eq!(
            initial_ref_lists(&cands, SliceType::P, 6),
            (vec![1, 2, 0], Vec::new())
        );
    }

    #[test]
    fn intra_lists_stay_empty() {
        let cands = [cand(0, 0, None)];
        assert_eq!(
            initial_ref_lists(&cands, SliceType::I, 0),
            (Vec::new(), Vec::new())
        );
    }

    #[test]
    fn mods_move_short_and_long_targets() {
        let cands = [cand(0, 0, None), cand(1, 2, None), cand(2, 4, Some(0))];
        let mut order = vec![1, 0, 2];
        assert!(apply_ref_mods(
            &mut order,
            &cands,
            2,
            16,
            &[RefMod::ShortBelow(1), RefMod::Long(0)]
        ));
        assert_eq!(order, vec![0, 2, 1]);
    }

    #[test]
    fn mods_wrap_predictor_past_zero() {
        let cands = [cand(0, 0, None), cand(1, 2, None)];
        let mut order = vec![1, 0];
        assert!(apply_ref_mods(
            &mut order,
            &cands,
            2,
            16,
            &[
                RefMod::ShortBelow(0),
                RefMod::ShortBelow(15),
                RefMod::ShortBelow(0),
            ]
        ));
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn mods_reject_unknown_targets() {
        let cands = [cand(0, 0, None)];
        let mut order = vec![0];
        assert!(!apply_ref_mods(
            &mut order,
            &cands,
            2,
            16,
            &[RefMod::ShortBelow(9)]
        ));
        assert!(!apply_ref_mods(
            &mut order,
            &cands,
            2,
            16,
            &[RefMod::Long(4)]
        ));
    }
}
