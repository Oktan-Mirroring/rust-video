//! Typed D3D12 tier failures and skip reasons.

use crate::guid::Codec;

use rust_video_caps::frame::PixelFormat;
use rust_video_caps::transcode::FallbackCause;

/// Reference list naming a picture the buffer cannot supply.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum BrokenReferenceReason {
    /// Crop target without a crop rectangle.
    #[error("crop target without crop")]
    CropTargetWithoutCrop,
    /// Marking sweep names an absent picture.
    #[error("marking target absent")]
    MarkingTargetAbsent,
    /// Sliding window finds no short-term picture.
    #[error("no short-term to slide")]
    NoShortTermToSlide,
    /// Move names an absent short-term picture.
    #[error("short-term target absent")]
    ShortTermTargetAbsent,
    /// Slot outside the texture pool.
    #[error("slot outside pool")]
    SlotOutsidePool,
}

/// Hard D3D12 tier failure.
#[derive(Debug, thiserror::Error)]
pub enum D3d12Error {
    /// Machine declined; the tier cannot run here.
    #[error("d3d12 skipped: {0}")]
    Skipped(#[from] SkipReason),
    /// D3D12 call failure.
    #[error("{call} failed: {hresult:#x}")]
    Call {
        /// Failing call name.
        call: &'static str,
        /// Failing call status.
        hresult: i32,
    },
    /// Bitstream the tier refuses: interlaced, depth and chroma outside
    /// 8-bit 4:2:0, or a pending picture builder.
    #[error("unsupported stream: {0}")]
    Unsupported(UnsupportedReason),
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(ParseReason),
    /// Reference list names a picture the buffer cannot supply.
    #[error("broken reference list: {0}")]
    BrokenReference(BrokenReferenceReason),
    /// Picture buffer has no free slot for the next picture.
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

/// Bitstream element the parser rejects.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum ParseReason {
    /// Window outside the coded size.
    #[error("conf_win outside coded size")]
    ConfWinOutsideCodedSize,
    /// Crop empties the picture.
    #[error("crop empties picture")]
    CropEmptiesPicture,
    /// Coded size of zero.
    #[error("empty coded size")]
    EmptyCodedSize,
    /// Crop extends past the coded size.
    #[error("frame_crop outside coded size")]
    FrameCropOutsideCodedSize,
    /// Gap with gaps forbidden by the sequence.
    #[error("frame_num gap with gaps forbidden")]
    FrameNumGapForbidden,
    /// NAL `forbidden_zero_bit` set.
    #[error("nal forbidden bit")]
    NalForbiddenBit,
    /// Non-intra slice inside an IDR picture.
    #[error("non-intra slice in IDR")]
    NonIntraSliceInIdr,
    /// Signed Exp-Golomb code exceeds `i32`.
    #[error("{syntax} overflows i32")]
    OverflowI32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Exp-Golomb code exceeds 32 bits.
    #[error("{syntax} overflows u32")]
    OverflowU32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Slices of one picture disagree.
    #[error("picture slices disagree")]
    PictureSlicesDisagree,
    /// Picture without its first slice.
    #[error("picture without first slice")]
    PictureWithoutFirstSlice,
    /// Picture without slices.
    #[error("picture without slices")]
    PictureWithoutSlices,
    /// Order-count syntax against the sequence method.
    #[error("poc syntax against sequence method")]
    PocSyntaxAgainstSequenceMethod,
    /// Picture set cites an unseen sequence set.
    #[error("pps cites unknown sps")]
    PpsCitesUnknownSps,
    /// Picture set without a body.
    #[error("pps without body")]
    PpsWithoutBody,
    /// Value outside its field width.
    #[error("{syntax} range")]
    Range {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Reference with zero reference frames.
    #[error("reference with zero ref frames")]
    ReferenceWithZeroRefFrames,
    /// Slice cites an unseen picture set.
    #[error("slice cites unknown pps")]
    SliceCitesUnknownPps,
    /// Slice cites an unseen sequence set.
    #[error("slice cites unknown sps")]
    SliceCitesUnknownSps,
    /// Slice header exceeds its length cap.
    #[error("slice header too long")]
    SliceHeaderTooLong,
    /// Slice without a header.
    #[error("slice without header")]
    SliceWithoutHeader,
    /// Sequence set without a body.
    #[error("sps without body")]
    SpsWithoutBody,
    /// Element wider than 32 bits.
    #[error("{syntax} wider than 32 bits")]
    TooWide {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// NAL ends before the element completes.
    #[error("truncated {syntax}")]
    Truncated {
        /// Syntax element under parse.
        syntax: &'static str,
    },
}

/// Machine declined by the D3D12 tier, with the cause.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SkipReason {
    /// No hardware D3D12 device on the machine.
    #[error("no hardware d3d12 device: {hresult:#x}")]
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

/// Bitstream construct the D3D12 tier declines.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum UnsupportedReason {
    /// Slices out of decode order.
    #[error("arbitrary slice order")]
    ArbitrarySliceOrder,
    /// Driver bitstream above the length cap.
    #[error("bitstream too large")]
    BitstreamTooLarge,
    /// Resource call returning null.
    #[error("{call} returned null")]
    CallReturnedNull {
        /// Call returning null.
        call: &'static str,
    },
    /// Chroma depth above 8 bit.
    #[error("chroma depth above 8 bit")]
    ChromaDepthAbove8Bit,
    /// Chroma outside 4:2:0.
    #[error("chroma outside 4:2:0")]
    ChromaOutside420,
    /// Partitioned slices.
    #[error("data partitions")]
    DataPartitions,
    /// Decode without a picture builder.
    #[error("{codec} decode pending")]
    DecodePending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Decode tier 1 hardware.
    #[error("decode tier 1")]
    DecodeTier1,
    /// Dependent slices.
    #[error("dependent slices pending")]
    DependentSlicesPending,
    /// Driver bitstream outside its buffer.
    #[error("driver bitstream outside buffer")]
    DriverBitstreamOutsideBuffer,
    /// NV12 layout of zero extent.
    #[error("empty NV12 layout")]
    EmptyNv12Layout,
    /// Encode without picture arguments.
    #[error("{codec} encode args pending")]
    EncodeArgsPending {
        /// Codec the tier cannot encode yet.
        codec: Codec,
    },
    /// Driver error flags on the picture.
    #[error("encoder error flags {flags:#x}")]
    EncoderErrorFlags {
        /// Driver error flags.
        flags: u64,
    },
    /// Encoder input outside NV12.
    #[error("encoder input {format:?}; nv12 only")]
    EncoderInputNotNv12 {
        /// Rejected input layout.
        format: PixelFormat,
    },
    /// Extent above HEVC level 5.2.
    #[error("extent above level 5.2")]
    ExtentAboveLevel52,
    /// Extent above HEVC level 6.2.
    #[error("extent above level 6.2")]
    ExtentAboveLevel62,
    /// GPU hop miss behind the transcode.
    #[error("{cause}")]
    Fallback {
        /// Failed hop.
        cause: FallbackCause,
    },
    /// Flexible macroblock order.
    #[error("flexible macroblock order")]
    FlexibleMacroblockOrder,
    /// Gap in frame numbers.
    #[error("frame_num gap")]
    FrameNumGap,
    /// Hypothetical reference decoder data.
    #[error("hrd pending")]
    HrdPending,
    /// Inter picture without references.
    #[error("inter picture needs references")]
    InterPictureNeedsReferences,
    /// Interlaced fields.
    #[error("interlaced")]
    Interlaced,
    /// Level outside 5.2.
    #[error("level outside 5.2")]
    LevelOutside52,
    /// Level outside 6.2.
    #[error("level outside 6.2")]
    LevelOutside62,
    /// Long-term references.
    #[error("long-term refs pending")]
    LongTermRefsPending,
    /// Luma depth above 8 bit.
    #[error("luma depth above 8 bit")]
    LumaDepthAbove8Bit,
    /// Packet with more than one picture.
    #[error("multi-picture packet needs per-picture encode")]
    MultiPicturePacketNeedsPerPictureEncode,
    /// Multilayer slices.
    #[error("multilayer")]
    Multilayer,
    /// Multiview slices.
    #[error("multiview")]
    Multiview,
    /// Codec without a decode profile.
    #[error("no {codec} decode profile")]
    NoDecodeProfile {
        /// Codec the adapter cannot decode.
        codec: Codec,
    },
    /// Codec without adapter video encode.
    #[error("no {codec} d3d12 video encode on this adapter")]
    NoVideoEncode {
        /// Codec the adapter cannot encode.
        codec: Codec,
    },
    /// Non-IDR intra without order-count tracking.
    #[error("non-IDR intra needs POC tracking")]
    NonIdrIntraNeedsPocTracking,
    /// NV12 bytes short of the extent.
    #[error("nv12 short of its extent")]
    Nv12ShortOfExtent,
    /// NV12 frame without two planes.
    #[error("nv12 frame without two planes")]
    Nv12WithoutTwoPlanes,
    /// Odd NV12 extent.
    #[error("odd NV12 extent")]
    OddNv12Extent,
    /// Odd visible extent.
    #[error("odd visible extent")]
    OddVisibleExtent,
    /// Pulse-code-modulation blocks.
    #[error("pcm pending")]
    PcmPending,
    /// Decode without picture arguments.
    #[error("{codec} picture args pending")]
    PictureArgsPending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Picture arguments above the length cap.
    #[error("picture args too large")]
    PictureArgsTooLarge,
    /// Decode without a picture builder.
    #[error("{codec} picture builder pending")]
    PictureBuilderPending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Picture above the length cap.
    #[error("picture too large")]
    PictureTooLarge,
    /// Picture without slices.
    #[error("picture without slices")]
    PictureWithoutSlices,
    /// Plane short of its stride.
    #[error("plane short of its stride")]
    PlaneShortOfStride,
    /// Plane stride below the width.
    #[error("plane stride below width")]
    PlaneStrideBelowWidth,
    /// Picture-set extension data.
    #[error("pps extension pending")]
    PpsExtensionPending,
    /// Profile outside main.
    #[error("profile outside main")]
    ProfileOutsideMain,
    /// Readback map returning null.
    #[error("readback map returned null")]
    ReadbackMapReturnedNull,
    /// Readback bytes short of the extent.
    #[error("readback short of extent")]
    ReadbackShortOfExtent,
    /// Redundant pictures.
    #[error("redundant pictures")]
    RedundantPictures,
    /// Reference picture sets.
    #[error("reference sets pending")]
    ReferenceSetsPending,
    /// Reserved slice type.
    #[error("reserved slice type")]
    ReservedSliceType,
    /// Resolved metadata short of its length.
    #[error("resolved metadata short")]
    ResolvedMetadataShort,
    /// Scale source outside the scaler.
    #[error("scale source outside scaler")]
    ScaleSourceOutsideScaler,
    /// Scale target outside even extents.
    #[error("scale target outside even extents")]
    ScaleTargetOutsideEvenExtents,
    /// Scale target outside power-of-two steps.
    #[error("scale target outside power-of-two")]
    ScaleTargetOutsidePowerOfTwo,
    /// Scale target outside the process range.
    #[error("scale target outside process range")]
    ScaleTargetOutsideProcessRange,
    /// Scaling lists.
    #[error("scaling lists pending")]
    ScalingListsPending,
    /// SP/SI slices.
    #[error("sp/si slices")]
    SpSiSlices,
    /// Sequence-set extension data.
    #[error("sps extension pending")]
    SpsExtensionPending,
    /// Staging bytes short of the layout.
    #[error("staging short of its layout")]
    StagingShortOfLayout,
    /// Temporal sublayers.
    #[error("sublayers")]
    Sublayers,
    /// Texture extent outside the session.
    #[error("texture extent outside session")]
    TextureExtentOutsideSession,
    /// Tile grid.
    #[error("tile grid pending")]
    TileGridPending,
    /// Transform bypass.
    #[error("transform bypass")]
    TransformBypass,
    /// Upload map returning null.
    #[error("upload map returned null")]
    UploadMapReturnedNull,
    /// Video process call the driver declines.
    #[error("video process unsupported")]
    VideoProcessUnsupported,
}

#[cfg(test)]
mod tests {
    use super::{BrokenReferenceReason, D3d12Error, ParseReason, SkipReason, UnsupportedReason};
    use crate::guid::Codec;

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
            "no hardware d3d12 device: 0xffffffff"
        );
        assert_eq!(
            SkipReason::ProfileUnsupported { codec: Codec::Av1 }.to_string(),
            "no av1 decoder profile"
        );
    }

