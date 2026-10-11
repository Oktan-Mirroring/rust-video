//! Typed `MediaCodec` tier failures.

use crate::decoder::DecodeState;
use crate::quirks::QuirkSyntax;

/// `MediaCodec` tier failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MediaCodecError {
    /// Backend missing off Android.
    #[error("MediaCodec backend absent on this platform")]
    BackendAbsent,
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
    /// Packet overflows its input buffer.
    #[error("packet of {len} bytes exceeds input capacity {capacity}")]
    InputTooLarge {
        /// Packet length.
        len: usize,
        /// Buffer capacity.
        capacity: usize,
    },
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
    /// Quirk table line rejected.
    #[error("quirks.toml:{line}: {reason}")]
    Quirk {
        /// Failing line number.
        line: u64,
        /// Rejection cause.
        reason: QuirkSyntax,
    },
    /// NDK media call failure.
    #[cfg(target_os = "android")]
    #[error("media call failed: {0}")]
    Ndk(#[from] ndk::media_error::MediaError),
}

#[cfg(test)]
mod tests {
    use super::MediaCodecError;
    use crate::decoder::DecodeState;
    use crate::quirks::QuirkSyntax;

    #[test]
    fn displays_name_cause() {
        assert_eq!(
            MediaCodecError::BackendAbsent.to_string(),
            "MediaCodec backend absent on this platform"
        );
        assert_eq!(
            MediaCodecError::NoDecoder { mime: "video/avc" }.to_string(),
            "no MediaCodec decoder for video/avc"
        );
        assert_eq!(
            MediaCodecError::BufferLost.to_string(),
            "codec buffer vanished mid-call"
        );
        assert_eq!(
            MediaCodecError::InputStarved.to_string(),
            "no input buffer inside the queue budget"
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
    }

    #[test]
    fn state_and_quirk_name_operation() {
        let error = MediaCodecError::InvalidState {
            op: "decode",
            state: DecodeState::Unconfigured,
        };
        assert_eq!(error.to_string(), "cannot decode while Unconfigured");
        let error = MediaCodecError::Quirk {
            line: 9,
            reason: QuirkSyntax::ExpectedSection,
        };
        assert_eq!(error.to_string(), "quirks.toml:9: expected [[quirk]]");
    }
}
