//! VP9 profile, level, tile, and superframe enforcement.
//!
//! Values from the VP9 v0.7 specification and the webmproject levels
//! page; cites name the section.

use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

/// Maximum tile width in samples, 64 superblocks (S3).
pub const MAX_TILE_WIDTH: u32 = 4096;

/// Superframe index maximum bytes (S5.26).
pub const SUPERFRAME_INDEX_MAX: u8 = 34;

/// Superframe marker 0b110 (S5.26).
pub const SUPERFRAME_MARKER: u8 = 0b110;

/// Maximum superframe frames (S5.26).
pub const SUPERFRAME_MAX_FRAMES: u8 = 8;

/// VP9 level (levels page).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// Level 1.
    L1,
    /// Level 1.1.
    L11,
    /// Level 2.
    L2,
    /// Level 2.1.
    L21,
    /// Level 3.
    L3,
    /// Level 3.1.
    L31,
    /// Level 4.
    L4,
    /// Level 4.1.
    L41,
    /// Level 5.
    L5,
    /// Level 5.1.
    L51,
    /// Level 5.2.
    L52,
    /// Level 6.
    L6,
    /// Level 6.1.
    L61,
    /// Level 6.2.
    L62,
}

impl Level {
    /// Levels-page rows in level order.
    #[rustfmt::skip]
    const CAPS: [LevelCaps; 14] = [
        LevelCaps { sr: 829_440, pic: 36_864, br: 200, cpb: Some(400), min_cr: 2, tiles: 1, alt_ref: 4, refs: 8, dim: 512 },
        LevelCaps { sr: 2_764_800, pic: 73_728, br: 800, cpb: Some(1000), min_cr: 2, tiles: 1, alt_ref: 4, refs: 8, dim: 768 },
        LevelCaps { sr: 4_608_000, pic: 122_880, br: 1800, cpb: Some(1500), min_cr: 2, tiles: 1, alt_ref: 4, refs: 8, dim: 960 },
        LevelCaps { sr: 9_216_000, pic: 245_760, br: 3600, cpb: Some(2800), min_cr: 2, tiles: 2, alt_ref: 4, refs: 8, dim: 1344 },
        LevelCaps { sr: 20_736_000, pic: 552_960, br: 7200, cpb: Some(6000), min_cr: 2, tiles: 4, alt_ref: 4, refs: 8, dim: 2048 },
        LevelCaps { sr: 36_864_000, pic: 983_040, br: 12_000, cpb: Some(10_000), min_cr: 2, tiles: 4, alt_ref: 4, refs: 8, dim: 2752 },
        LevelCaps { sr: 83_558_400, pic: 2_228_224, br: 18_000, cpb: Some(16_000), min_cr: 4, tiles: 4, alt_ref: 4, refs: 8, dim: 4160 },
        LevelCaps { sr: 160_432_128, pic: 2_228_224, br: 30_000, cpb: Some(18_000), min_cr: 4, tiles: 4, alt_ref: 5, refs: 6, dim: 4160 },
        LevelCaps { sr: 311_951_360, pic: 8_912_896, br: 60_000, cpb: Some(36_000), min_cr: 6, tiles: 8, alt_ref: 6, refs: 4, dim: 8384 },
        LevelCaps { sr: 588_251_136, pic: 8_912_896, br: 120_000, cpb: Some(46_000), min_cr: 8, tiles: 8, alt_ref: 10, refs: 4, dim: 8384 },
        LevelCaps { sr: 1_176_502_272, pic: 8_912_896, br: 180_000, cpb: None, min_cr: 8, tiles: 8, alt_ref: 10, refs: 4, dim: 8384 },
        LevelCaps { sr: 1_176_502_272, pic: 35_651_584, br: 180_000, cpb: None, min_cr: 8, tiles: 16, alt_ref: 10, refs: 4, dim: 16_832 },
        LevelCaps { sr: 2_353_004_544, pic: 35_651_584, br: 240_000, cpb: None, min_cr: 8, tiles: 16, alt_ref: 10, refs: 4, dim: 16_832 },
        LevelCaps { sr: 4_706_009_088, pic: 35_651_584, br: 480_000, cpb: None, min_cr: 8, tiles: 16, alt_ref: 10, refs: 4, dim: 16_832 },
    ];
    /// Resolve 0-based level ordinal; 14 and up are undefined.
    ///
    /// # Errors
    ///
    /// Returns `ReservedLevel` for undefined ordinals.
    ///
    /// # Arguments
    ///
    /// * `idx` - 0-based level ordinal in table order.
    pub fn from_idx(idx: u8) -> Result<Self, Violation> {
        match idx {
            0 => Ok(Self::L1),
            1 => Ok(Self::L11),
            2 => Ok(Self::L2),
            3 => Ok(Self::L21),
            4 => Ok(Self::L3),
            5 => Ok(Self::L31),
            6 => Ok(Self::L4),
            7 => Ok(Self::L41),
            8 => Ok(Self::L5),
            9 => Ok(Self::L51),
            10 => Ok(Self::L52),
            11 => Ok(Self::L6),
            12 => Ok(Self::L61),
            13 => Ok(Self::L62),
            reserved => Err(Violation::ReservedLevel(reserved)),
        }
    }

