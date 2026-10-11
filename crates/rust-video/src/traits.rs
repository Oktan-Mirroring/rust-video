//! Backend contracts: decoders and encoders.

use crate::frame::CpuFrame;
use crate::packet::Packet;

/// Coded packets to frames.
pub trait Decoder {
    /// Decoded frame type.
    type Frame;
    /// Decode failure.
    type Error: std::error::Error;

    /// Frames decoded from `packet`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `packet` - coded input.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error>;

    /// Frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error>;
}

/// Frames to coded packets.
pub trait Encoder {
    /// Encode failure.
    type Error: std::error::Error;

    /// Packets coded from `frame`, possibly none.
    ///
    /// # Arguments
    ///
    /// * `frame` - raw input frame.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot encode `frame`.
    fn encode(&mut self, frame: &CpuFrame) -> Result<Vec<Packet>, Self::Error>;

    /// Packets held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held packets.
    fn flush(&mut self) -> Result<Vec<Packet>, Self::Error>;
}
