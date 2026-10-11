//! AV1 profile, level, color, and OBU enforcement.
//!
//! Values from the `AOMedia` AV1 specification; cites name the section.
//!
//! Film grain applies after decode and never reaches references (S7.18).
//! Switch frames overwrite all references without forcing intra (S3).

use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

/// AV1 profile (Annex A.2, S6.4.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// Main, 0: 8/10-bit 4:2:0 plus monochrome.
    Main,
    /// High, 1: 8/10-bit 4:4:4.
    High,
    /// Professional, 2: 8/10-bit 4:2:2 plus monochrome, 12-bit anything.
    Professional,
}

impl Profile {
    /// `seq_profile` value.
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Main => 0,
            Self::High => 1,
            Self::Professional => 2,
        }
    }

    /// Decoder capability matrix (S6.4.1, Annex A.2).
    #[must_use]
    pub fn allows(self, depth_bits: u8, chroma: ChromaSampling, mono: bool) -> bool {
        match self {
            Self::Main => {
                matches!(depth_bits, 8 | 10)
                    && (chroma == ChromaSampling::Yuv420
                        || (mono && chroma == ChromaSampling::Mono))
            }
            Self::High => matches!(depth_bits, 8 | 10) && !mono && chroma == ChromaSampling::Yuv444,
            Self::Professional => {
                if mono && chroma == ChromaSampling::Mono {
                    return matches!(depth_bits, 8 | 10 | 12);
                }
                match depth_bits {
                    8 | 10 => matches!(chroma, ChromaSampling::Yuv420 | ChromaSampling::Yuv422),
                    12 => !matches!(chroma, ChromaSampling::Mono),
                    _ => false,
                }
            }
        }
    }

    /// Cumulative decoder tiers: High decodes up to 1, Pro up to 2.
    #[must_use]
    pub fn decodes(self, stream: Self) -> bool {
        self.idc() >= stream.idc()
    }
}

/// Tier flag (Annex A.3).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tier {
    /// Main tier, flag 0.
    Main,
    /// High tier, flag 1, L4.0 and up.
    High,
}

/// AV1 level, `seq_level_idx` 0-23 (Annex A.3).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// Level 2.0, idx 0.
    L20,
    /// Level 2.1, idx 1.
    L21,
    /// Level 3.0, idx 4.
    L30,
    /// Level 3.1, idx 5.
    L31,
    /// Level 4.0, idx 8.
    L40,
    /// Level 4.1, idx 9.
    L41,
    /// Level 5.0, idx 12.
    L50,
    /// Level 5.1, idx 13.
    L51,
    /// Level 5.2, idx 14.
    L52,
    /// Level 5.3, idx 15.
    L53,
    /// Level 6.0, idx 16.
    L60,
    /// Level 6.1, idx 17.
    L61,
    /// Level 6.2, idx 18.
    L62,
    /// Level 6.3, idx 19.
    L63,
    /// Maximum parameters, idx 31, unconstrained.
    Maximum,
}

impl Level {
    /// Resolve `seq_level_idx`; 24-30 and gaps are reserved (Annex A.3).
    ///
    /// # Errors
    ///
    /// Returns `ReservedLevel` for undefined indexes.
    pub fn from_idx(idx: u8) -> Result<Self, Violation> {
        match idx {
            0 => Ok(Self::L20),
            1 => Ok(Self::L21),
            4 => Ok(Self::L30),
            5 => Ok(Self::L31),
            8 => Ok(Self::L40),
            9 => Ok(Self::L41),
            12 => Ok(Self::L50),
            13 => Ok(Self::L51),
            14 => Ok(Self::L52),
            15 => Ok(Self::L53),
            16 => Ok(Self::L60),
            17 => Ok(Self::L61),
            18 => Ok(Self::L62),
            19 => Ok(Self::L63),
            31 => Ok(Self::Maximum),
            reserved => Err(Violation::ReservedLevel(reserved)),
        }
    }

    /// High tier allowed at L4.0 and up (Annex A.3).
    #[must_use]
    pub fn allows_high_tier(self) -> bool {
        matches!(
            self,
            Self::L40
                | Self::L41
                | Self::L50
                | Self::L51
                | Self::L52
                | Self::L53
                | Self::L60
                | Self::L61
                | Self::L62
                | Self::L63
        )
    }

