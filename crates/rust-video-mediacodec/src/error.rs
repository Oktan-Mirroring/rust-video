//! Typed `MediaCodec` tier failures.

use crate::decoder::DecodeState;
use crate::encoder::EncodeState;

use rust_video_caps::frame::PixelFormat;

/// `MediaCodec` tier failure.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MediaCodecError {
    /// Device below the 21 floor.
    #[error("API level {found} below minimum {}", crate::sdk::MIN_SDK)]
    BelowMinSdk {
        /// Device API level.
        found: u32,
    },
    /// No decoder for the mime.
    #[error("no MediaCodec decoder for {mime}")]
    NoDecoder {
        /// Mime the lookup used.
        mime: &'static str,
    },
    /// No encoder for the mime.
    #[error("no MediaCodec encoder for {mime}")]
    NoEncoder {
        /// Mime the lookup used.
        mime: &'static str,
    },
    /// Input frame layout the encoder cannot pack.
    #[error("unsupported encoder input {format:?}")]
    UnsupportedInput {
        /// Rejected pixel layout.
        format: PixelFormat,
    },
    /// Input frame extent against the configured extent.
    #[error("frame {width}x{height} mismatches configured {expected_width}x{expected_height}")]
    FrameMismatch {
        /// Frame width in luma samples.
        width: u32,
        /// Frame height in luma samples.
        height: u32,
        /// Configured width in luma samples.
        expected_width: u32,
        /// Configured height in luma samples.
        expected_height: u32,
    },
    /// Packed frame overflows its input buffer.
    #[error("frame of {len} bytes exceeds input capacity {capacity}")]
    FrameTooLarge {
        /// Packed frame length.
        len: usize,
        /// Buffer capacity.
        capacity: usize,
    },
    /// H.264/H.265 format without its codec-specific blob.
    #[error("missing {key} for {mime}")]
    MissingCsd {
        /// Codec mime demanding the blob.
        mime: &'static str,
        /// Missing `csd` key.
        key: &'static str,
    },
    /// Output color without a plane layout.
    #[error("color format {value} has no plane layout")]
    UnsupportedColorFormat {
        /// Raw `color-format` value.
        value: i32,
    },
    /// Codec buffer vanished mid-call.
    #[error("codec buffer vanished mid-call")]
    BufferLost,
    /// Output range outside its buffer.
    #[error("buffer range {offset}+{size} outside {len} bytes")]
    BufferRange {
        /// Range start.
        offset: i32,
        /// Range length.
        size: i32,
        /// Buffer length.
        len: usize,
    },
    /// Output extent rejected.
    #[error("output extent {width}x{height} rejected")]
    BadExtent {
        /// Reported width.
        width: i32,
        /// Reported height.
        height: i32,
    },
    /// No input buffer inside the queue budget.
    #[error("no input buffer inside the queue budget")]
    InputStarved,
    /// No queue inside the drain budget.
    #[error("still starved after {attempts} drain attempts ({in_flight} in flight)")]
    QueueStalled {
        /// Starved attempts before the stall.
        attempts: u32,
        /// Inputs awaiting outputs at the stall.
        in_flight: usize,
    },
    /// Packet overflows its input buffer.
    #[error("packet of {len} bytes exceeds input capacity {capacity}")]
    InputTooLarge {
        /// Packet length.
        len: usize,
        /// Buffer capacity.
        capacity: usize,
    },
    /// Live surfaces hold every decoder slot.
    #[error("no free surface slot")]
    NoFreeSlot,
    /// Native codec fault.
    #[error("codec fault: {kind}")]
    CodecFault {
        /// Client may continue after reset.
        recoverable: bool,
        /// Fault may clear on retry.
        transient: bool,
        /// Native error kind.
        kind: String,
    },
    /// Operation refused in this state.
    #[error("cannot {op} while {state:?}")]
    InvalidState {
        /// Refused operation.
        op: &'static str,
        /// State the decoder was in.
        state: DecodeState,
    },
    /// Operation refused in this state.
    #[error("cannot {op} while {state:?}")]
    InvalidEncodeState {
        /// Refused operation.
        op: &'static str,
        /// State the encoder was in.
        state: EncodeState,
    },
}

