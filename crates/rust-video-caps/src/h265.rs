//! H.265 profile, tier, level, and NAL enforcement.
//!
//! Values from ITU-T H.265 V10 (07/2024); cites name the section.
//! Reference-set, HRD timing, and tile-rate validation are unmapped.

use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

/// H.265 profile (ITU-T H.265 A.3, F.3, H.11).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// Main, idc 1 (A.3.2).
    Main,
    /// Main 10, idc 2 (A.3.3).
    Main10,
    /// Main Still Picture, idc 3 (A.3.4).
    MainStillPicture,
    /// Range extensions, idc 4 (A.3.5).
    RangeExtensions,
    /// High throughput, idc 5 (A.3.6).
    HighThroughput,
    /// Screen content, idc 9 (A.3.7).
    ScreenContent,
    /// High throughput screen content, idc 11 (A.3.8).
    HighThroughputScreenContent,
    /// Scalable Main (F.11).
    ScalableMain,
    /// Scalable Main 10 (F.11).
    ScalableMain10,
    /// Multiview Main (G.11).
    MultiviewMain,
    /// 3D Main, idc 8 (H.11).
    ThreeDMain,
}

impl Profile {
    /// `general_profile_idc` where the spec fixes one.
    #[must_use]
    pub fn idc(self) -> Option<u8> {
        match self {
            Self::Main => Some(1),
            Self::Main10 => Some(2),
            Self::MainStillPicture => Some(3),
            Self::RangeExtensions => Some(4),
            Self::HighThroughput => Some(5),
            Self::ScreenContent => Some(9),
            Self::HighThroughputScreenContent => Some(11),
            Self::ThreeDMain => Some(8),
            Self::ScalableMain | Self::ScalableMain10 | Self::MultiviewMain => None,
        }
    }

    /// Maximum bit depth (A.3.2-3).
    #[must_use]
    pub fn max_bit_depth(self) -> u8 {
        match self {
            Self::Main10 | Self::ScalableMain10 => 10,
            Self::RangeExtensions
            | Self::HighThroughput
            | Self::ScreenContent
            | Self::HighThroughputScreenContent => 16,
            _ => 8,
        }
    }

    /// Allowed chroma sampling (A.3.2-3).
    #[must_use]
    pub fn allows_chroma(self, chroma: ChromaSampling) -> bool {
        match self {
            Self::Main
            | Self::Main10
            | Self::MainStillPicture
            | Self::ScalableMain
            | Self::ScalableMain10
            | Self::MultiviewMain => {
                matches!(chroma, ChromaSampling::Mono | ChromaSampling::Yuv420)
            }
            Self::ThreeDMain => {
                matches!(chroma, ChromaSampling::Mono | ChromaSampling::Yuv420)
            }
            Self::RangeExtensions
            | Self::HighThroughput
            | Self::ScreenContent
            | Self::HighThroughputScreenContent => true,
        }
    }

    /// `HbrFactor` for rate scaling (A.4.2, Table A.10).
    #[must_use]
    pub fn hbr_factor(self) -> u32 {
        match self {
            Self::Main | Self::Main10 | Self::MainStillPicture => 1,
            Self::HighThroughput | Self::HighThroughputScreenContent => 6,
            _ => 2,
        }
    }
}

/// Tier flag, not a boolean (A.4.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tier {
    /// Main tier, flag 0.
    Main,
    /// High tier, flag 1, L4 and up.
    High,
}

/// H.265 level, `level_idc` 30x the level (A.4.1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// Level 1.
    L1,
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
    /// Level 6.3.
    L63,
    /// Level 7.
    L7,
    /// Level 7.1.
    L71,
    /// Level 7.2.
    L72,
}

impl Level {
    /// `level_idc`, 30x the level (A.4.1).
    #[must_use]
    pub fn idc(self) -> u16 {
        match self {
            Self::L1 => 30,
            Self::L2 => 60,
            Self::L21 => 63,
            Self::L3 => 90,
            Self::L31 => 93,
            Self::L4 => 120,
            Self::L41 => 123,
            Self::L5 => 150,
            Self::L51 => 153,
            Self::L52 => 156,
            Self::L6 => 180,
            Self::L61 => 183,
            Self::L62 => 186,
            Self::L63 => 189,
            Self::L7 => 210,
            Self::L71 => 213,
            Self::L72 => 216,
        }
    }

    /// High tier allowed at L4 and up (A.4.1).
    #[must_use]
    pub fn allows_high_tier(self) -> bool {
        !matches!(self, Self::L1 | Self::L2 | Self::L21 | Self::L3 | Self::L31)
    }

