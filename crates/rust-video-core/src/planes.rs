//! Strided plane validation plus tight row copies.

use std::fmt;

/// Strided-plane validation failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PlaneFault {
    /// Stride below the width.
    StrideBelowWidth {
        /// Plane stride in bytes.
        stride: u32,
        /// Visible width in bytes.
        width: u32,
    },
    /// Plane short of its strided rows.
    ShortOfStride {
        /// Bytes the strided rows need.
        need: u64,
        /// Bytes the plane holds.
        len: usize,
    },
}

impl fmt::Display for PlaneFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StrideBelowWidth { stride, width } => {
                write!(f, "stride {stride} below width {width}")
            }
            Self::ShortOfStride { need, len } => {
                write!(f, "plane holds {len} of {need} bytes")
            }
        }
    }
}

impl std::error::Error for PlaneFault {}

/// `rows` of `width` bytes validated inside a strided plane.
///
/// # Arguments
///
/// * `data` - strided plane bytes.
/// * `stride` - plane stride in bytes.
/// * `width` - visible width in bytes.
/// * `rows` - visible row count.
///
/// # Errors
///
/// Returns `Err` for a stride below the width or a plane short of its stride.
pub fn check_plane(data: &[u8], stride: u32, width: u32, rows: u32) -> Result<(), PlaneFault> {
    let stride_64 = u64::from(stride);
    let width_64 = u64::from(width);
    if stride_64 < width_64 {
        return Err(PlaneFault::StrideBelowWidth { stride, width });
    }
    let need = stride_64
        .saturating_mul(u64::from(rows).saturating_sub(1))
        .saturating_add(width_64);
    if u64::try_from(data.len()).unwrap_or(0) < need {
        return Err(PlaneFault::ShortOfStride {
            need,
            len: data.len(),
        });
    }
    Ok(())
}

/// `rows` of `width` bytes from a validated strided plane into `dst`.
///
/// # Arguments
///
/// * `data` - strided plane bytes; holds its strided rows.
/// * `stride` - plane stride in bytes.
/// * `width` - visible width in bytes.
/// * `rows` - visible row count.
/// * `dst` - tight destination; holds `width` times `rows` bytes.
///
/// # Panics
///
/// Panics when `dst` holds fewer than `width` times `rows` bytes, when `data`
/// holds fewer than its strided rows, or when the row offsets overflow.
pub fn copy_rows(data: &[u8], stride: u32, width: u32, rows: u32, dst: &mut [u8]) {
    let stride = usize::try_from(stride).unwrap_or(0);
    let width = usize::try_from(width).unwrap_or(0);
    let rows = usize::try_from(rows).unwrap_or(0);
    if stride == width {
        let total = width.saturating_mul(rows);
        dst[..total].copy_from_slice(&data[..total]);
        return;
    }
    for (row, chunk) in data.chunks(stride).take(rows).enumerate() {
        dst[row * width..(row + 1) * width].copy_from_slice(&chunk[..width]);
    }
}

/// `rows` of `width` bytes from a validated strided plane appended to `out`.
///
/// # Arguments
///
/// * `data` - strided plane bytes; holds its strided rows.
/// * `stride` - plane stride in bytes.
/// * `width` - visible width in bytes.
/// * `rows` - visible row count.
/// * `out` - tight destination; grows by `width` times `rows` bytes.
///
/// # Panics
///
/// Panics when `data` holds fewer than its strided rows or when the row
/// offsets overflow.
pub fn copy_rows_extend(data: &[u8], stride: u32, width: u32, rows: u32, out: &mut Vec<u8>) {
    let stride = usize::try_from(stride).unwrap_or(0);
    let width = usize::try_from(width).unwrap_or(0);
    let rows = usize::try_from(rows).unwrap_or(0);
    if stride == width {
        let total = width.saturating_mul(rows);
        out.reserve(total);
        out.extend_from_slice(&data[..total]);
        return;
    }
    out.reserve(width.saturating_mul(rows));
    for chunk in data.chunks(stride).take(rows) {
        out.extend_from_slice(&chunk[..width]);
    }
}

#[cfg(test)]
mod tests {
    use super::{check_plane, copy_rows, copy_rows_extend, PlaneFault};

    #[test]
    fn valid_plane_passes() {
        assert!(check_plane(&[0; 8], 4, 4, 2).is_ok());
        assert!(check_plane(&[0; 12], 6, 4, 2).is_ok());
    }

    #[test]
    fn narrow_stride_names_widths() {
        assert_eq!(
            check_plane(&[0; 8], 2, 4, 2),
            Err(PlaneFault::StrideBelowWidth {
                stride: 2,
                width: 4
            })
        );
    }

    #[test]
    fn short_plane_names_need() {
        assert_eq!(
            check_plane(&[0; 5], 4, 4, 2),
            Err(PlaneFault::ShortOfStride { need: 8, len: 5 })
        );
    }

    #[test]
    fn trailing_row_needs_width_not_stride() {
        assert!(check_plane(&[0; 8], 4, 4, 2).is_ok());
        assert_eq!(
            check_plane(&[0; 7], 4, 4, 2),
            Err(PlaneFault::ShortOfStride { need: 8, len: 7 })
        );
    }

    #[test]
    fn fault_display_names_counts() {
        assert_eq!(
            PlaneFault::StrideBelowWidth {
                stride: 2,
                width: 4
            }
            .to_string(),
            "stride 2 below width 4"
        );
        assert_eq!(
            PlaneFault::ShortOfStride { need: 8, len: 5 }.to_string(),
            "plane holds 5 of 8 bytes"
        );
    }

    #[test]
    fn tight_planes_copy_whole() {
        let data: Vec<u8> = (0..16).collect();
        let mut dst = vec![0; 16];
        copy_rows(&data, 4, 4, 4, &mut dst);
        assert_eq!(dst, (0..16).collect::<Vec<u8>>());
    }

    #[test]
    fn strided_planes_strip_padding() {
        let data = vec![1, 2, 9, 9, 3, 4, 9, 9];
        let mut dst = vec![0; 4];
        copy_rows(&data, 4, 2, 2, &mut dst);
        assert_eq!(dst, vec![1, 2, 3, 4]);
    }

    #[test]
    fn extended_planes_match_copied() {
        for (stride, width, rows) in [(4u32, 4u32, 4u32), (4, 2, 2), (8, 3, 5)] {
            let count = usize::try_from(stride * rows).unwrap_or(0);
            let data: Vec<u8> = (0..count)
                .map(|v| u8::try_from(v % 256).unwrap_or(0))
                .collect();
            let mut sliced = vec![0; usize::try_from(width * rows).unwrap_or(0)];
            copy_rows(&data, stride, width, rows, &mut sliced);
            let mut extended = Vec::new();
            copy_rows_extend(&data, stride, width, rows, &mut extended);
            assert_eq!(sliced, extended);
        }
    }
}
