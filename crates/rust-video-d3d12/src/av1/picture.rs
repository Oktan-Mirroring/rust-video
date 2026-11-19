//! AV1 picture arguments behind one `DecodeFrame` call.

use super::params::{PicParamsAv1, TileAv1};

/// AV1 picture arguments for one `DecodeFrame` call.
#[derive(Debug)]
pub(crate) struct Av1Submission {
    /// Picture parameters.
    pub(crate) params: PicParamsAv1,
    /// Tile control entries.
    pub(crate) tiles: Vec<TileAv1>,
    /// Concatenated tile payloads padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output texture slot.
    pub(crate) slot: u8,
}