    #[test]
    fn skipped_wraps_reason() {
        let error = D3d12Error::from(SkipReason::NoDecoderProfiles);
        assert_eq!(
            error.to_string(),
            "d3d12 skipped: adapter reports no decoder profiles"
        );
    }

    #[test]
    fn call_names_call_and_status() {
        let error = D3d12Error::Call {
            call: "CreateVideoDecoder",
            hresult: -2_147_220_985,
        };
        assert_eq!(error.to_string(), "CreateVideoDecoder failed: 0x80040207");
    }

    #[test]
    fn stream_failures_name_cause() {
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::Interlaced).to_string(),
            "unsupported stream: interlaced"
        );
        assert_eq!(
            D3d12Error::Parse(ParseReason::Truncated { syntax: "sps" }).to_string(),
            "parse failed: truncated sps"
        );
        assert_eq!(
            D3d12Error::BrokenReference(BrokenReferenceReason::SlotOutsidePool).to_string(),
            "broken reference list: slot outside pool"
        );
        assert_eq!(D3d12Error::DpbFull.to_string(), "dpb full");
        assert_eq!(D3d12Error::NoSession.to_string(), "picture before session");
    }

    #[test]
    fn reasons_carry_values() {
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::DecodePending { codec: Codec::Vp9 })
                .to_string(),
            "unsupported stream: vp9 decode pending"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::NoVideoEncode { codec: Codec::H264 })
                .to_string(),
            "unsupported stream: no h264 d3d12 video encode on this adapter"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::PictureArgsPending { codec: Codec::Vp9 })
                .to_string(),
            "unsupported stream: vp9 picture args pending"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::PictureBuilderPending { codec: Codec::Av1 })
                .to_string(),
            "unsupported stream: av1 picture builder pending"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::EncoderInputNotNv12 {
                format: PixelFormat::Rgba8
            })
            .to_string(),
            "unsupported stream: encoder input Rgba8; nv12 only"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::EncoderErrorFlags { flags: 3 }).to_string(),
            "unsupported stream: encoder error flags 0x3"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::CallReturnedNull {
                call: "CreateCommittedResource"
            })
            .to_string(),
            "unsupported stream: CreateCommittedResource returned null"
        );
        assert_eq!(
            D3d12Error::Unsupported(UnsupportedReason::Fallback {
                cause: FallbackCause::NoSharedExport
            })
            .to_string(),
            "unsupported stream: decode texture without share handle"
        );
        assert_eq!(
            D3d12Error::Parse(ParseReason::Range {
                syntax: "frame_num"
            })
            .to_string(),
            "parse failed: frame_num range"
        );
        assert_eq!(
            D3d12Error::Parse(ParseReason::OverflowU32 { syntax: "pps" }).to_string(),
            "parse failed: pps overflows u32"
        );
    }

    #[test]
    fn limits_carry_values() {
        let error = D3d12Error::BitstreamTooLarge {
            len: 1_000,
            capacity: 512,
        };
        assert_eq!(
            error.to_string(),
            "bitstream of 1000 bytes exceeds driver buffer 512"
        );
        let error = D3d12Error::OverCaps {
            what: "render targets",
        };
        assert_eq!(
            error.to_string(),
            "stream render targets above adapter caps"
        );
    }
}
