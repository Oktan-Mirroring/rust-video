//! Typed D3D11 tier failures and skip reasons.

use crate::guid::Codec;

use rust_video_caps::encode::Decline;
use rust_video_caps::reason::{
    BrokenReferenceReason as BrokenCore, ParseReason as ParseCore, UnsupportedReason as StreamCore,
};

/// Reference list naming a picture the DPB cannot supply.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum BrokenReferenceReason {
    /// Shared broken-reference core.
    #[error("{0}")]
    Broken(BrokenCore),
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
    Unsupported(UnsupportedReason),
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(ParseReason),
    /// Reference list names a picture the DPB cannot supply.
    #[error("broken reference list: {0}")]
    BrokenReference(BrokenReferenceReason),
    /// Encode config the tier declines.
    #[error("encode declined: {0}")]
    Declined(#[from] Decline),
    /// DPB has no free slot for the next picture.
    #[error("dpb full")]
    DpbFull,
    /// Live surfaces pin every decoder slot.
    #[error("no free surface slot")]
    NoFreeSlot,
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

/// Bitstream element the parser rejects.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum ParseReason {
    /// Shared parse core.
    #[error("{0}")]
    Parse(ParseCore),
}

/// Machine declined by the D3D11 tier, with the cause.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SkipReason {
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

/// Bitstream construct the D3D11 tier declines.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum UnsupportedReason {
    /// Async transform without an attribute store.
    #[error("async MFT without an attribute store")]
    AsyncMftWithoutAttributeStore,
    /// Async transform without an event pump.
    #[error("async MFT without an event pump")]
    AsyncMftWithoutEventPump,
    /// Decode without a picture builder.
    #[error("{codec} decode pending")]
    DecodePending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Transcode without surface decode.
    #[error("transcode from {codec} without surface decode")]
    DecodeWithoutSurfaceDecode {
        /// Codec without surface decode.
        codec: Codec,
    },
    /// Encoder without GPU texture import.
    #[error("encoder without gpu import")]
    EncoderWithoutGpuImport,
    /// Gap in frame numbers.
    #[error("frame_num gap")]
    FrameNumGap,
    /// Frame above the 4 GiB sample cap.
    #[error("frame over 4 GiB")]
    FrameOver4Gib,
    /// Codec without an encoder transform.
    #[error("no {codec} encoder MFT on this machine")]
    NoEncoder {
        /// Codec the machine cannot encode.
        codec: Codec,
    },
    /// Codec without a decode session.
    #[error("no d3d11 session for {codec}")]
    NoSessionForCodec {
        /// Codec the tier cannot open.
        codec: Codec,
    },
    /// Decode without a picture builder.
    #[error("{codec} picture builder pending")]
    PictureBuilderPending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Shared stream-decline core.
    #[error("{0}")]
    Stream(StreamCore),
    /// Poisoned submit lock.
    #[error("submit lock poisoned")]
    SubmitLockPoisoned,
    /// Tile grid above the driver grid.
    #[error("tile grid above the driver grid")]
    TileGridAboveDriverGrid,
    /// Video processor without NV12 scale.
    #[error("video processor without nv12 scale")]
    VideoProcessorWithoutNv12Scale,
    /// Video processor without RGBA convert input.
    #[error("video processor without rgba convert")]
    VideoProcessorWithoutRgbaConvert,
}

#[cfg(test)]
mod tests {
    use super::{BrokenCore, ParseCore, StreamCore};
    use super::{BrokenReferenceReason, D3d11Error, ParseReason, SkipReason, UnsupportedReason};
    use crate::guid::Codec;

    use rust_video_caps::encode::Decline;
    use rust_video_caps::frame::PixelFormat;
    use rust_video_caps::transcode::FallbackCause;

    #[test]
    fn skip_reasons_name_cause() {
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
            D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::Interlaced)).to_string(),
            "unsupported stream: interlaced"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::Parse(ParseCore::Truncated { syntax: "sps" }))
                .to_string(),
            "parse failed: truncated sps"
        );
        assert_eq!(
            D3d11Error::BrokenReference(BrokenReferenceReason::Broken(BrokenCore::SlotOutsidePool))
                .to_string(),
            "broken reference list: slot outside pool"
        );
        assert_eq!(D3d11Error::DpbFull.to_string(), "dpb full");
        assert_eq!(D3d11Error::NoFreeSlot.to_string(), "no free surface slot");
        assert_eq!(D3d11Error::NoSession.to_string(), "picture before session");
    }

    #[test]
    fn reasons_carry_values() {
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::DecodePending { codec: Codec::H265 })
                .to_string(),
            "unsupported stream: h265 decode pending"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::NoEncoder { codec: Codec::Av1 }).to_string(),
            "unsupported stream: no av1 encoder MFT on this machine"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::PictureBuilderPending { codec: Codec::Vp9 })
                .to_string(),
            "unsupported stream: vp9 picture builder pending"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::EncoderInputNotNv12 {
                format: PixelFormat::Rgba8
            }))
            .to_string(),
            "unsupported stream: encoder input Rgba8; nv12 only"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::Stream(StreamCore::Fallback {
                cause: FallbackCause::NoSharedExport
            }))
            .to_string(),
            "unsupported stream: decode texture without share handle"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::Parse(ParseCore::Range {
                syntax: "frame_num"
            }))
            .to_string(),
            "parse failed: frame_num range"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::Parse(ParseCore::OverflowU32 { syntax: "pps" }))
                .to_string(),
            "parse failed: pps overflows u32"
        );
    }

    #[test]
    fn declined_names_decline() {
        let error = D3d11Error::from(Decline::HdrUnsupported);
        assert_eq!(
            error.to_string(),
            "encode declined: HDR metadata on SDR tier unsupported"
        );
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