    /// Frame rate cap factor: 1/300 below L7, 1/960 from L7 (A.4.2).
    #[must_use]
    pub fn frame_rate_factor(self) -> (u32, u32) {
        match self {
            Self::L7 | Self::L71 | Self::L72 => (1, 960),
            _ => (1, 300),
        }
    }

    /// Level caps row (Tables A.8, A.9).
    #[must_use]
    pub fn caps(self) -> &'static LevelCaps {
        match self {
            Self::L1 => &CAPS[0],
            Self::L2 => &CAPS[1],
            Self::L21 => &CAPS[2],
            Self::L3 => &CAPS[3],
            Self::L31 => &CAPS[4],
            Self::L4 => &CAPS[5],
            Self::L41 => &CAPS[6],
            Self::L5 => &CAPS[7],
            Self::L51 => &CAPS[8],
            Self::L52 => &CAPS[9],
            Self::L6 => &CAPS[10],
            Self::L61 => &CAPS[11],
            Self::L62 => &CAPS[12],
            Self::L63 => &CAPS[13],
            Self::L7 => &CAPS[14],
            Self::L71 => &CAPS[15],
            Self::L72 => &CAPS[16],
        }
    }
}

/// One Tables A.8/A.9 row: idc, luma picture, CPB Main/High kbit, max
/// slices, max tiles cols/rows, luma sample rate, BR Main/High kbps,
/// `MinCR` Main/High.
pub struct LevelCaps {
    /// `level_idc`.
    pub idc: u16,
    /// Max luma picture samples.
    pub ps: u32,
    /// Max CPB Main kbit.
    pub cpb_m: u32,
    /// Max CPB High kbit.
    pub cpb_h: Option<u32>,
    /// Max slices.
    pub slices: u16,
    /// Max tile columns.
    pub tiles_x: u8,
    /// Max tile rows.
    pub tiles_y: u8,
    /// Max luma sample rate.
    pub sr: u64,
    /// Max bitrate Main kbps.
    pub br_m: u32,
    /// Max bitrate High kbps.
    pub br_h: Option<u32>,
    /// `MinCR` Main.
    pub cr_m: u8,
    /// `MinCR` High.
    pub cr_h: u8,
}

