//! AV2 profile, level, and substream enforcement.
//!
//! Values from the `AOMedia` AV2 specification v1.0.0; cites name the table.

use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

/// AV2 profile (Table A.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// Main 4:2:0 10-bit IP0, idc 0.
    Main420Ip0,
    /// Main 4:2:0 10-bit IP1, idc 1.
    Main420Ip1,
    /// Main 4:2:0 10-bit IP2, idc 2.
    Main420Ip2,
    /// Main 4:2:2 10-bit IP1, idc 3.
    Main422Ip1,
    /// Main 4:4:4 10-bit IP1, idc 4.
    Main444Ip1,
}

impl Profile {
    /// profile idc (Table A.1).
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Main420Ip0 => 0,
            Self::Main420Ip1 => 1,
            Self::Main420Ip2 => 2,
            Self::Main422Ip1 => 3,
            Self::Main444Ip1 => 4,
        }
    }

    /// Resolve idc 0-4; 5-30 are reserved and 31 is configurable
    /// with no fixed profile.
    #[must_use]
    pub fn from_idc(idc: u8) -> Option<Self> {
        match idc {
            0 => Some(Self::Main420Ip0),
            1 => Some(Self::Main420Ip1),
            2 => Some(Self::Main420Ip2),
            3 => Some(Self::Main422Ip1),
            4 => Some(Self::Main444Ip1),
            _ => None,
        }
    }

    /// Allowed chroma sampling (Table A.1).
    #[must_use]
    pub fn allows_chroma(self, chroma: ChromaSampling) -> bool {
        match self {
            Self::Main420Ip0 | Self::Main420Ip1 | Self::Main420Ip2 => {
                matches!(chroma, ChromaSampling::Mono | ChromaSampling::Yuv420)
            }
            Self::Main422Ip1 => !matches!(chroma, ChromaSampling::Yuv444),
            Self::Main444Ip1 => true,
        }
    }
}

/// Bit depth from `bit_depth_idc` (Table 6.3): 0 selects 10-bit, 1 8-bit.
#[must_use]
pub fn bit_depth_from_idc(idc: u8) -> Option<BitDepth> {
    match idc {
        0 => BitDepth::new(10),
        1 => BitDepth::new(8),
        _ => None,
    }
}

/// Tier flag (Table A.9 note).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tier {
    /// Main tier.
    Main,
    /// High tier, L4.0 and up.
    High,
}

/// AV2 level, `seq_level_idx` 0-21 (Table A.7).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// Level 2.0, idx 0.
    L20,
    /// Level 2.1, idx 1.
    L21,
    /// Level 3.0, idx 2.
    L30,
    /// Level 3.1, idx 3.
    L31,
    /// Level 4.0, idx 4.
    L40,
    /// Level 4.1, idx 5.
    L41,
    /// Level 5.0, idx 6.
    L50,
    /// Level 5.1, idx 7.
    L51,
    /// Level 5.2, idx 8.
    L52,
    /// Level 5.3, idx 9.
    L53,
    /// Level 6.0, idx 10.
    L60,
    /// Level 6.1, idx 11.
    L61,
    /// Level 6.2, idx 12.
    L62,
    /// Level 6.3, idx 13.
    L63,
    /// Level 7.0, idx 14.
    L70,
    /// Level 7.1, idx 15.
    L71,
    /// Level 7.2, idx 16.
    L72,
    /// Level 7.3, idx 17.
    L73,
    /// Level 8.0, idx 18.
    L80,
    /// Level 8.1, idx 19.
    L81,
    /// Level 8.2, idx 20.
    L82,
    /// Level 8.3, idx 21.
    L83,
    /// Maximum parameters, idx 31, unconstrained.
    Maximum,
}

impl Level {
    /// Resolve `seq_level_idx`; 22-30 are reserved (Table A.7).
    ///
    /// # Errors
    ///
    /// Returns `ReservedLevel` for 22-30.
    pub fn from_idx(idx: u8) -> Result<Self, Violation> {
        match idx {
            0 => Ok(Self::L20),
            1 => Ok(Self::L21),
            2 => Ok(Self::L30),
            3 => Ok(Self::L31),
            4 => Ok(Self::L40),
            5 => Ok(Self::L41),
            6 => Ok(Self::L50),
            7 => Ok(Self::L51),
            8 => Ok(Self::L52),
            9 => Ok(Self::L53),
            10 => Ok(Self::L60),
            11 => Ok(Self::L61),
            12 => Ok(Self::L62),
            13 => Ok(Self::L63),
            14 => Ok(Self::L70),
            15 => Ok(Self::L71),
            16 => Ok(Self::L72),
            17 => Ok(Self::L73),
            18 => Ok(Self::L80),
            19 => Ok(Self::L81),
            20 => Ok(Self::L82),
            21 => Ok(Self::L83),
            31 => Ok(Self::Maximum),
            reserved => Err(Violation::ReservedLevel(reserved)),
        }
    }

