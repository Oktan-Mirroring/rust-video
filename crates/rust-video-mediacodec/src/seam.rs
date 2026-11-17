#![allow(dead_code)]
//! Codec session behind the portable drain discipline.
//!
//! Reached only from the Android module.

use crate::error::MediaCodecError;

use bytes::Bytes;

/// Codec session behind the portable drain discipline.
///
/// One queued payload in, output items out; implemented once over the
/// NDK `MediaCodec`.
pub(crate) trait CodecSeam: std::fmt::Debug {
    /// One input payload queued; outputs freed along the way.
    ///
    /// # Arguments
    ///
    /// * `payload` - packet or packed frame bytes.
    /// * `pts_us` - presentation time in microseconds.
    ///
    /// # Errors
    ///
    /// Returns `Err` when no input buffer frees inside the queue budget,
    /// when the payload overflows its buffer, or when the codec faults.
    fn submit(&mut self, payload: &[u8], pts_us: u64) -> Result<Vec<SeamItem>, MediaCodecError>;

    /// Available outputs.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the codec faults.
    fn read(&mut self) -> Result<Vec<SeamItem>, MediaCodecError>;

    /// Drained outputs, then a codec flush; stays started.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the codec faults.
    fn reset(&mut self) -> Result<Vec<SeamItem>, MediaCodecError>;
}

/// One drained codec item, in arrival order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SeamItem {
    /// Output payload with flags and timestamp.
    Output(SeamOutput),
    /// Output format reading.
    Format(SeamFormat),
}

/// One codec output payload.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SeamOutput {
    /// Raw output bytes.
    bytes: Bytes,
    /// Buffer flags.
    flags: u32,
    /// Presentation time in microseconds.
    pts_us: i64,
}

impl SeamOutput {
    /// Output over `bytes` with `flags` stamped `pts_us`.
    pub(crate) fn new(bytes: Bytes, flags: u32, pts_us: i64) -> Self {
        Self {
            bytes,
            flags,
            pts_us,
        }
    }

    /// Raw output bytes.
    pub(crate) fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Raw output bytes, owned.
    pub(crate) fn into_bytes(self) -> Bytes {
        self.bytes
    }

    /// Buffer flags.
    pub(crate) fn flags(&self) -> u32 {
        self.flags
    }

    /// Presentation time in microseconds.
    pub(crate) fn pts_us(&self) -> i64 {
        self.pts_us
    }
}

/// One output format reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SeamFormat {
    /// Coded width.
    width: Option<i32>,
    /// Coded height.
    height: Option<i32>,
    /// `color-format` value.
    color: Option<i32>,
    /// Row stride.
    stride: Option<i32>,
    /// Row count.
    slice_height: Option<i32>,
}

impl SeamFormat {
    /// Format reading from raw output format values.
    pub(crate) fn new(
        width: Option<i32>,
        height: Option<i32>,
        color: Option<i32>,
        stride: Option<i32>,
        slice_height: Option<i32>,
    ) -> Self {
        Self {
            width,
            height,
            color,
            stride,
            slice_height,
        }
    }

    /// Coded width.
    pub(crate) fn width(&self) -> Option<i32> {
        self.width
    }

    /// Coded height.
    pub(crate) fn height(&self) -> Option<i32> {
        self.height
    }

    /// `color-format` value.
    pub(crate) fn color(&self) -> Option<i32> {
        self.color
    }

    /// Row stride.
    pub(crate) fn stride(&self) -> Option<i32> {
        self.stride
    }

    /// Row count.
    pub(crate) fn slice_height(&self) -> Option<i32> {
        self.slice_height
    }
}
