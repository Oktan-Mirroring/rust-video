//! Typed D3D11 tier failures and skip reasons.

use crate::guid::Codec;

use rust_video_caps::frame::PixelFormat;
use rust_video_caps::transcode::FallbackCause;

/// Reference list naming a picture the DPB cannot supply.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum BrokenReferenceReason {
    /// Marking sweep names an absent picture.
    #[error("marking target absent")]
    MarkingTargetAbsent,
    /// Reference names an empty slot.
    #[error("{reference} names empty slot")]
    NamesEmptySlot {
        /// Reference naming the empty slot.
        reference: &'static str,
    },
    /// Sliding window finds no short-term picture.
    #[error("no short-term to slide")]
    NoShortTermToSlide,
    /// Reference POC absent from the buffer.
    #[error("reference poc {poc} absent")]
    ReferencePocAbsent {
        /// Absent picture order count.
        poc: i32,
    },
    /// Move names an absent short-term picture.
    #[error("short-term target absent")]
    ShortTermTargetAbsent,
    /// Slot outside the view array.
    #[error("slot outside views")]
    SlotOutsideViews,
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
    /// Conformance window outside the coded size.
    #[error("conformance_window outside coded size")]
    ConformanceWindowOutsideCodedSize,
    /// Crop empties the picture.
    #[error("crop empties picture")]
    CropEmptiesPicture,
    /// Frame before its sequence header.
    #[error("frame before sequence header")]
    FrameBeforeSequenceHeader,
    /// Crop extends past the coded size.
    #[error("frame_crop outside coded size")]
    FrameCropOutsideCodedSize,
    /// Gap with gaps forbidden by the sequence.
    #[error("frame_num gap with gaps forbidden")]
    FrameNumGapForbidden,
    /// Frame size above the sequence maximum.
    #[error("frame size above sequence maximum")]
    FrameSizeAboveSequenceMaximum,
    /// Frame without tile data.
    #[error("frame without tile data")]
    FrameWithoutTileData,
    /// Frame without tiles.
    #[error("frame without tiles")]
    FrameWithoutTiles,
    /// Tile group short of its tiles.
    #[error("incomplete tile group")]
    IncompleteTileGroup,
    /// NAL `forbidden_zero_bit` set.
    #[error("nal forbidden bit")]
    NalForbiddenBit,
    /// NAL temporal id zero.
    #[error("nal temporal id")]
    NalTemporalId,
    /// Non-intra slice inside an IDR picture.
    #[error("non-intra slice in IDR")]
    NonIntraSliceInIdr,
    /// OBU extension reserved bits set.
    #[error("obu extension reserved bits")]
    ObuExtensionReservedBits,
    /// OBU `forbidden_zero_bit` set.
    #[error("obu forbidden bit")]
    ObuForbiddenBit,
    /// OBU reserved bit set.
    #[error("obu reserved bit")]
    ObuReservedBit,
    /// OBU size past its payload.
    #[error("obu size exceeds payload")]
    ObuSizeExceedsPayload,
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
    /// Picture without an order count.
    #[error("picture without poc")]
    PictureWithoutPoc,
    /// Picture without a reference set.
    #[error("picture without reference set")]
    PictureWithoutReferenceSet,
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
    /// Redundant header against the first.
    #[error("redundant header mismatch")]
    RedundantHeaderMismatch,
    /// Reference picture absent from the buffer.
    #[error("reference picture missing")]
    ReferencePictureMissing,
    /// Reference with zero reference frames.
    #[error("reference with zero ref frames")]
    ReferenceWithZeroRefFrames,
    /// Reserved OBU type.
    #[error("reserved obu type")]
    ReservedObuType,
    /// Short-term set past its size.
    #[error("short_term_ref_pic_set size")]
    ShortTermRefPicSetSize,
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
    /// Sequence set cites an unseen video set.
    #[error("sps cites unknown vps")]
    SpsCitesUnknownVps,
    /// Sequence set without a body.
    #[error("sps without body")]
    SpsWithoutBody,
    /// Tile grid short of the frame.
    #[error("tile grid coverage")]
    TileGridCoverage,
    /// Tile groups out of order.
    #[error("tile group order")]
    TileGroupOrder,
    /// Tile group without data.
    #[error("tile group without data")]
    TileGroupWithoutData,
    /// Tile group without a frame header.
    #[error("tile group without frame header")]
    TileGroupWithoutFrameHeader,
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
    /// Video set without a body.
    #[error("vps without body")]
    VpsWithoutBody,
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
    /// Slices out of decode order.
    #[error("arbitrary slice order")]
    ArbitrarySliceOrder,
    /// Async transform without an attribute store.
    #[error("async MFT without an attribute store")]
    AsyncMftWithoutAttributeStore,
    /// Async transform without an event pump.
    #[error("async MFT without an event pump")]
    AsyncMftWithoutEventPump,
    /// AV1 bit depth above the 8-bit target.
    #[error("av1 {depth}-bit needs a {depth}-bit target")]
    Av1BitDepthNeedsTarget {
        /// Sequence bit depth.
        depth: u8,
    },
    /// AV1 chroma outside 4:2:0.
    #[error("av1 chroma outside 4:2:0")]
    Av1ChromaOutside420,
    /// AV1 monochrome.
    #[error("av1 monochrome")]
    Av1Monochrome,
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
    /// Transcode without surface decode.
    #[error("transcode from {codec} without surface decode")]
    DecodeWithoutSurfaceDecode {
        /// Codec without surface decode.
        codec: Codec,
    },
    /// Dependent slice segments.
    #[error("dependent slice segments")]
    DependentSliceSegments,
    /// Encoder input outside NV12.
    #[error("encoder input {format:?}; nv12 only")]
    EncoderInputNotNv12 {
        /// Rejected input layout.
        format: PixelFormat,
    },
    /// Encoder without GPU texture import.
    #[error("encoder without gpu import")]
    EncoderWithoutGpuImport,
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
    /// Frame above the 4 GiB sample cap.
    #[error("frame over 4 GiB")]
    FrameOver4Gib,
    /// Interlaced fields.
    #[error("interlaced")]
    Interlaced,
    /// Luma depth above 8 bit.
    #[error("luma depth above 8 bit")]
    LumaDepthAbove8Bit,
    /// Multilayer stream.
    #[error("multilayer")]
    Multilayer,
    /// Multiview slices.
    #[error("multiview")]
    Multiview,
    /// Codec without an encoder transform.
    #[error("no {codec} encoder MFT on this machine")]
    NoEncoderMft {
        /// Codec the machine cannot encode.
        codec: Codec,
    },
    /// Codec without a decode session.
    #[error("no d3d11 session for {codec}")]
    NoSessionForCodec {
        /// Codec the tier cannot open.
        codec: Codec,
    },
    /// NV12 bytes short of the extent.
    #[error("nv12 short of its extent")]
    Nv12ShortOfExtent,
    /// NV12 frame without two planes.
    #[error("nv12 frame without two planes")]
    Nv12WithoutTwoPlanes,
    /// Odd NV12 extent.
    #[error("odd NV12 extent")]
    OddNv12Extent,
    /// Packed NV12 length outside its extent.
    #[error("packed NV12 length outside its extent")]
    PackedNv12OutsideExtent,
    /// PCM depth above 8 bit.
    #[error("pcm depth above 8 bit")]
    PcmDepthAbove8Bit,
    /// Decode without a picture builder.
    #[error("{codec} picture builder pending")]
    PictureBuilderPending {
        /// Codec the tier cannot decode yet.
        codec: Codec,
    },
    /// Plane short of its stride.
    #[error("plane short of its stride")]
    PlaneShortOfStride,
    /// Plane stride below the width.
    #[error("plane stride below width")]
    PlaneStrideBelowWidth,
    /// Picture-set extension.
    #[error("pps extension")]
    PpsExtension,
    /// Redundant pictures.
    #[error("redundant pictures")]
    RedundantPictures,
    /// Render size against the frame size.
    #[error("render size differs from frame size")]
    RenderSizeDiffersFromFrameSize,
    /// Scale source outside the scaler.
    #[error("scale source outside scaler")]
    ScaleSourceOutsideScaler,
    /// SP/SI slices.
    #[error("sp/si slices")]
    SpSiSlices,
    /// Spatial layers.
    #[error("spatial layers")]
    SpatialLayers,
    /// Sequence-set extension.
    #[error("sps extension")]
    SpsExtension,
    /// Poisoned submit lock.
    #[error("submit lock poisoned")]
    SubmitLockPoisoned,
    /// Superres upscale.
    #[error("superres upscale")]
    SuperresUpscale,
    /// Tile count above 256.
    #[error("tile count above 256")]
    TileCountAbove256,
    /// Tile grid above the driver grid.
    #[error("tile grid above the driver grid")]
    TileGridAboveDriverGrid,
    /// Tile list.
    #[error("tile list")]
    TileList,
    /// Transform bypass.
    #[error("transform bypass")]
    TransformBypass,
    /// Video processor without NV12 scale.
    #[error("video processor without nv12 scale")]
    VideoProcessorWithoutNv12Scale,
    /// Video processor without RGBA convert input.
    #[error("video processor without rgba convert")]
    VideoProcessorWithoutRgbaConvert,
}

