//! H.264 picture arguments behind one `DecodeFrame` call.

use crate::decode::Submission;

use super::params::{PicParamsH264, QmatrixH264, SliceShortH264};

/// H.264 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct H264Submission {
    /// Picture parameters.
    pub(crate) params: PicParamsH264,
    /// Inverse quantization matrices.
    pub(crate) qmatrix: QmatrixH264,
    /// Slice control entries.
    pub(crate) slices: Vec<SliceShortH264>,
    /// Start-coded slices padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}

impl Submission for H264Submission {
    type Params = PicParamsH264;
    type Matrix = QmatrixH264;
    type Slice = SliceShortH264;

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