#[cfg(test)]
mod tests {
    use super::MediaCodecError;
    use crate::decoder::DecodeState;
    use crate::encoder::EncodeState;

    use rust_video_caps::frame::PixelFormat;

    #[test]
    fn displays_name_cause() {
        assert_eq!(
            MediaCodecError::NoDecoder { mime: "video/avc" }.to_string(),
            "no MediaCodec decoder for video/avc"
        );
        assert_eq!(
            MediaCodecError::NoEncoder { mime: "video/hevc" }.to_string(),
            "no MediaCodec encoder for video/hevc"
        );
        assert_eq!(
            MediaCodecError::UnsupportedInput {
                format: PixelFormat::Nv12,
            }
            .to_string(),
            "unsupported encoder input Nv12"
        );
        assert_eq!(
            MediaCodecError::MissingCsd {
                mime: "video/avc",
                key: "csd-0",
            }
            .to_string(),
            "missing csd-0 for video/avc"
        );
        assert_eq!(
            MediaCodecError::BufferLost.to_string(),
            "codec buffer vanished mid-call"
        );
        assert_eq!(
            MediaCodecError::InputStarved.to_string(),
            "no input buffer inside the queue budget"
        );
        assert_eq!(
            MediaCodecError::NoFreeSlot.to_string(),
            "no free surface slot"
        );
    }

    #[test]
    fn displays_carry_values() {
        let error = MediaCodecError::BelowMinSdk { found: 19 };
        assert_eq!(error.to_string(), "API level 19 below minimum 21");
        let error = MediaCodecError::UnsupportedColorFormat { value: 7 };
        assert_eq!(error.to_string(), "color format 7 has no plane layout");
        let error = MediaCodecError::BufferRange {
            offset: 8,
            size: 64,
            len: 16,
        };
        assert_eq!(error.to_string(), "buffer range 8+64 outside 16 bytes");
        let error = MediaCodecError::BadExtent {
            width: 0,
            height: 1080,
        };
        assert_eq!(error.to_string(), "output extent 0x1080 rejected");
        let error = MediaCodecError::InputTooLarge {
            len: 1_000,
            capacity: 512,
        };
        assert_eq!(
            error.to_string(),
            "packet of 1000 bytes exceeds input capacity 512"
        );
        let error = MediaCodecError::CodecFault {
            recoverable: true,
            transient: false,
            kind: "ErrorUnknown".to_owned(),
        };
        assert_eq!(error.to_string(), "codec fault: ErrorUnknown");
        let error = MediaCodecError::FrameMismatch {
            width: 64,
            height: 64,
            expected_width: 128,
            expected_height: 64,
        };
        assert_eq!(
            error.to_string(),
            "frame 64x64 mismatches configured 128x64"
        );
        let error = MediaCodecError::FrameTooLarge {
            len: 2_000,
            capacity: 512,
        };
        assert_eq!(
            error.to_string(),
            "frame of 2000 bytes exceeds input capacity 512"
        );
        let error = MediaCodecError::QueueStalled {
            attempts: 200,
            in_flight: 4,
        };
        assert_eq!(
            error.to_string(),
            "still starved after 200 drain attempts (4 in flight)"
        );
    }

    #[test]
    fn state_names_operation() {
        let error = MediaCodecError::InvalidState {
            op: "decode",
            state: DecodeState::Unconfigured,
        };
        assert_eq!(error.to_string(), "cannot decode while Unconfigured");
        let error = MediaCodecError::InvalidEncodeState {
            op: "encode",
            state: EncodeState::Configured,
        };
        assert_eq!(error.to_string(), "cannot encode while Configured");
    }
}
