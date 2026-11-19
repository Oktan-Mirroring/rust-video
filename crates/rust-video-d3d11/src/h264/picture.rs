//! H.264 driver submission for one decoded picture.

use super::params::{PicParamsH264, QmatrixH264, SliceLongH264};

/// Driver submission for one decoded picture.
#[derive(Debug)]
pub(crate) struct SubmittedPicture {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsH264,
    /// Quantization matrix buffer.
    pub(crate) qmatrix: QmatrixH264,
    /// Slice control buffer.
    pub(crate) slices: Vec<SliceLongH264>,
    /// Concatenated slices with start codes, padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface slot.
    pub(crate) slot: u8,
}