#[rustfmt::skip]
const CAPS: [LevelCaps; 17] = [
    LevelCaps { idc: 30, ps: 36_864, cpb_m: 350, cpb_h: None, slices: 16, tiles_x: 1, tiles_y: 1, sr: 552_960, br_m: 128, br_h: None, cr_m: 2, cr_h: 2 },
    LevelCaps { idc: 60, ps: 122_880, cpb_m: 1500, cpb_h: None, slices: 16, tiles_x: 1, tiles_y: 1, sr: 3_686_400, br_m: 1500, br_h: None, cr_m: 2, cr_h: 2 },
    LevelCaps { idc: 63, ps: 245_760, cpb_m: 3000, cpb_h: None, slices: 20, tiles_x: 1, tiles_y: 1, sr: 7_372_800, br_m: 3000, br_h: None, cr_m: 2, cr_h: 2 },
    LevelCaps { idc: 90, ps: 552_960, cpb_m: 6000, cpb_h: None, slices: 30, tiles_x: 2, tiles_y: 2, sr: 16_588_800, br_m: 6000, br_h: None, cr_m: 2, cr_h: 2 },
    LevelCaps { idc: 93, ps: 983_040, cpb_m: 10_000, cpb_h: None, slices: 40, tiles_x: 3, tiles_y: 3, sr: 33_177_600, br_m: 10_000, br_h: None, cr_m: 2, cr_h: 2 },
    LevelCaps { idc: 120, ps: 2_228_224, cpb_m: 12_000, cpb_h: Some(30_000), slices: 75, tiles_x: 5, tiles_y: 5, sr: 66_846_720, br_m: 12_000, br_h: Some(30_000), cr_m: 4, cr_h: 4 },
    LevelCaps { idc: 123, ps: 2_228_224, cpb_m: 20_000, cpb_h: Some(50_000), slices: 75, tiles_x: 5, tiles_y: 5, sr: 133_693_440, br_m: 20_000, br_h: Some(50_000), cr_m: 4, cr_h: 4 },
    LevelCaps { idc: 150, ps: 8_912_896, cpb_m: 25_000, cpb_h: Some(100_000), slices: 200, tiles_x: 11, tiles_y: 10, sr: 267_386_880, br_m: 25_000, br_h: Some(100_000), cr_m: 6, cr_h: 4 },
    LevelCaps { idc: 153, ps: 8_912_896, cpb_m: 40_000, cpb_h: Some(160_000), slices: 200, tiles_x: 11, tiles_y: 10, sr: 534_773_760, br_m: 40_000, br_h: Some(160_000), cr_m: 8, cr_h: 4 },
    LevelCaps { idc: 156, ps: 8_912_896, cpb_m: 60_000, cpb_h: Some(240_000), slices: 200, tiles_x: 11, tiles_y: 10, sr: 1_069_547_520, br_m: 60_000, br_h: Some(240_000), cr_m: 8, cr_h: 4 },
    LevelCaps { idc: 180, ps: 35_651_584, cpb_m: 60_000, cpb_h: Some(240_000), slices: 600, tiles_x: 22, tiles_y: 20, sr: 1_069_547_520, br_m: 60_000, br_h: Some(240_000), cr_m: 8, cr_h: 4 },
    LevelCaps { idc: 183, ps: 35_651_584, cpb_m: 120_000, cpb_h: Some(480_000), slices: 600, tiles_x: 22, tiles_y: 20, sr: 2_139_095_040, br_m: 120_000, br_h: Some(480_000), cr_m: 8, cr_h: 4 },
    LevelCaps { idc: 186, ps: 35_651_584, cpb_m: 240_000, cpb_h: Some(800_000), slices: 600, tiles_x: 22, tiles_y: 20, sr: 4_278_190_080, br_m: 240_000, br_h: Some(800_000), cr_m: 6, cr_h: 4 },
    LevelCaps { idc: 189, ps: 80_216_064, cpb_m: 240_000, cpb_h: Some(1_600_000), slices: 600, tiles_x: 22, tiles_y: 20, sr: 4_812_963_840, br_m: 320_000, br_h: Some(1_600_000), cr_m: 6, cr_h: 4 },
    LevelCaps { idc: 210, ps: 142_606_336, cpb_m: 240_000, cpb_h: Some(1_600_000), slices: 1800, tiles_x: 44, tiles_y: 40, sr: 4_812_963_840, br_m: 320_000, br_h: Some(1_600_000), cr_m: 6, cr_h: 4 },
    LevelCaps { idc: 213, ps: 142_606_336, cpb_m: 480_000, cpb_h: Some(3_200_000), slices: 1800, tiles_x: 44, tiles_y: 40, sr: 8_556_380_160, br_m: 480_000, br_h: Some(3_200_000), cr_m: 6, cr_h: 4 },
    LevelCaps { idc: 216, ps: 142_606_336, cpb_m: 960_000, cpb_h: Some(6_400_000), slices: 1800, tiles_x: 44, tiles_y: 40, sr: 17_112_760_320, br_m: 960_000, br_h: Some(6_400_000), cr_m: 6, cr_h: 4 },
];

/// IRAP NAL unit type (Table 7-1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IrapType {
    /// Broken link access, types 16-18.
    Bla,
    /// Clean random access, type 21.
    Cra,
    /// Instantaneous decoding refresh, types 19-20.
    Idr,
}

/// NAL unit class (Table 7-1, 7.4.2.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NalClass {
    /// Coded slice segment, types 0-15.
    Vcl {
        /// IRAP variant when 16-21.
        irap: Option<IrapType>,
    },
    /// Non-VCL unit.
    NonVcl,
}

impl NalClass {
    /// Classify NAL unit type 0-63.
    #[must_use]
    pub fn classify(unit_type: u8) -> Option<Self> {
        if unit_type > 63 {
            return None;
        }
        let irap = match unit_type {
            16..=18 => Some(IrapType::Bla),
            19..=20 => Some(IrapType::Idr),
            21 => Some(IrapType::Cra),
            _ => None,
        };
        if unit_type <= 21 {
            Some(Self::Vcl { irap })
        } else if matches!(unit_type, 22 | 23) {
            None
        } else {
            Some(Self::NonVcl)
        }
    }

    /// Leading pictures allowed (RADL/RASL rules).
    #[must_use]
    pub fn allows_leading(unit_type: u8) -> bool {
        !matches!(unit_type, 18 | 20)
    }
}

/// H.265 coding parameters.
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
    /// DPB reference count.
    pub refs: u8,
    /// Tile columns, when tiled.
    pub tile_cols: Option<u16>,
    /// Current-picture referencing enabled (SCC).
    pub curr_pic_ref: bool,
}

