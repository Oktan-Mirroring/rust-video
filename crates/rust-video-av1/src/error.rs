//! Typed AV1 grammar failures.

/// Reference list naming a picture the slots cannot supply.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum BrokenReferenceKind {
    /// Reference names an empty slot.
    #[error("{reference} names empty slot")]
    NamesEmptySlot {
        /// Reference naming the empty slot.
        reference: &'static str,
    },
}

pub use self::BrokenReferenceKind as BrokenReferenceReason;

/// AV1 grammar failure.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum Error {
    /// Bitstream the grammar declines: depth and chroma outside 8-bit
    /// 4:2:0, spatial layers, superres upscale, or tile lists.
    #[error("unsupported stream: {0}")]
    Unsupported(UnsupportedKind),
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(ParseKind),
    /// Reference list names a picture the slots cannot supply.
    #[error("broken reference list: {0}")]
    BrokenReference(BrokenReferenceKind),
}

/// Bitstream element the parser rejects.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum ParseKind {
    /// Frame before its sequence header.
    #[error("frame before sequence header")]
    FrameBeforeSequenceHeader,
    /// Frame without tile data.
    #[error("frame without tile data")]
    FrameWithoutTileData,
    /// Tile group short of its tiles.
    #[error("incomplete tile group")]
    IncompleteTileGroup,
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
    /// Signed value exceeds `i32`.
    #[error("{syntax} overflows i32")]
    OverflowI32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Variable-length code exceeds 32 bits.
    #[error("{syntax} overflows u32")]
    OverflowU32 {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Value outside its field width.
    #[error("{syntax} range")]
    Range {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Redundant header against the first.
    #[error("redundant header mismatch")]
    RedundantHeaderMismatch,
    /// Reserved OBU type.
    #[error("reserved obu type")]
    ReservedObuType,
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
    /// Payload ends before the element completes.
    #[error("truncated {syntax}")]
    Truncated {
        /// Syntax element under parse.
        syntax: &'static str,
    },
}

pub use self::ParseKind as ParseReason;

/// Bitstream construct the grammar declines.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum UnsupportedKind {
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
    /// Render size against the frame size.
    #[error("render size differs from frame size")]
    RenderSizeDiffersFromFrameSize,
    /// Spatial layers.
    #[error("spatial layers")]
    SpatialLayers,
    /// Superres upscale.
    #[error("superres upscale")]
    SuperresUpscale,
    /// Tile count above 256.
    #[error("tile count above 256")]
    TileCountAbove256,
    /// Tile list.
    #[error("tile list")]
    TileList,
}

pub use self::UnsupportedKind as UnsupportedReason;

impl Error {
    /// Parse failure.
    ///
    /// # Returns
    ///
    /// `kind` wrapped as the caller's error type.
    #[must_use]
    pub fn parse<E: From<Self>>(kind: ParseKind) -> E {
        Self::Parse(kind).into()
    }

    /// Declined construct.
    ///
    /// # Returns
    ///
    /// `kind` wrapped as the caller's error type.
    #[must_use]
    pub fn unsupported<E: From<Self>>(kind: UnsupportedKind) -> E {
        Self::Unsupported(kind).into()
    }

    /// Broken reference list.
    ///
    /// # Returns
    ///
    /// `kind` wrapped as the caller's error type.
    #[must_use]
    pub fn broken<E: From<Self>>(kind: BrokenReferenceKind) -> E {
        Self::BrokenReference(kind).into()
    }
}
