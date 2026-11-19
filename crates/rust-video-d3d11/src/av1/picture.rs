//! AV1 driver submission for one decoded frame.

use super::params::{PicParamsAv1, TileAv1};

/// Driver submission for one decoded frame.
#[derive(Debug)]
pub(crate) struct SubmittedAv1 {
    /// Picture parameters buffer.
    pub(crate) params: PicParamsAv1,
    /// Tile control buffer.
    pub(crate) tiles: Vec<TileAv1>,
    /// Concatenated tile payloads, padded to 128 bytes.
    pub(crate) bitstream: Vec<u8>,
    /// Output surface view.
    pub(crate) slot: u8,
}