    /// Level caps row; Maximum carries none.
    #[must_use]
    pub fn caps(self) -> Option<&'static LevelCaps> {
        match self {
            Self::L20 => Some(&CAPS[0]),
            Self::L21 => Some(&CAPS[1]),
            Self::L30 => Some(&CAPS[2]),
            Self::L31 => Some(&CAPS[3]),
            Self::L40 => Some(&CAPS[4]),
            Self::L41 => Some(&CAPS[5]),
            Self::L50 => Some(&CAPS[6]),
            Self::L51 => Some(&CAPS[7]),
            Self::L52 => Some(&CAPS[8]),
            Self::L53 => Some(&CAPS[9]),
            Self::L60 => Some(&CAPS[10]),
            Self::L61 => Some(&CAPS[11]),
            Self::L62 => Some(&CAPS[12]),
            Self::L63 => Some(&CAPS[13]),
            Self::Maximum => None,
        }
    }
}

/// One Annex A.3 row: idx, max picture, max W/H, display/decode rates,
/// header rate, Mbps Main/High (kbps), CR Main/High, max tiles/columns.
pub struct LevelCaps {
    /// `seq_level_idx`.
    pub idx: u8,
    /// Max picture samples.
    pub pic: u32,
    /// Max width samples.
    pub w: u16,
    /// Max height samples.
    pub h: u16,
    /// Max display rate samples/s.
    pub disp: u64,
    /// Max decode rate samples/s.
    pub dec: u64,
    /// Max header rate/s.
    pub hdr: u32,
    /// Max Main bitrate kbps.
    pub mbps_m: u32,
    /// Max High bitrate kbps.
    pub mbps_h: Option<u32>,
    /// `MinCR` Main.
    pub cr_m: u8,
    /// `MinCR` High.
    pub cr_h: Option<u8>,
    /// Max tiles.
    pub tiles: u16,
    /// Max tile columns.
    pub tile_cols: u8,
}

