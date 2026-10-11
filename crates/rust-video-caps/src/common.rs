use std::fmt;

/// Conformance check against codec caps.
pub trait Validate {
    /// Check every applicable cap.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    fn validate(&self) -> Result<(), Violation>;
}

/// Chroma sampling.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChromaSampling {
    /// Monochrome.
    Mono,
    /// 4:2:0.
    Yuv420,
    /// 4:2:2.
    Yuv422,
    /// 4:4:0.
    Yuv440,
    /// 4:4:4.
    Yuv444,
}

/// One failed conformance check.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Violation {
    /// Chroma sampling the profile forbids.
    ChromaNotAllowed,
    /// Bit depth above the profile maximum.
    BitDepthNotAllowed {
        /// Requested bits.
        actual: u8,
        /// Profile maximum bits.
        max: u8,
    },
    /// Picture above the level maximum samples.
    PictureTooLarge {
        /// Requested samples.
        actual: u64,
        /// Level maximum samples.
        max: u64,
    },
    /// Axis above the level dimension rule.
    SizeRuleExceeded {
        /// Requested axis samples.
        actual: u32,
        /// Level maximum axis samples.
        max: u64,
    },
    /// Luma samples/s above the level maximum.
    LumaRateExceeded {
        /// Requested samples/s.
        actual: u64,
        /// Level maximum samples/s.
        max: u64,
    },
    /// Bitrate above the level maximum.
    BitrateExceeded {
        /// Requested bps.
        actual: u32,
        /// Level maximum bps.
        max: u64,
    },
    /// CPB above the level maximum.
    CpbExceeded {
        /// Requested bits.
        actual: u32,
        /// Level maximum bits.
        max: u64,
    },
    /// References above the DPB frame count.
    DpbExceeded {
        /// Requested references.
        actual: u8,
        /// DPB frame count.
        max: u32,
    },
    /// Tile columns above the level maximum.
    TileCountExceeded {
        /// Requested columns.
        actual: u16,
        /// Level maximum columns.
        max: u16,
    },
    /// Tiles/s above the level maximum.
    TileRateExceeded {
        /// Requested tiles/s.
        actual: u32,
        /// Level maximum tiles/s.
        max: u32,
    },
    /// Tile below the minimum width.
    TileTooSmall {
        /// Requested width samples.
        actual: u32,
        /// Minimum width samples.
        min: u32,
    },
    /// References above the level maximum.
    RefsExceeded {
        /// Requested references.
        actual: u8,
        /// Level maximum references.
        max: u8,
    },
    /// High tier signaled below its minimum level.
    TierRequired,
    /// Reserved level index.
    ReservedLevel(u8),
    /// Reserved profile idc.
    ReservedProfile(u8),
    /// Axis below the minimum dimension.
    PictureTooSmall {
        /// Requested axis samples.
        axis: u32,
        /// Minimum axis samples.
        min: u32,
    },
    /// Reserved NAL unit type.
    ReservedNal(u8),
    /// Reserved OBU type.
    ObuReserved(u8),
    /// NAL `forbidden_zero_bit` set.
    NalForbiddenBit,
    /// NAL `ref_idc` against the type rule.
    RefIdc {
        /// NAL unit type.
        nal_type: u8,
    },
    /// Slice type the profile forbids.
    SliceNotAllowed(u8),
    /// Entropy coding the profile forbids.
    EntropyNotAllowed,
    /// Flexible macroblock order the profile forbids.
    FmoNotAllowed,
    /// Arbitrary slice order the profile forbids.
    AsoNotAllowed,
    /// Data partitions the profile forbids.
    PartitionNotAllowed,
    /// Superres without the sequence enable.
    SuperresDenied,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChromaNotAllowed => write!(f, "chroma sampling not allowed"),
            Self::BitDepthNotAllowed { actual, max } => {
                write!(f, "bit depth {actual} above maximum {max}")
            }
            Self::PictureTooLarge { actual, max } => {
                write!(f, "picture {actual} samples above maximum {max}")
            }
            Self::SizeRuleExceeded { actual, max } => {
                write!(f, "axis {actual} above maximum {max}")
            }
            Self::LumaRateExceeded { actual, max } => {
                write!(f, "luma rate {actual}/s above maximum {max}/s")
            }
            Self::BitrateExceeded { actual, max } => {
                write!(f, "bitrate {actual} bps above maximum {max} bps")
            }
            Self::CpbExceeded { actual, max } => {
                write!(f, "cpb {actual} bits above maximum {max} bits")
            }
            Self::DpbExceeded { actual, max } => {
                write!(f, "{actual} references above {max} DPB frames")
            }
            Self::TileCountExceeded { actual, max } => {
                write!(f, "{actual} tile columns above maximum {max}")
            }
            Self::TileRateExceeded { actual, max } => {
                write!(f, "{actual} tiles/s above maximum {max}/s")
            }
            Self::TileTooSmall { actual, min } => {
                write!(f, "tile width {actual} below minimum {min}")
            }
            Self::RefsExceeded { actual, max } => {
                write!(f, "{actual} references above maximum {max}")
            }
            Self::TierRequired => write!(f, "high tier below its minimum level"),
            Self::ReservedLevel(index) => write!(f, "reserved level index {index}"),
            Self::ReservedProfile(idc) => write!(f, "reserved profile idc {idc}"),
            Self::PictureTooSmall { axis, min } => {
                write!(f, "axis {axis} below minimum {min}")
            }
            Self::ReservedNal(unit) => write!(f, "reserved NAL unit type {unit}"),
            Self::ObuReserved(unit) => write!(f, "reserved OBU type {unit}"),
            Self::NalForbiddenBit => write!(f, "NAL forbidden_zero_bit set"),
            Self::RefIdc { nal_type } => {
                write!(f, "NAL ref_idc against the rule for type {nal_type}")
            }
            Self::SliceNotAllowed(slice) => {
                write!(f, "slice type {slice} not allowed")
            }
            Self::EntropyNotAllowed => write!(f, "entropy coding not allowed"),
            Self::FmoNotAllowed => write!(f, "FMO not allowed"),
            Self::AsoNotAllowed => write!(f, "ASO not allowed"),
            Self::PartitionNotAllowed => write!(f, "data partitions not allowed"),
            Self::SuperresDenied => write!(f, "superres without sequence enable"),
        }
    }
}