    /// Level caps row (levels page); every level carries one.
    #[must_use]
    pub fn caps(self) -> Option<&'static LevelCaps> {
        match self {
            Self::L1 => Some(&Self::CAPS[0]),
            Self::L11 => Some(&Self::CAPS[1]),
            Self::L2 => Some(&Self::CAPS[2]),
            Self::L21 => Some(&Self::CAPS[3]),
            Self::L3 => Some(&Self::CAPS[4]),
            Self::L31 => Some(&Self::CAPS[5]),
            Self::L4 => Some(&Self::CAPS[6]),
            Self::L41 => Some(&Self::CAPS[7]),
            Self::L5 => Some(&Self::CAPS[8]),
            Self::L51 => Some(&Self::CAPS[9]),
            Self::L52 => Some(&Self::CAPS[10]),
            Self::L6 => Some(&Self::CAPS[11]),
            Self::L61 => Some(&Self::CAPS[12]),
            Self::L62 => Some(&Self::CAPS[13]),
        }
    }
}

/// VP9 profile (spec S7.2, S6.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// P0: 8-bit 4:2:0.
    P0,
    /// P1: 8-bit 4:2:2/4:4:0/4:4:4.
    P1,
    /// P2: 10/12-bit 4:2:0.
    P2,
    /// P3: 10/12-bit 4:2:2/4:4:0/4:4:4.
    P3,
}

impl Profile {
    /// Resolve 2-bit profile idc 0-3.
    ///
    /// # Arguments
    ///
    /// * `idc` - profile idc value.
    #[must_use]
    pub fn from_idc(idc: u8) -> Option<Self> {
        match idc {
            0 => Some(Self::P0),
            1 => Some(Self::P1),
            2 => Some(Self::P2),
            3 => Some(Self::P3),
            _ => None,
        }
    }

    /// Maximum bit depth.
    #[must_use]
    pub fn max_bit_depth(self) -> u8 {
        match self {
            Self::P0 | Self::P1 => 8,
            Self::P2 | Self::P3 => 12,
        }
    }

    /// Profile capability matrix (S7.2, S6.2, S6.2.2).
    #[must_use]
    pub fn allows(self, depth_bits: u8, chroma: ChromaSampling) -> bool {
        match self {
            Self::P0 => depth_bits == 8 && chroma == ChromaSampling::Yuv420,
            Self::P1 => {
                depth_bits == 8
                    && matches!(
                        chroma,
                        ChromaSampling::Yuv422 | ChromaSampling::Yuv440 | ChromaSampling::Yuv444
                    )
            }
            Self::P2 => matches!(depth_bits, 10 | 12) && chroma == ChromaSampling::Yuv420,
            Self::P3 => {
                matches!(depth_bits, 10 | 12)
                    && matches!(
                        chroma,
                        ChromaSampling::Yuv422 | ChromaSampling::Yuv440 | ChromaSampling::Yuv444
                    )
            }
        }
    }

    /// Allowed chroma sampling at any depth (S7.2, S6.2).
    #[must_use]
    pub fn allows_chroma(self, chroma: ChromaSampling) -> bool {
        match self {
            Self::P0 | Self::P2 => chroma == ChromaSampling::Yuv420,
            Self::P1 | Self::P3 => matches!(
                chroma,
                ChromaSampling::Yuv422 | ChromaSampling::Yuv440 | ChromaSampling::Yuv444
            ),
        }
    }
}