#[rustfmt::skip]
const CAPS: [LevelCaps; 14] = [
    LevelCaps { idx: 0, pic: 147_456, w: 2048, h: 1152, disp: 4_423_680, dec: 5_529_600, hdr: 150, mbps_m: 1500, mbps_h: None, cr_m: 2, cr_h: None, tiles: 8, tile_cols: 4 },
    LevelCaps { idx: 1, pic: 278_784, w: 2816, h: 1584, disp: 8_363_520, dec: 10_454_400, hdr: 150, mbps_m: 3000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 8, tile_cols: 4 },
    LevelCaps { idx: 4, pic: 665_856, w: 4352, h: 2448, disp: 19_975_680, dec: 24_969_600, hdr: 150, mbps_m: 6000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 16, tile_cols: 6 },
    LevelCaps { idx: 5, pic: 1_065_024, w: 5504, h: 3096, disp: 31_950_720, dec: 39_938_400, hdr: 150, mbps_m: 10_000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 16, tile_cols: 6 },
    LevelCaps { idx: 8, pic: 2_359_296, w: 6144, h: 3456, disp: 70_778_880, dec: 77_856_768, hdr: 300, mbps_m: 12_000, mbps_h: Some(30_000), cr_m: 4, cr_h: Some(4), tiles: 32, tile_cols: 8 },
    LevelCaps { idx: 9, pic: 2_359_296, w: 6144, h: 3456, disp: 141_557_760, dec: 155_713_536, hdr: 300, mbps_m: 20_000, mbps_h: Some(50_000), cr_m: 4, cr_h: Some(4), tiles: 32, tile_cols: 8 },
    LevelCaps { idx: 12, pic: 8_912_896, w: 8192, h: 4352, disp: 267_386_880, dec: 273_715_200, hdr: 300, mbps_m: 30_000, mbps_h: Some(100_000), cr_m: 6, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 13, pic: 8_912_896, w: 8192, h: 4352, disp: 534_773_760, dec: 547_430_400, hdr: 300, mbps_m: 40_000, mbps_h: Some(160_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 14, pic: 8_912_896, w: 8192, h: 4352, disp: 1_069_547_520, dec: 1_094_860_800, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 15, pic: 8_912_896, w: 8192, h: 4352, disp: 1_069_547_520, dec: 1_176_502_272, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 16, pic: 35_651_584, w: 16_384, h: 8704, disp: 1_069_547_520, dec: 1_176_502_272, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 17, pic: 35_651_584, w: 16_384, h: 8704, disp: 2_139_095_040, dec: 2_189_721_600, hdr: 300, mbps_m: 100_000, mbps_h: Some(480_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 18, pic: 35_651_584, w: 16_384, h: 8704, disp: 4_278_190_080, dec: 4_379_443_200, hdr: 300, mbps_m: 160_000, mbps_h: Some(800_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 19, pic: 35_651_584, w: 16_384, h: 8704, disp: 4_278_190_080, dec: 4_706_009_088, hdr: 300, mbps_m: 160_000, mbps_h: Some(800_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
];

/// Minimum frame dimension in samples (Annex A.3).
pub const MIN_DIM: u32 = 16;

/// Reference frame slot count (S4).
pub const NUM_REF_FRAMES: u8 = 8;

/// Max luma tile width in samples (S4).
pub const MAX_TILE_WIDTH: u32 = 4096;

/// Superres denominator 9-16 over 8 (S5.9.8).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Superres(u8);

impl Superres {
    /// Denominator coded+9, range 9-16.
    #[must_use]
    pub fn new(denom: u8) -> Option<Self> {
        (9..=16).contains(&denom).then_some(Self(denom))
    }

    /// Denominator value.
    #[must_use]
    pub fn denom(&self) -> u8 {
        self.0
    }
}

/// Frame type (S6.8.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FrameType {
    /// Key frame, 0.
    Key,
    /// Inter frame, 1.
    Inter,
    /// Intra-only frame, 2.
    IntraOnly,
    /// Switch frame, 3.
    Switch,
}

impl FrameType {
    /// Classify the 2-bit frame type.
    #[must_use]
    pub fn classify(bits: u8) -> Option<Self> {
        match bits {
            0 => Some(Self::Key),
            1 => Some(Self::Inter),
            2 => Some(Self::IntraOnly),
            3 => Some(Self::Switch),
            _ => None,
        }
    }
}

/// OBU kind (S6.2.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObuKind {
    /// Sequence header, 1.
    SequenceHeader,
    /// Temporal delimiter, 2.
    TemporalDelimiter,
    /// Frame header, 3.
    FrameHeader,
    /// Tile group, 4.
    TileGroup,
    /// Metadata, 5.
    Metadata,
    /// Frame, 6.
    Frame,
    /// Redundant frame header, 7.
    RedundantFrameHeader,
    /// Tile list, 8.
    TileList,
    /// Padding, 15.
    Padding,
}

impl ObuKind {
    /// Classify the 4-bit OBU type; 0 and 9-14 are reserved.
    ///
    /// # Errors
    ///
    /// Returns `ObuReserved` for reserved types.
    pub fn classify(obu_type: u8) -> Result<Self, Violation> {
        match obu_type {
            1 => Ok(Self::SequenceHeader),
            2 => Ok(Self::TemporalDelimiter),
            3 => Ok(Self::FrameHeader),
            4 => Ok(Self::TileGroup),
            5 => Ok(Self::Metadata),
            6 => Ok(Self::Frame),
            7 => Ok(Self::RedundantFrameHeader),
            8 => Ok(Self::TileList),
            15 => Ok(Self::Padding),
            reserved => Err(Violation::ObuReserved(reserved)),
        }
    }
}

/// AV1 coding parameters.
pub struct Params {
    /// Profile.
    pub profile: Profile,
    /// Tier.
    pub tier: Tier,
    /// Level.
    pub level: Level,
    /// Picture size.
    pub size: PictureSize,
    /// Chroma sampling.
    pub chroma: ChromaSampling,
    /// Bit depth.
    pub depth: BitDepth,
    /// Monochrome stream.
    pub mono: bool,
    /// Frames/s.
    pub fps: u32,
    /// Bitrate bps.
    pub bitrate_bps: u32,
    /// Tile columns, when tiled.
    pub tile_cols: Option<u16>,
    /// Total tiles, when tiled.
    pub tiles: Option<u16>,
    /// Superres denominator, when used.
    pub superres: Option<Superres>,
    /// Sequence superres enable.
    pub seq_superres: bool,
}

impl Validate for Params {
    /// Check color, level, size, rate, bitrate, and tiles.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    fn validate(&self) -> Result<(), Violation> {
        if self.depth.bits() == 12 && self.profile != Profile::Professional {
            return Err(Violation::BitDepthNotAllowed {
                actual: 12,
                max: 10,
            });
        }
        if self.chroma == ChromaSampling::Yuv422 && self.depth.bits() != 12 {
            return Err(Violation::ChromaNotAllowed);
        }
        if self.mono && !matches!(self.profile, Profile::Main | Profile::Professional) {
            return Err(Violation::ChromaNotAllowed);
        }
        if !self
            .profile
            .allows(self.depth.bits(), self.chroma, self.mono)
        {
            return Err(Violation::ChromaNotAllowed);
        }
        if self.tier == Tier::High && !self.level.allows_high_tier() {
            return Err(Violation::TierRequired);
        }
        let Some(caps) = self.level.caps() else {
            return Ok(());
        };
        if self.size.width() < MIN_DIM || self.size.height() < MIN_DIM {
            let axis = self.size.width().min(self.size.height());
            return Err(Violation::PictureTooSmall { axis, min: MIN_DIM });
        }
        let samples = self.size.samples();
        let max_samples = u64::from(caps.pic);
        if samples > max_samples {
            return Err(Violation::PictureTooLarge {
                actual: samples,
                max: max_samples,
            });
        }
        if u64::from(self.size.width()) > u64::from(caps.w)
            || u64::from(self.size.height()) > u64::from(caps.h)
        {
            let axis = self.size.width().max(self.size.height());
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: u64::from(caps.w.max(caps.h)),
            });
        }
        let rate = samples * u64::from(self.fps);
        if rate > caps.disp {
            return Err(Violation::LumaRateExceeded {
                actual: rate,
                max: caps.disp,
            });
        }
        let max_br = match self.tier {
            Tier::Main => u64::from(caps.mbps_m),
            Tier::High => u64::from(caps.mbps_h.unwrap_or(caps.mbps_m)),
        } * 1000;
        if u64::from(self.bitrate_bps) > max_br {
            return Err(Violation::BitrateExceeded {
                actual: self.bitrate_bps,
                max: max_br,
            });
        }
        if let Some(cols) = self.tile_cols {
            if cols > u16::from(caps.tile_cols) {
                return Err(Violation::TileCountExceeded {
                    actual: cols,
                    max: u16::from(caps.tile_cols),
                });
            }
        }
        if let Some(tiles) = self.tiles {
            let rate = u32::from(tiles) * self.fps;
            let max = u32::from(caps.tiles) * 120;
            if rate > max {
                return Err(Violation::TileRateExceeded { actual: rate, max });
            }
        }
        if self.superres.is_some() && !self.seq_superres {
            return Err(Violation::SuperresDenied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

    use super::{FrameType, Level, ObuKind, Params, Profile, Superres, Tier};

    fn params_1080p() -> Params {
        Params {
            profile: Profile::Main,
            tier: Tier::Main,
            level: Level::L41,
            size: PictureSize::new(1920, 1080).expect("hd"),
            chroma: ChromaSampling::Yuv420,
            depth: BitDepth::new(8).expect("8-bit"),
            mono: false,
            fps: 30,
            bitrate_bps: 8_000_000,
            tile_cols: None,
            tiles: None,
            superres: None,
            seq_superres: false,
        }
    }

    #[test]
    fn level_40_caps_anchor() {
        let caps = Level::L40.caps().expect("caps");
        assert_eq!(caps.pic, 2_359_296);
        assert_eq!(caps.disp, 70_778_880);
        assert_eq!(caps.mbps_m, 12_000);
    }

    #[test]
    fn level_51_caps_anchor() {
        let caps = Level::L51.caps().expect("caps");
        assert_eq!(caps.disp, 534_773_760);
        assert_eq!(caps.mbps_h, Some(160_000));
    }

    #[test]
    fn from_idx_gaps_reserved() {
        assert!(Level::from_idx(2).is_err());
        assert!(Level::from_idx(24).is_err());
        assert_eq!(Level::from_idx(13).expect("5.1"), Level::L51);
        assert_eq!(Level::from_idx(31).expect("max"), Level::Maximum);
    }

    #[test]
    fn valid_1080p_passes() {
        params_1080p().validate().expect("valid");
    }

    #[test]
    fn twelve_bit_needs_pro() {
        let mut params = params_1080p();
        params.depth = BitDepth::new(12).expect("12-bit");
        assert!(matches!(
            params.validate(),
            Err(Violation::BitDepthNotAllowed { .. })
        ));
    }

    #[test]
    fn min_frame_enforced() {
        let mut params = params_1080p();
        params.size = PictureSize::new(8, 8).expect("tiny");
        assert!(matches!(
            params.validate(),
            Err(Violation::PictureTooSmall { .. })
        ));
    }

    #[test]
    fn tile_rate_enforced() {
        let mut params = params_1080p();
        params.tiles = Some(65);
        params.fps = 60;
        assert!(matches!(
            params.validate(),
            Err(Violation::TileRateExceeded { .. })
        ));
    }

    #[test]
    fn superres_needs_enable() {
        let mut params = params_1080p();
        params.superres = Superres::new(12);
        assert_eq!(params.validate(), Err(Violation::SuperresDenied));
    }

    #[test]
    fn obu_and_frame_types() {
        assert!(ObuKind::classify(0).is_err());
        assert!(ObuKind::classify(9).is_err());
        assert_eq!(FrameType::classify(3), Some(FrameType::Switch));
    }
}
