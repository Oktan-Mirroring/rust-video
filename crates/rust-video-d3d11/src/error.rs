//! Typed D3D11 tier failures and skip reasons.

use crate::guid::Codec;

/// Machine declined by the D3D11 tier, with the cause.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SkipReason {
    /// D3D11 exists on Windows only.
    #[error("d3d11 needs windows")]
    PlatformUnsupported,
    /// No hardware D3D11 device on the machine.
    #[error("no hardware d3d11 device: {hresult:#x}")]
    NoHardwareDevice {
        /// Failing call status.
        hresult: i32,
    },
    /// Device exposes no video decode interfaces.
    #[error("device without video support")]
    VideoSupportMissing,
    /// Adapter enumerates no decoder profiles.
    #[error("adapter reports no decoder profiles")]
    NoDecoderProfiles,
    /// Codec profile absent from the adapter enumeration.
    #[error("no {codec} decoder profile")]
    ProfileUnsupported {
        /// Codec the adapter cannot decode.
        codec: Codec,
    },
    /// H.264 profile without an NV12 render target.
    #[error("no nv12 render target")]
    NoNv12Target,
}

/// Hard D3D11 tier failure.
#[derive(Debug, thiserror::Error)]
pub enum D3d11Error {
    /// Machine declined; the tier cannot run here.
    #[error("d3d11 skipped: {0}")]
    Skipped(#[from] SkipReason),
    /// D3D11 call failure.
    #[error("{call} failed: {hresult:#x}")]
    Call {
        /// Failing call name.
        call: &'static str,
        /// Failing call status.
        hresult: i32,
    },
    /// Bitstream the tier refuses: interlaced, MBAFF, FMO, ASO, partitions,
    /// gaps, or depth and chroma outside 8-bit 4:2:0.
    #[error("unsupported stream: {0}")]
    Unsupported(String),
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(String),
    /// Reference list names a picture the DPB cannot supply.
    #[error("broken reference list: {0}")]
    BrokenReference(String),
    /// DPB has no free slot for the next picture.
    #[error("dpb full")]
    DpbFull,
    /// Frame over the VRAM budget.
    #[error("frame over budget: {0}")]
    OverBudget(#[from] rust_video_caps::traits::OverBudget),
    /// Picture arrived before any parameter set built a session.
    #[error("picture before session")]
    NoSession,
    /// Bitstream larger than the driver buffer.
    #[error("bitstream of {len} bytes exceeds driver buffer {capacity}")]
    BitstreamTooLarge {
        /// Picture bitstream length.
        len: usize,
        /// Driver buffer length.
        capacity: u64,
    },
    /// Stream demand above the adapter caps.
    #[error("stream {what} above adapter caps")]
    OverCaps {
        /// Capped demand the stream exceeds.
        what: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::{D3d11Error, SkipReason};
    use crate::guid::Codec;

    #[test]
    fn skip_reasons_name_cause() {
        assert_eq!(
            SkipReason::PlatformUnsupported.to_string(),
            "d3d11 needs windows"
        );
        assert_eq!(
            SkipReason::VideoSupportMissing.to_string(),
            "device without video support"
        );
        assert_eq!(
            SkipReason::NoDecoderProfiles.to_string(),
            "adapter reports no decoder profiles"
        );
        assert_eq!(
            SkipReason::NoNv12Target.to_string(),
            "no nv12 render target"
        );
    }

    #[test]
    fn skip_reasons_carry_values() {
        assert_eq!(
            SkipReason::NoHardwareDevice { hresult: -1 }.to_string(),
            "no hardware d3d11 device: 0xffffffff"
        );
        assert_eq!(
            SkipReason::ProfileUnsupported { codec: Codec::Av1 }.to_string(),
            "no av1 decoder profile"
        );
    }

    #[test]
    fn skipped_wraps_reason() {
        let error = D3d11Error::from(SkipReason::NoDecoderProfiles);
        assert_eq!(
            error.to_string(),
            "d3d11 skipped: adapter reports no decoder profiles"
        );
    }

    #[test]
    fn call_names_call_and_status() {
        let error = D3d11Error::Call {
            call: "CreateVideoDecoder",
            hresult: -2_147_220_985,
        };
        assert_eq!(error.to_string(), "CreateVideoDecoder failed: 0x80040207");
    }

    #[test]
    fn stream_failures_name_cause() {
        assert_eq!(
            D3d11Error::Unsupported("interlaced".to_owned()).to_string(),
            "unsupported stream: interlaced"
        );
        assert_eq!(
            D3d11Error::Parse("truncated sps".to_owned()).to_string(),
            "parse failed: truncated sps"
        );
        assert_eq!(
            D3d11Error::BrokenReference("slot 9".to_owned()).to_string(),
            "broken reference list: slot 9"
        );
        assert_eq!(D3d11Error::DpbFull.to_string(), "dpb full");
        assert_eq!(D3d11Error::NoSession.to_string(), "picture before session");
    }

    #[test]
    fn limits_carry_values() {
        let error = D3d11Error::BitstreamTooLarge {
            len: 1_000,
            capacity: 512,
        };
        assert_eq!(
            error.to_string(),
            "bitstream of 1000 bytes exceeds driver buffer 512"
        );
        let error = D3d11Error::OverCaps {
            what: "render targets",
        };
        assert_eq!(
            error.to_string(),
            "stream render targets above adapter caps"
        );
    }
}