/// One levels-page row: sample rate, picture, bitrate kbps, CPB kbits,
/// `MinCR`, max tiles, min alt-ref distance, max refs, max dimension.
pub struct LevelCaps {
    /// Max luma sample rate.
    pub sr: u64,
    /// Max luma picture samples.
    pub pic: u32,
    /// Max bitrate kbps.
    pub br: u32,
    /// Max CPB kbits; TBD on the page from 5.2 up.
    pub cpb: Option<u32>,
    /// Min compression ratio.
    pub min_cr: u8,
    /// Max tiles.
    pub tiles: u8,
    /// Min alt-ref distance.
    pub alt_ref: u8,
    /// Max reference frames.
    pub refs: u8,
    /// Max width/height samples.
    pub dim: u16,
}

/// VP9 coding parameters.
pub struct Params {
    /// Profile.
    pub profile: Profile,
    /// Level.
    pub level: Level,
    /// Picture size.
    pub size: PictureSize,
    /// Chroma sampling.
    pub chroma: ChromaSampling,
    /// Bit depth.
    pub depth: BitDepth,
    /// Frames/s.
    pub fps: u32,
    /// Bitrate bps.
    pub bitrate_bps: u32,
    /// Reference frame count.
    pub refs: u8,
    /// Tile columns, when tiled.
    pub tile_cols: Option<u16>,
    /// Tile rows, when tiled.
    pub tile_rows: Option<u8>,
}

impl Params {
    /// Maximum tile rows (S6.2.13-14).
    pub const MAX_TILE_ROWS: u8 = 4;

    /// Minimum tile width in samples, 4 superblocks (S3).
    pub const MIN_TILE_WIDTH: u32 = 256;
}

impl Validate for Params {
    /// Check profile, level, size, rate, bitrate, refs, and tiles.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    fn validate(&self) -> Result<(), Violation> {
        if !self.profile.allows(self.depth.bits(), self.chroma) {
            return Err(Violation::ChromaNotAllowed);
        }
        let Some(caps) = self.level.caps() else {
            return Ok(());
        };
        let samples = self.size.samples();
        let max_samples = u64::from(caps.pic);
        if samples > max_samples {
            return Err(Violation::PictureTooLarge {
                actual: samples,
                max: max_samples,
            });
        }
        let axis = self.size.width().max(self.size.height());
        if u64::from(axis) > u64::from(caps.dim) {
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: u64::from(caps.dim),
            });
        }
        let rate = samples * u64::from(self.fps);
        if rate > caps.sr {
            return Err(Violation::LumaRateExceeded {
                actual: rate,
                max: caps.sr,
            });
        }
        let max_bitrate = u64::from(caps.br) * 1000;
        if u64::from(self.bitrate_bps) > max_bitrate {
            return Err(Violation::BitrateExceeded {
                actual: self.bitrate_bps,
                max: max_bitrate,
            });
        }
        if self.refs > caps.refs {
            return Err(Violation::RefsExceeded {
                actual: self.refs,
                max: caps.refs,
            });
        }
        if let Some(cols) = self.tile_cols {
            if cols > u16::from(caps.tiles) {
                return Err(Violation::TileCountExceeded {
                    actual: cols,
                    max: u16::from(caps.tiles),
                });
            }
            let col_width = self.size.width().div_ceil(u32::from(cols.max(1)));
            if col_width < Self::MIN_TILE_WIDTH {
                return Err(Violation::TileTooSmall {
                    actual: col_width,
                    min: Self::MIN_TILE_WIDTH,
                });
            }
        }
        if let Some(rows) = self.tile_rows {
            if rows > Self::MAX_TILE_ROWS {
                return Err(Violation::TileCountExceeded {
                    actual: u16::from(rows),
                    max: u16::from(Self::MAX_TILE_ROWS),
                });
            }
        }
        Ok(())
    }
}

/// Inter scale bounds: 2xW at least `RefW`, W at most 16xRefW (S7.2.5).
#[must_use]
pub fn inter_scale_valid(width: u32, ref_width: u32) -> bool {
    u64::from(width) * 2 >= u64::from(ref_width) && u64::from(width) <= u64::from(ref_width) * 16
}

/// Plain coded frame final bytes exclude the marker (B.4).
#[must_use]
pub fn plain_final_byte_valid(byte: u8) -> bool {
    byte & 0b1110_0000 != SUPERFRAME_MARKER << 5
}

/// VP9 encode profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EncodeProfile {
    /// P0 profile.
    P0,
    /// P1 profile.
    P1,
    /// P2 profile.
    P2,
    /// P3 profile.
    P3,
}