#[cfg(test)]
mod tests {
    use super::{BrokenReferenceReason, D3d11Error, ParseReason, SkipReason, UnsupportedReason};
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
            D3d11Error::Unsupported(UnsupportedReason::Interlaced).to_string(),
            "unsupported stream: interlaced"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::Truncated { syntax: "sps" }).to_string(),
            "parse failed: truncated sps"
        );
        assert_eq!(
            D3d11Error::BrokenReference(BrokenReferenceReason::SlotOutsideViews).to_string(),
            "broken reference list: slot outside views"
        );
        assert_eq!(D3d11Error::DpbFull.to_string(), "dpb full");
        assert_eq!(D3d11Error::NoFreeSlot.to_string(), "no free surface slot");
        assert_eq!(D3d11Error::NoSession.to_string(), "picture before session");
    }

    #[test]
    fn reasons_carry_values() {
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::DecodePending { codec: Codec::Hevc })
                .to_string(),
            "unsupported stream: hevc decode pending"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::NoEncoderMft { codec: Codec::Av1 })
                .to_string(),
            "unsupported stream: no av1 encoder MFT on this machine"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::PictureBuilderPending { codec: Codec::Vp9 })
                .to_string(),
            "unsupported stream: vp9 picture builder pending"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::EncoderInputNotNv12 {
                format: PixelFormat::Rgba8
            })
            .to_string(),
            "unsupported stream: encoder input Rgba8; nv12 only"
        );
        assert_eq!(
            D3d11Error::Unsupported(UnsupportedReason::Fallback {
                cause: FallbackCause::NoSharedExport
            })
            .to_string(),
            "unsupported stream: decode texture without share handle"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::Range {
                syntax: "frame_num"
            })
            .to_string(),
            "parse failed: frame_num range"
        );
        assert_eq!(
            D3d11Error::Parse(ParseReason::OverflowU32 { syntax: "pps" }).to_string(),
            "parse failed: pps overflows u32"
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
