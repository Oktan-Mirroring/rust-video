//! VP9 driver submission for one decoded frame.

use super::params::{PicParamsVp9, SliceVpxShort};

/// Driver submission for one decoded frame.
#[derive(Debug)]
pub(crate) struct SubmittedVp9 {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsVp9,
    /// Slice control buffer, one entry per tile.
    pub(crate) slices: Vec<SliceVpxShort>,
    /// Coded frame bytes behind the slice offsets.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface view.
    pub(crate) slot: u8,
}
