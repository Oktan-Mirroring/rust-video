//! Shared error cores behind tier errors.
//!
//! Grammar-derived reasons tiers wrap in their own error types.

use crate::frame::PixelFormat;
use crate::transcode::FallbackCause;

/// Reference list naming a picture the buffer cannot supply.
#[non_exhaustive]
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
    /// Slot outside the texture pool.
    #[error("slot outside pool")]
    SlotOutsidePool,
}

/// Bitstream element the parser rejects.
#[non_exhaustive]
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
    /// Slice cites a short-term table missing from the sequence.
    #[error("short-term table missing from sequence")]
    EmptySpsRps,
    /// Frame before its sequence header.
    #[error("frame before sequence header")]
    FrameBeforeSequenceHeader,
    /// Crop extends past the coded size.
    #[error("frame_crop outside coded size")]
    FrameCropOutsideCodedSize,
    /// Frame marker against the codec bits.
    #[error("frame marker mismatch")]
    FrameMarkerMismatch,
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
    /// Sequence long-term table past its size.
    #[error("sequence long-term table past its size")]
    LtSpsOversize,
    /// Slice long-term pictures past their size.
    #[error("slice long-term pictures past their size")]
    LtTotalOversize,
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
    /// Picture without its first slice.
    #[error("picture without first slice")]
    PictureWithoutFirstSlice,
    /// Picture without an order count.
    #[error("picture without poc")]
    PictureWithoutPoc,
    /// Picture without a reference set.
    #[error("picture without reference set")]
    PictureWithoutReferenceSet,
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
    /// Reserved profile bits.
    #[error("reserved profile")]
    ReservedProfile,
    /// Short-term table past its size.
    #[error("short-term table past its size")]
    RpsTableOversize,
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
    /// Sync code against the codec bytes.
    #[error("sync code mismatch")]
    SyncCodeMismatch,
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

/// Bitstream construct a tier declines.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum UnsupportedReason {
    /// Slices out of decode order.
    #[error("arbitrary slice order")]
    ArbitrarySliceOrder,
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
    /// Dependent slices.
    #[error("dependent slices")]
    DependentSlices,
    /// Encoder input outside NV12.
    #[error("encoder input {format:?}; nv12 only")]
    EncoderInputNotNv12 {
        /// Rejected input layout.
        format: PixelFormat,
    },
    /// GPU hop miss behind the transcode.
    #[error("{cause}")]
    Fallback {
        /// Failed hop.
        cause: FallbackCause,
    },
    /// Flexible macroblock order.
    #[error("flexible macroblock order")]
    FlexibleMacroblockOrder,
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
    /// Plane short of its stride.
    #[error("plane short of its stride")]
    PlaneShortOfStride,
    /// Plane stride below the width.
    #[error("plane stride below width")]
    PlaneStrideBelowWidth,
    /// Picture-set extension.
    #[error("pps extension")]
    PpsExtension,
    /// Profile outside main.
    #[error("profile outside main")]
    ProfileOutsideMain,
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
    /// Temporal sublayers.
    #[error("sublayers")]
    Sublayers,
    /// Superres upscale.
    #[error("superres upscale")]
    SuperresUpscale,
    /// Tile count above 256.
    #[error("tile count above 256")]
    TileCountAbove256,
    /// Tile list.
    #[error("tile list")]
    TileList,
    /// Transform bypass.
    #[error("transform bypass")]
    TransformBypass,
}

#[cfg(test)]
mod tests {
    use super::{BrokenReferenceReason, ParseReason, UnsupportedReason};
    use crate::frame::PixelFormat;

    #[test]
    fn reason_strings_match_tiers() {
        assert_eq!(
            ParseReason::ConfWinOutsideCodedSize.to_string(),
            "conf_win outside coded size"
        );
        assert_eq!(
            ParseReason::Truncated { syntax: "sps" }.to_string(),
            "truncated sps"
        );
        assert_eq!(
            ParseReason::ReferencePictureMissing.to_string(),
            "reference picture missing"
        );
        assert_eq!(
            ParseReason::PictureWithoutSlices.to_string(),
            "picture without slices"
        );
        assert_eq!(
            UnsupportedReason::DependentSlices.to_string(),
            "dependent slices"
        );
        assert_eq!(UnsupportedReason::SpsExtension.to_string(), "sps extension");
        assert_eq!(
            UnsupportedReason::EncoderInputNotNv12 {
                format: PixelFormat::Rgba8
            }
            .to_string(),
            "encoder input Rgba8; nv12 only"
        );
        assert_eq!(
            BrokenReferenceReason::SlotOutsidePool.to_string(),
            "slot outside pool"
        );
        assert_eq!(
            BrokenReferenceReason::NamesEmptySlot { reference: "L0" }.to_string(),
            "L0 names empty slot"
        );
    }

    #[test]
    fn reasons_are_errors() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<ParseReason>();
        assert_error::<UnsupportedReason>();
        assert_error::<BrokenReferenceReason>();
    }
}