impl std::error::Error for Violation {}

/// Bit depth in bits per sample.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BitDepth(u8);

impl BitDepth {
    /// Sample depth of 8 to 14 bits.
    #[must_use]
    pub fn new(depth: u8) -> Option<Self> {
        (8..=14).contains(&depth).then_some(Self(depth))
    }

    /// Bits per sample.
    #[must_use]
    pub fn bits(&self) -> u8 {
        self.0
    }
}

/// Picture dimensions in luma samples.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PictureSize {
    width: u32,
    height: u32,
}

impl PictureSize {
    /// Dimensions; zero is rejected.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Option<Self> {
        (width > 0 && height > 0).then_some(Self { width, height })
    }

    /// Width in luma samples.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in luma samples.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Luma sample count.
    #[must_use]
    pub fn samples(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Macroblock count in 16x16 units, rounded up.
    #[must_use]
    pub fn macroblocks(&self) -> u32 {
        self.width.div_ceil(16) * self.height.div_ceil(16)
    }

    /// Both axes within the square root of `bound`.
    #[must_use]
    pub fn fits_sqrt_rule(&self, bound: u64) -> bool {
        let max = bound.isqrt();
        u64::from(self.width) <= max && u64::from(self.height) <= max
    }
}

#[cfg(test)]
mod tests {
    use super::{BitDepth, PictureSize};

    #[test]
    fn bit_depth_bounds() {
        assert!(BitDepth::new(7).is_none());
        assert_eq!(BitDepth::new(8).expect("8-bit").bits(), 8);
        assert_eq!(BitDepth::new(14).expect("14-bit").bits(), 14);
        assert!(BitDepth::new(15).is_none());
    }

    #[test]
    fn picture_size_rejects_zero() {
        assert!(PictureSize::new(0, 1080).is_none());
        assert!(PictureSize::new(1920, 0).is_none());
    }

    #[test]
    fn picture_samples_and_blocks() {
        let size = PictureSize::new(1920, 1080).expect("hd");
        assert_eq!(size.samples(), 2_073_600);
        assert_eq!(size.macroblocks(), 120 * 68);
    }

    #[test]
    fn sqrt_rule() {
        let size = PictureSize::new(1920, 1080).expect("hd");
        assert!(size.fits_sqrt_rule(8192 * 256 * 8));
        assert!(!size.fits_sqrt_rule(99 * 256 * 8));
    }
}