impl EncodeProfile {
    /// Decode profile backing validation.
    #[must_use]
    pub fn profile(self) -> Profile {
        match self {
            Self::P0 => Profile::P0,
            Self::P1 => Profile::P1,
            Self::P2 => Profile::P2,
            Self::P3 => Profile::P3,
        }
    }

    /// Maximum bit depth.
    #[must_use]
    pub fn max_bit_depth(self) -> u8 {
        self.profile().max_bit_depth()
    }

    /// Depth and chroma allowed together.
    #[must_use]
    pub fn allows(self, depth: u8, chroma: ChromaSampling) -> bool {
        self.profile().allows(depth, chroma)
    }
}

/// VP9 encode level: tier-picked or pinned.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EncodeLevel {
    /// Tier picks the level from size and rate.
    Auto,
    /// Pinned spec level.
    Pinned(Level),
}

impl EncodeLevel {
    /// Pinned level, absent for auto.
    #[must_use]
    pub fn level(self) -> Option<Level> {
        match self {
            Self::Auto => None,
            Self::Pinned(level) => Some(level),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

    use super::{inter_scale_valid, plain_final_byte_valid, Level, Params, Profile};

    fn params_1080p() -> Params {
        Params {
            profile: Profile::P0,
            level: Level::L41,
            size: PictureSize::new(1920, 1080).expect("hd"),
            chroma: ChromaSampling::Yuv420,
            depth: BitDepth::new(8).expect("8-bit"),
            fps: 30,
            bitrate_bps: 8_000_000,
            refs: 4,
            tile_cols: None,
            tile_rows: None,
        }
    }

    #[test]
    fn level_41_caps_anchor() {
        let caps = Level::L41.caps().expect("caps");
        assert_eq!(caps.sr, 160_432_128);
        assert_eq!(caps.refs, 6);
        assert_eq!(caps.dim, 4160);
    }

    #[test]
    fn level_62_caps_anchor() {
        let caps = Level::L62.caps().expect("caps");
        assert_eq!(caps.sr, 4_706_009_088);
        assert_eq!(caps.cpb, None);
    }

    #[test]
    fn valid_1080p_passes() {
        params_1080p().validate().expect("valid");
    }

    #[test]
    fn refs_gate() {
        let mut params = params_1080p();
        params.refs = 8;
        assert!(matches!(
            params.validate(),
            Err(Violation::RefsExceeded { .. })
        ));
    }

    #[test]
    fn profile_matrix() {
        assert!(Profile::P0.allows(8, ChromaSampling::Yuv420));
        assert!(!Profile::P0.allows(10, ChromaSampling::Yuv420));
        assert!(Profile::P2.allows(10, ChromaSampling::Yuv420));
        assert!(!Profile::P2.allows(8, ChromaSampling::Yuv420));
    }

    #[test]
    fn scale_and_marker_rules() {
        assert!(inter_scale_valid(1920, 1920));
        assert!(!inter_scale_valid(100, 1920));
        assert!(!inter_scale_valid(40_000, 1920));
        assert!(plain_final_byte_valid(0x00));
        assert!(!plain_final_byte_valid(0b1100_0000));
    }

    #[test]
    fn profile_idc_and_caps() {
        use super::Profile;
        use crate::common::ChromaSampling;
        assert_eq!(Profile::from_idc(3), Some(Profile::P3));
        assert_eq!(Profile::from_idc(4), None);
        assert!(Profile::P1.allows_chroma(ChromaSampling::Yuv444));
        assert!(!Profile::P0.allows_chroma(ChromaSampling::Yuv444));
        assert_eq!(Profile::P2.max_bit_depth(), 12);
        assert_eq!(Profile::P0.max_bit_depth(), 8);
        assert_eq!(super::EncodeProfile::P3.max_bit_depth(), 12);
    }

    #[test]
    fn level_idx_roundtrip() {
        use super::Level;
        assert_eq!(Level::from_idx(0).unwrap(), Level::L1);
        assert_eq!(Level::from_idx(13).unwrap(), Level::L62);
        assert!(Level::from_idx(14).is_err());
        for level in [
            Level::L1,
            Level::L11,
            Level::L2,
            Level::L21,
            Level::L3,
            Level::L31,
            Level::L4,
            Level::L41,
            Level::L5,
            Level::L51,
            Level::L52,
            Level::L6,
            Level::L61,
            Level::L62,
        ] {
            assert!(level.caps().is_some());
        }
    }
}
