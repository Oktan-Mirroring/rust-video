//! Scaling lists in coded order.

use crate::bits::BitReader;
use crate::error::{H265Error, ParseReason};

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
pub struct ScalingLists {
    /// Size id 0: six 4x4 lists.
    pub size0: [[u8; 16]; 6],
    /// Size id 1: six 8x8 lists.
    pub size1: [[u8; 64]; 6],
    /// Size id 2: six 8x8 lists.
    pub size2: [[u8; 64]; 6],
    /// Size id 3: two 8x8 lists.
    pub size3: [[u8; 64]; 2],
    /// Size id 2 DC coefficients.
    pub dc2: [u8; 6],
    /// Size id 3 DC coefficients.
    pub dc3: [u8; 2],
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
pub(crate) fn parse_scaling_list_data<E: H265Error>(
    reader: &mut BitReader<'_, E>,
) -> Result<ScalingLists, E> {
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
fn parse_scaling_matrix<E: H265Error>(
    reader: &mut BitReader<'_, E>,
    lists: &mut ScalingLists,
    size_id: usize,
    matrix_id: usize,
) -> Result<(), E> {
    if !reader.read_bit("scaling_list_pred_mode_flag")? {
        let delta = reader.read_ue("scaling_list_pred_matrix_id_delta")?;
        if delta == 0 {
            return Ok(());
        }
        let delta = usize::try_from(delta).unwrap_or(usize::MAX);
        let reference = matrix_id.checked_sub(delta).ok_or({
            E::parse(ParseReason::Range {
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
            return Err(E::parse(ParseReason::Range {
                syntax: "scaling_list_dc_coef",
            }));
        }
        let dc = u8::try_from(dc + 8).map_err(|_| {
            E::parse(ParseReason::Range {
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
            E::parse(ParseReason::Range {
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