impl Params {
    /// `MaxDpbSize` for the picture (A.4.2, A-2).
    #[must_use]
    pub fn max_dpb_frames(&self) -> u32 {
        let max_ps = u64::from(self.level.caps().ps);
        let samples = self.size.samples();
        let ratio_x4 = samples.saturating_mul(4);
        let base: u32 = if self.curr_pic_ref { 7 } else { 6 };
        if ratio_x4 <= max_ps {
            (base * 4).min(16)
        } else if samples.saturating_mul(2) <= max_ps {
            (base * 2).min(16)
        } else if samples.saturating_mul(4) <= max_ps.saturating_mul(3) {
            (base * 4 / 3).min(16)
        } else {
            base.min(16)
        }
    }
}

impl Validate for Params {
    /// Check profile, tier, level, size, rate, bitrate, DPB, and tiles.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    fn validate(&self) -> Result<(), Violation> {
        if !self.profile.allows_chroma(self.chroma) {
            return Err(Violation::ChromaNotAllowed);
        }
        let max_depth = self.profile.max_bit_depth();
        if self.depth.bits() > max_depth {
            return Err(Violation::BitDepthNotAllowed {
                actual: self.depth.bits(),
                max: max_depth,
            });
        }
        if self.tier == Tier::High && !self.level.allows_high_tier() {
            return Err(Violation::TierRequired);
        }
        let caps = self.level.caps();
        let samples = self.size.samples();
        let max_samples = u64::from(caps.ps);
        if samples > max_samples {
            return Err(Violation::PictureTooLarge {
                actual: samples,
                max: max_samples,
            });
        }
        let bound = max_samples * 8;
        if !self.size.fits_sqrt_rule(bound) {
            let axis = self.size.width().max(self.size.height());
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: bound.isqrt(),
            });
        }
        let rate = samples * u64::from(self.fps);
        if rate > caps.sr {
            return Err(Violation::LumaRateExceeded {
                actual: rate,
                max: caps.sr,
            });
        }
        let max_br = match self.tier {
            Tier::Main => u64::from(caps.br_m),
            Tier::High => u64::from(caps.br_h.unwrap_or(caps.br_m)),
        } * 1000;
        if u64::from(self.bitrate_bps) > max_br {
            return Err(Violation::BitrateExceeded {
                actual: self.bitrate_bps,
                max: max_br,
            });
        }
        if u32::from(self.refs) > self.max_dpb_frames() {
            return Err(Violation::DpbExceeded {
                actual: self.refs,
                max: self.max_dpb_frames(),
            });
        }
        if let Some(cols) = self.tile_cols {
            if cols > u16::from(caps.tiles_x) {
                return Err(Violation::TileCountExceeded {
                    actual: cols,
                    max: u16::from(caps.tiles_x),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

    use super::{IrapType, Level, NalClass, Params, Profile, Tier};

    fn params_1080p() -> Params {
        Params {
            profile: Profile::Main,
            tier: Tier::Main,
            level: Level::L41,
            size: PictureSize::new(1920, 1080).expect("hd"),
            chroma: ChromaSampling::Yuv420,
            depth: BitDepth::new(8).expect("8-bit"),
            fps: 30,
            bitrate_bps: 10_000_000,
            refs: 4,
            tile_cols: None,
            curr_pic_ref: false,
        }
    }

    #[test]
    fn level_41_caps_anchor() {
        let caps = Level::L41.caps();
        assert_eq!(caps.idc, 123);
        assert_eq!(caps.ps, 2_228_224);
        assert_eq!(caps.sr, 133_693_440);
        assert_eq!(caps.br_m, 20_000);
    }

    #[test]
    fn level_62_caps_anchor() {
        let caps = Level::L62.caps();
        assert_eq!(caps.sr, 4_278_190_080);
        assert_eq!(caps.br_h, Some(800_000));
    }

    #[test]
    fn valid_1080p_passes() {
        params_1080p().validate().expect("valid");
    }

    #[test]
    fn high_tier_below_l4_fails() {
        let mut params = params_1080p();
        params.level = Level::L31;
        params.tier = Tier::High;
        assert_eq!(params.validate(), Err(Violation::TierRequired));
    }

    #[test]
    fn dpb_scales_with_picture() {
        let params = params_1080p();
        assert_eq!(params.max_dpb_frames(), 6);
        let mut small = params_1080p();
        small.size = PictureSize::new(640, 480).expect("small");
        assert_eq!(small.max_dpb_frames(), 16);
    }

    #[test]
    fn nal_classes() {
        assert_eq!(
            NalClass::classify(19),
            Some(NalClass::Vcl {
                irap: Some(IrapType::Idr)
            })
        );
        assert_eq!(NalClass::classify(22), None);
        assert_eq!(NalClass::classify(64), None);
        assert!(!NalClass::allows_leading(20));
        assert!(NalClass::allows_leading(21));
    }
}