    /// High tier allowed at L4.0 and up (Table A.9 note).
    #[must_use]
    pub fn allows_high_tier(self) -> bool {
        !matches!(
            self,
            Self::L20 | Self::L21 | Self::L30 | Self::L31 | Self::Maximum
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
            Self::L70 => Some(&CAPS[14]),
            Self::L71 => Some(&CAPS[15]),
            Self::L72 => Some(&CAPS[16]),
            Self::L73 => Some(&CAPS[17]),
            Self::L80 => Some(&CAPS[18]),
            Self::L81 => Some(&CAPS[19]),
            Self::L82 => Some(&CAPS[20]),
            Self::L83 => Some(&CAPS[21]),
            Self::Maximum => None,
        }
    }
}

/// One Tables A.8/A.9 row: idx, max picture, max dimension,
/// display/decode rates, header rate, Mbps Main/High (kbps), CR
/// Main/High, max tiles/columns.
pub struct LevelCaps {
    /// `seq_level_idx`.
    pub idx: u8,
    /// Max picture samples.
    pub pic: u32,
    /// Max dimension samples.
    pub dim: u32,
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
const CAPS: [LevelCaps; 22] = [
    LevelCaps { idx: 0, pic: 147_456, dim: 640, disp: 4_423_680, dec: 5_529_600, hdr: 150, mbps_m: 1500, mbps_h: None, cr_m: 2, cr_h: None, tiles: 8, tile_cols: 4 },
    LevelCaps { idx: 1, pic: 278_784, dim: 880, disp: 8_363_520, dec: 10_454_400, hdr: 150, mbps_m: 3000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 8, tile_cols: 4 },
    LevelCaps { idx: 2, pic: 665_856, dim: 1360, disp: 19_975_680, dec: 24_969_600, hdr: 150, mbps_m: 6000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 16, tile_cols: 6 },
    LevelCaps { idx: 3, pic: 1_065_024, dim: 1720, disp: 31_950_720, dec: 39_938_400, hdr: 150, mbps_m: 10_000, mbps_h: None, cr_m: 2, cr_h: None, tiles: 16, tile_cols: 6 },
    LevelCaps { idx: 4, pic: 2_359_296, dim: 2560, disp: 70_778_880, dec: 77_856_768, hdr: 300, mbps_m: 12_000, mbps_h: Some(30_000), cr_m: 4, cr_h: Some(4), tiles: 32, tile_cols: 8 },
    LevelCaps { idx: 5, pic: 2_359_296, dim: 2560, disp: 141_557_760, dec: 155_713_536, hdr: 300, mbps_m: 20_000, mbps_h: Some(50_000), cr_m: 4, cr_h: Some(4), tiles: 32, tile_cols: 8 },
    LevelCaps { idx: 6, pic: 8_912_896, dim: 4975, disp: 267_386_880, dec: 273_715_200, hdr: 300, mbps_m: 30_000, mbps_h: Some(100_000), cr_m: 6, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 7, pic: 8_912_896, dim: 4975, disp: 534_773_760, dec: 547_430_400, hdr: 300, mbps_m: 40_000, mbps_h: Some(160_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 8, pic: 8_912_896, dim: 4975, disp: 1_069_547_520, dec: 1_094_860_800, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 9, pic: 8_912_896, dim: 4975, disp: 1_069_547_520, dec: 1_176_502_272, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 64, tile_cols: 8 },
    LevelCaps { idx: 10, pic: 35_651_584, dim: 9951, disp: 1_069_547_520, dec: 1_176_502_272, hdr: 300, mbps_m: 60_000, mbps_h: Some(240_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 11, pic: 35_651_584, dim: 9951, disp: 2_139_095_040, dec: 2_189_721_600, hdr: 300, mbps_m: 100_000, mbps_h: Some(480_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 12, pic: 35_651_584, dim: 9951, disp: 4_278_190_080, dec: 4_379_443_200, hdr: 300, mbps_m: 160_000, mbps_h: Some(800_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 13, pic: 35_651_584, dim: 9951, disp: 4_278_190_080, dec: 4_706_009_088, hdr: 300, mbps_m: 160_000, mbps_h: Some(800_000), cr_m: 8, cr_h: Some(4), tiles: 128, tile_cols: 16 },
    LevelCaps { idx: 14, pic: 142_606_336, dim: 19_902, disp: 4_278_190_080, dec: 4_706_009_088, hdr: 960, mbps_m: 160_000, mbps_h: Some(800_000), cr_m: 8, cr_h: Some(4), tiles: 256, tile_cols: 32 },
    LevelCaps { idx: 15, pic: 142_606_336, dim: 19_902, disp: 8_556_380_160, dec: 8_758_886_400, hdr: 960, mbps_m: 200_000, mbps_h: Some(960_000), cr_m: 8, cr_h: Some(4), tiles: 256, tile_cols: 32 },
    LevelCaps { idx: 16, pic: 142_606_336, dim: 19_902, disp: 17_112_760_320, dec: 17_517_772_800, hdr: 960, mbps_m: 320_000, mbps_h: Some(1_600_000), cr_m: 8, cr_h: Some(4), tiles: 256, tile_cols: 32 },
    LevelCaps { idx: 17, pic: 142_606_336, dim: 19_902, disp: 17_112_760_320, dec: 18_824_036_352, hdr: 960, mbps_m: 320_000, mbps_h: Some(1_600_000), cr_m: 8, cr_h: Some(4), tiles: 256, tile_cols: 32 },
    LevelCaps { idx: 18, pic: 530_841_600, dim: 38_400, disp: 17_112_760_320, dec: 18_824_036_352, hdr: 960, mbps_m: 320_000, mbps_h: Some(1_600_000), cr_m: 8, cr_h: Some(4), tiles: 512, tile_cols: 64 },
    LevelCaps { idx: 19, pic: 530_841_600, dim: 38_400, disp: 34_225_520_640, dec: 34_910_031_052, hdr: 960, mbps_m: 400_000, mbps_h: Some(1_920_000), cr_m: 8, cr_h: Some(4), tiles: 512, tile_cols: 64 },
    LevelCaps { idx: 20, pic: 530_841_600, dim: 38_400, disp: 68_451_041_280, dec: 69_820_062_105, hdr: 960, mbps_m: 640_000, mbps_h: Some(3_200_000), cr_m: 8, cr_h: Some(4), tiles: 512, tile_cols: 64 },
    LevelCaps { idx: 21, pic: 530_841_600, dim: 38_400, disp: 68_451_041_280, dec: 75_296_145_408, hdr: 960, mbps_m: 640_000, mbps_h: Some(3_200_000), cr_m: 8, cr_h: Some(4), tiles: 512, tile_cols: 64 },
];

/// One substream scaling row: picture, scale factor, max V/H,
/// max tile columns, max header rate.
pub struct SubCaps {
    /// Max picture samples.
    pub pic: u32,
    /// Scale factor.
    pub scale: f32,
    /// Max scaled height.
    pub max_v: u16,
    /// Max scaled width.
    pub max_h: u16,
    /// Max scaled tile columns.
    pub tile_cols: u8,
    /// Max scaled header rate.
    pub hdr_x: u16,
}

#[rustfmt::skip]
const SUB_CAPS: [SubCaps; 15] = [
    SubCaps { pic: 2_359_296, scale: 1.5, max_v: 1600, max_h: 896, tile_cols: 7, hdr_x: 132 },
    SubCaps { pic: 2_359_296, scale: 4.0, max_v: 960, max_h: 576, tile_cols: 4, hdr_x: 132 },
    SubCaps { pic: 2_359_296, scale: 9.0, max_v: 640, max_h: 384, tile_cols: 3, hdr_x: 132 },
    SubCaps { pic: 8_912_896, scale: 1.5, max_v: 2560, max_h: 1472, tile_cols: 7, hdr_x: 132 },
    SubCaps { pic: 8_912_896, scale: 4.0, max_v: 1920, max_h: 1088, tile_cols: 4, hdr_x: 132 },
    SubCaps { pic: 8_912_896, scale: 9.0, max_v: 1280, max_h: 768, tile_cols: 3, hdr_x: 132 },
    SubCaps { pic: 35_651_584, scale: 1.5, max_v: 5120, max_h: 2280, tile_cols: 13, hdr_x: 132 },
    SubCaps { pic: 35_651_584, scale: 4.0, max_v: 3840, max_h: 2176, tile_cols: 8, hdr_x: 132 },
    SubCaps { pic: 35_651_584, scale: 9.0, max_v: 2560, max_h: 1472, tile_cols: 5, hdr_x: 132 },
    SubCaps { pic: 142_606_336, scale: 1.5, max_v: 10_240, max_h: 5760, tile_cols: 26, hdr_x: 132 },
    SubCaps { pic: 142_606_336, scale: 4.0, max_v: 7680, max_h: 4320, tile_cols: 16, hdr_x: 132 },
    SubCaps { pic: 142_606_336, scale: 9.0, max_v: 5120, max_h: 2880, tile_cols: 11, hdr_x: 132 },
    SubCaps { pic: 530_841_600, scale: 1.5, max_v: 20_480, max_h: 11_520, tile_cols: 52, hdr_x: 132 },
    SubCaps { pic: 530_841_600, scale: 4.0, max_v: 15_360, max_h: 8640, tile_cols: 32, hdr_x: 132 },
    SubCaps { pic: 530_841_600, scale: 9.0, max_v: 10_240, max_h: 5760, tile_cols: 21, hdr_x: 132 },
];

/// Substream rows for one picture size.
pub fn sub_caps_for(pic: u32) -> impl Iterator<Item = &'static SubCaps> {
    SUB_CAPS.iter().filter(move |row| row.pic == pic)
}

/// Maximum superblock edge in luma samples.
pub const SUPERBLOCK_MAX: u32 = 256;

/// AV2 coding parameters.
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
    /// Frames/s.
    pub fps: u32,
    /// Bitrate bps.
    pub bitrate_bps: u32,
    /// Tile columns, when tiled.
    pub tile_cols: Option<u16>,
}

impl Validate for Params {
    /// Check profile, tier, level, size, rate, bitrate, and tiles.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    fn validate(&self) -> Result<(), Violation> {
        if !self.profile.allows_chroma(self.chroma) {
            return Err(Violation::ChromaNotAllowed);
        }
        if !matches!(self.depth.bits(), 8 | 10) {
            return Err(Violation::BitDepthNotAllowed {
                actual: self.depth.bits(),
                max: 10,
            });
        }
        if self.tier == Tier::High && !self.level.allows_high_tier() {
            return Err(Violation::TierRequired);
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

    use super::{bit_depth_from_idc, sub_caps_for, Level, Params, Profile, Tier};

    fn params_1080p() -> Params {
        Params {
            profile: Profile::Main420Ip1,
            tier: Tier::Main,
            level: Level::L41,
            size: PictureSize::new(1920, 1080).expect("hd"),
            chroma: ChromaSampling::Yuv420,
            depth: BitDepth::new(10).expect("10-bit"),
            fps: 30,
            bitrate_bps: 8_000_000,
            tile_cols: None,
        }
    }

    #[test]
    fn level_40_caps_anchor() {
        let caps = Level::L40.caps().expect("caps");
        assert_eq!(caps.pic, 2_359_296);
        assert_eq!(caps.dim, 2560);
        assert_eq!(caps.disp, 70_778_880);
    }

    #[test]
    fn level_83_caps_anchor() {
        let caps = Level::L83.caps().expect("caps");
        assert_eq!(caps.pic, 530_841_600);
        assert_eq!(caps.tiles, 512);
    }

    #[test]
    fn from_idx_gaps_reserved() {
        assert!(Level::from_idx(22).is_err());
        assert_eq!(Level::from_idx(7).expect("5.1"), Level::L51);
    }

    #[test]
    fn bit_depth_idc() {
        assert_eq!(bit_depth_from_idc(0).expect("10-bit").bits(), 10);
        assert_eq!(bit_depth_from_idc(1).expect("8-bit").bits(), 8);
        assert_eq!(bit_depth_from_idc(2), None);
    }

    #[test]
    fn valid_1080p_passes() {
        params_1080p().validate().expect("valid");
    }

    #[test]
    fn twelve_bit_rejected() {
        let mut params = params_1080p();
        params.depth = BitDepth::new(12).expect("12-bit");
        assert!(matches!(
            params.validate(),
            Err(Violation::BitDepthNotAllowed { .. })
        ));
    }

    #[test]
    fn substream_lookup() {
        let rows: Vec<_> = sub_caps_for(8_912_896).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].max_v, 2560);
    }
}
