//! H.265 picture arguments behind one `DecodeFrame` call.

use crate::session::Submission;

use super::params::{PicParamsHevc, QmatrixHevc, SliceShortHevc};

/// H.265 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct HevcSubmission {
    /// Picture parameters.
    pub(crate) params: PicParamsHevc,
    /// Inverse quantization matrices.
    pub(crate) qmatrix: QmatrixHevc,
    /// Slice control entries.
    pub(crate) slices: Vec<SliceShortHevc>,
    /// Start-coded slices padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}

impl Submission for HevcSubmission {
    type Params = PicParamsHevc;
    type Matrix = QmatrixHevc;
    type Slice = SliceShortHevc;

    fn params(&self) -> &Self::Params {
        &self.params
    }

    fn matrix(&self) -> &Self::Matrix {
        &self.qmatrix
    }

    fn slices(&self) -> &[Self::Slice] {
        &self.slices
    }

    fn bitstream_len(&self) -> usize {
        self.bitstream.len()
    }
}
