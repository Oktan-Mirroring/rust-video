//! H.265 driver submission for one decoded picture.

use super::params::{PicParamsHevc, QmatrixHevc, SliceShortHevc};

/// Driver submission for one decoded picture.
#[derive(Debug)]
pub(crate) struct SubmittedHevc {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsHevc,
    /// Quantization matrix buffer, absent without scaling lists.
    pub(crate) qmatrix: Option<QmatrixHevc>,
    /// Slice control buffer.
    pub(crate) slices: Vec<SliceShortHevc>,
    /// Concatenated slices with start codes, padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface slot.
    pub(crate) slot: u8,
}
