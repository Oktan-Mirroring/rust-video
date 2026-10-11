//! H.264 profile, level, and NAL enforcement.
//!
//! Values from ITU-T H.264 (01/2012); cites name the section.
//! HRD timing and SVC/MVC profiles are unmapped.

use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

/// H.264 profile (ITU-T H.264 Annex A.2).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Profile {
    /// Baseline, idc 66 (A.2.1).
    Baseline,
    /// Constrained Baseline, idc 66 + cs1 (A.2.1.1).
    ConstrainedBaseline,
    /// Main, idc 77 (A.2.2).
    Main,
    /// Extended, idc 88 (A.2.3).
    Extended,
    /// High, idc 100 (A.2.4).
    High,
    /// Progressive High, idc 100 + cs4 (A.2.4.1).
    ProgressiveHigh,
    /// Constrained High, idc 100 + cs4 + cs5 (A.2.4.2).
    ConstrainedHigh,
    /// High 10, idc 110 (A.2.5).
    High10,
    /// High 4:2:2, idc 122 (A.2.6).
    High422,
    /// High 4:4:4 Predictive, idc 244 (A.2.7).
    High444Predictive,
    /// Intra High 10, idc 110 + cs3 (A.2.8).
    IntraHigh10,
    /// Intra High 4:2:2, idc 122 + cs3 (A.2.9).
    IntraHigh422,
    /// Intra High 4:4:4, idc 244 + cs3 (A.2.10).
    IntraHigh444,
    /// CAVLC 4:4:4 Intra, idc 44 (A.2.11).
    Cavlc444,
}

impl Profile {
    /// `profile_idc` (Annex A.2).
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Baseline | Self::ConstrainedBaseline => 66,
            Self::Main => 77,
            Self::Extended => 88,
            Self::High | Self::ProgressiveHigh | Self::ConstrainedHigh => 100,
            Self::High10 | Self::IntraHigh10 => 110,
            Self::High422 | Self::IntraHigh422 => 122,
            Self::High444Predictive | Self::IntraHigh444 => 244,
            Self::Cavlc444 => 44,
        }
    }

    /// Resolve `profile_idc` plus constraint flags (Annex A.2).
    #[must_use]
    pub fn from_idc(idc: u8, flags: ConstraintFlags) -> Option<Self> {
        match idc {
            66 if flags.set(1) => Some(Self::ConstrainedBaseline),
            66 => Some(Self::Baseline),
            77 => Some(Self::Main),
            88 => Some(Self::Extended),
            100 if flags.set(4) && flags.set(5) => Some(Self::ConstrainedHigh),
            100 if flags.set(4) => Some(Self::ProgressiveHigh),
            100 => Some(Self::High),
            110 if flags.set(3) => Some(Self::IntraHigh10),
            110 => Some(Self::High10),
            122 if flags.set(3) => Some(Self::IntraHigh422),
            122 => Some(Self::High422),
            244 if flags.set(3) => Some(Self::IntraHigh444),
            244 => Some(Self::High444Predictive),
            44 => Some(Self::Cavlc444),
            _ => None,
        }
    }

    /// Maximum bit depth (A.2.4-7).
    #[must_use]
    pub fn max_bit_depth(self) -> u8 {
        match self {
            Self::High444Predictive | Self::IntraHigh444 => 14,
            Self::High10 | Self::IntraHigh10 | Self::High422 | Self::IntraHigh422 => 10,
            _ => 8,
        }
    }

    /// Allowed chroma sampling (Table 6-1, A.2.4-7).
    #[must_use]
    pub fn allows_chroma(self, chroma: ChromaSampling) -> bool {
        match self {
            Self::High422 | Self::IntraHigh422 => !matches!(chroma, ChromaSampling::Yuv444),
            Self::High444Predictive | Self::IntraHigh444 | Self::Cavlc444 => true,
            _ => matches!(chroma, ChromaSampling::Mono | ChromaSampling::Yuv420),
        }
    }

    /// B slices allowed (A.2.1-2, A.2.4.2).
    #[must_use]
    pub fn allows_b_slice(self) -> bool {
        !matches!(
            self,
            Self::Baseline
                | Self::ConstrainedBaseline
                | Self::ConstrainedHigh
                | Self::IntraHigh10
                | Self::IntraHigh422
                | Self::IntraHigh444
                | Self::Cavlc444
        )
    }

    /// CABAC allowed (A.2.1, A.2.3).
    #[must_use]
    pub fn allows_cabac(self) -> bool {
        !matches!(
            self,
            Self::Baseline | Self::ConstrainedBaseline | Self::Extended | Self::Cavlc444
        )
    }

    /// FMO banned (A.2.2, A.2.4).
    #[must_use]
    pub fn bans_fmo(self) -> bool {
        matches!(self, Self::Main | Self::High)
    }

    /// ASO banned (A.2.2, A.2.4).
    #[must_use]
    pub fn bans_aso(self) -> bool {
        matches!(self, Self::Main | Self::High)
    }

    /// Data partitions (NAL 2-4) banned (A.2).
    #[must_use]
    pub fn bans_partitions(self) -> bool {
        matches!(
            self,
            Self::Baseline | Self::ConstrainedBaseline | Self::Main | Self::High
        )
    }

    /// IDR-only pictures (A.2.8-11).
    #[must_use]
    pub fn intra_only(self) -> bool {
        matches!(
            self,
            Self::IntraHigh10 | Self::IntraHigh422 | Self::IntraHigh444 | Self::Cavlc444
        )
    }

    /// BR/CPB scale factors, VCL and NAL (A.3.1i,j, A.3.3g,h, Table A-2).
    #[must_use]
    pub fn rate_scale(self) -> (u32, u32) {
        match self.idc() {
            110 => (3000, 3600),
            122 | 244 | 44 => (4000, 4800),
            100 => (1250, 1500),
            _ => (1000, 1200),
        }
    }
}

/// constraint_set0-5 flags from the SPS.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConstraintFlags(u8);

impl ConstraintFlags {
    /// Flags from the raw SPS byte.
    #[must_use]
    pub fn new(raw: u8) -> Self {
        Self(raw)
    }

    /// `constraint_setN_flag`, N 0-5.
    #[must_use]
    pub fn set(&self, flag: u8) -> bool {
        flag < 6 && self.0 & (1 << (7 - flag)) != 0
    }
}

/// H.264 level (ITU-T H.264 Table A-1).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Level {
    /// Level 1.
    L1,
    /// Level 1b.
    L1b,
    /// Level 1.1.
    L11,
    /// Level 1.2.
    L12,
    /// Level 1.3.
    L13,
    /// Level 2.
    L2,
    /// Level 2.1.
    L21,
    /// Level 2.2.
    L22,
    /// Level 3.
    L3,
    /// Level 3.1.
    L31,
    /// Level 3.2.
    L32,
    /// Level 4.
    L4,
    /// Level 4.1.
    L41,
    /// Level 4.2.
    L42,
    /// Level 5.
    L5,
    /// Level 5.1.
    L51,
    /// Level 5.2.
    L52,
}

/// `level_idc` for 1b under High profiles (A.3).
pub const ONE_B_HIGH_IDC: u8 = 9;

impl Level {
    /// `level_idc`; 1b reports 11, the base signalling (A.3).
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::L1 => 10,
            Self::L1b | Self::L11 => 11,
            Self::L12 => 12,
            Self::L13 => 13,
            Self::L2 => 20,
            Self::L21 => 21,
            Self::L22 => 22,
            Self::L3 => 30,
            Self::L31 => 31,
            Self::L32 => 32,
            Self::L4 => 40,
            Self::L41 => 41,
            Self::L42 => 42,
            Self::L5 => 50,
            Self::L51 => 51,
            Self::L52 => 52,
        }
    }

    /// Level caps row (Table A-1).
    #[must_use]
    pub fn caps(self) -> &'static LevelCaps {
        match self {
            Self::L1 => &CAPS[0],
            Self::L1b => &CAPS[1],
            Self::L11 => &CAPS[2],
            Self::L12 => &CAPS[3],
            Self::L13 => &CAPS[4],
            Self::L2 => &CAPS[5],
            Self::L21 => &CAPS[6],
            Self::L22 => &CAPS[7],
            Self::L3 => &CAPS[8],
            Self::L31 => &CAPS[9],
            Self::L32 => &CAPS[10],
            Self::L4 => &CAPS[11],
            Self::L41 => &CAPS[12],
            Self::L42 => &CAPS[13],
            Self::L5 => &CAPS[14],
            Self::L51 => &CAPS[15],
            Self::L52 => &CAPS[16],
        }
    }
}

/// One Table A-1 row: idc, MB/s, frame MB, DPB MB, BR kbps, CPB kbits,
/// `MinCR`, motion vectors per 2MB.
pub struct LevelCaps {
    /// `level_idc`.
    pub idc: u8,
    /// Max macroblocks/s.
    pub mbps: u32,
    /// Max frame macroblocks.
    pub fs_mb: u32,
    /// Max DPB macroblocks.
    pub dpb_mb: u32,
    /// Max bitrate kbps.
    pub br_kbps: u32,
    /// Max CPB kbits.
    pub cpb_kbits: u32,
    /// Min compression ratio.
    pub min_cr: u8,
    /// Max motion vectors per 2MB.
    pub mvs: Option<u8>,
}

#[rustfmt::skip]
const CAPS: [LevelCaps; 17] = [
    LevelCaps { idc: 10, mbps: 1485, fs_mb: 99, dpb_mb: 396, br_kbps: 64, cpb_kbits: 175, min_cr: 2, mvs: None },
    LevelCaps { idc: 11, mbps: 1485, fs_mb: 99, dpb_mb: 396, br_kbps: 128, cpb_kbits: 350, min_cr: 2, mvs: None },
    LevelCaps { idc: 11, mbps: 3000, fs_mb: 396, dpb_mb: 900, br_kbps: 192, cpb_kbits: 500, min_cr: 2, mvs: None },
    LevelCaps { idc: 12, mbps: 6000, fs_mb: 396, dpb_mb: 2376, br_kbps: 384, cpb_kbits: 1000, min_cr: 2, mvs: None },
    LevelCaps { idc: 13, mbps: 11_880, fs_mb: 396, dpb_mb: 2376, br_kbps: 768, cpb_kbits: 2000, min_cr: 2, mvs: None },
    LevelCaps { idc: 20, mbps: 11_880, fs_mb: 396, dpb_mb: 2376, br_kbps: 2000, cpb_kbits: 2000, min_cr: 2, mvs: None },
    LevelCaps { idc: 21, mbps: 19_800, fs_mb: 792, dpb_mb: 4752, br_kbps: 4000, cpb_kbits: 4000, min_cr: 2, mvs: None },
    LevelCaps { idc: 22, mbps: 20_250, fs_mb: 1620, dpb_mb: 8100, br_kbps: 4000, cpb_kbits: 4000, min_cr: 2, mvs: None },
    LevelCaps { idc: 30, mbps: 40_500, fs_mb: 1620, dpb_mb: 8100, br_kbps: 10_000, cpb_kbits: 10_000, min_cr: 2, mvs: Some(32) },
    LevelCaps { idc: 31, mbps: 108_000, fs_mb: 3600, dpb_mb: 18_000, br_kbps: 14_000, cpb_kbits: 14_000, min_cr: 4, mvs: Some(16) },
    LevelCaps { idc: 32, mbps: 216_000, fs_mb: 5120, dpb_mb: 20_480, br_kbps: 20_000, cpb_kbits: 20_000, min_cr: 4, mvs: Some(16) },
    LevelCaps { idc: 40, mbps: 245_760, fs_mb: 8192, dpb_mb: 32_768, br_kbps: 20_000, cpb_kbits: 25_000, min_cr: 4, mvs: Some(16) },
    LevelCaps { idc: 41, mbps: 245_760, fs_mb: 8192, dpb_mb: 32_768, br_kbps: 50_000, cpb_kbits: 62_500, min_cr: 2, mvs: Some(16) },
    LevelCaps { idc: 42, mbps: 522_240, fs_mb: 8704, dpb_mb: 34_816, br_kbps: 50_000, cpb_kbits: 62_500, min_cr: 2, mvs: Some(16) },
    LevelCaps { idc: 50, mbps: 589_824, fs_mb: 22_080, dpb_mb: 110_400, br_kbps: 135_000, cpb_kbits: 135_000, min_cr: 2, mvs: Some(16) },
    LevelCaps { idc: 51, mbps: 983_040, fs_mb: 36_864, dpb_mb: 184_320, br_kbps: 240_000, cpb_kbits: 240_000, min_cr: 2, mvs: Some(16) },
    LevelCaps { idc: 52, mbps: 2_073_600, fs_mb: 36_864, dpb_mb: 184_320, br_kbps: 240_000, cpb_kbits: 240_000, min_cr: 2, mvs: Some(16) },
];

/// Slice rate in slices/s, Table A-4 rows L3 and up.
///
/// Rows below L3 are unmapped.
#[must_use]
pub fn slice_rate(level: Level) -> Option<u16> {
    match level {
        Level::L3 => Some(22),
        Level::L31 | Level::L32 | Level::L4 => Some(60),
        Level::L41 | Level::L42 | Level::L5 | Level::L51 | Level::L52 => Some(24),
        _ => None,
    }
}

/// Stream format selecting the rate scale.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StreamFormat {
    /// Video coding layer.
    Vcl,
    /// NAL byte stream, 1.2x VCL.
    Nal,
}

/// H.264 coding parameters.
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
    /// Stream format.
    pub format: StreamFormat,
}

impl Validate for Params {
    /// Check profile, level, size, rate, bitrate, and DPB caps.
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
        let caps = self.level.caps();
        let max_samples = u64::from(caps.fs_mb) * 256;
        let samples = self.size.samples();
        if samples > max_samples {
            return Err(Violation::PictureTooLarge {
                actual: samples,
                max: max_samples,
            });
        }
        let bound = u64::from(caps.fs_mb) * 256 * 8;
        if !self.size.fits_sqrt_rule(bound) {
            let axis = self.size.width().max(self.size.height());
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: bound.isqrt(),
            });
        }
        let rate = samples * u64::from(self.fps);
        let max_rate = u64::from(caps.mbps) * 256;
        if rate > max_rate {
            return Err(Violation::LumaRateExceeded {
                actual: rate,
                max: max_rate,
            });
        }
        let (vcl, nal) = self.profile.rate_scale();
        let scale = match self.format {
            StreamFormat::Vcl => vcl,
            StreamFormat::Nal => nal,
        };
        let max_bitrate = u64::from(caps.br_kbps) * u64::from(scale);
        if u64::from(self.bitrate_bps) > max_bitrate {
            return Err(Violation::BitrateExceeded {
                actual: self.bitrate_bps,
                max: max_bitrate,
            });
        }
        let dpb_frames = (u64::from(caps.dpb_mb) * 256 / samples.max(1)).min(16);
        if u64::from(self.refs) > dpb_frames {
            return Err(Violation::DpbExceeded {
                actual: self.refs,
                max: dpb_frames.min(u64::from(u32::MAX)) as u32,
            });
        }
        Ok(())
    }
}

/// Parsed NAL header (ITU-T H.264 Table 7-1, 7.4.1).
pub struct NalInfo {
    /// NAL unit type 0-31.
    pub unit_type: u8,
    /// `nal_ref_idc`.
    pub ref_idc: u8,
}

impl NalInfo {
    /// Parse and check the first NAL byte.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a set forbidden bit, a reserved type, or a
    /// `ref_idc` against the type rule.
    pub fn validate(byte: u8) -> Result<Self, Violation> {
        if byte & 0x80 != 0 {
            return Err(Violation::NalForbiddenBit);
        }
        let unit_type = byte & 0x1f;
        let ref_idc = (byte >> 5) & 0x03;
        if matches!(unit_type, 16..=18 | 21..=23) {
            return Err(Violation::ReservedNal(unit_type));
        }
        let zero_required = matches!(unit_type, 6 | 9..=12);
        let nonzero_required = matches!(unit_type, 5 | 7 | 8);
        if (zero_required && ref_idc != 0) || (nonzero_required && ref_idc == 0) {
            return Err(Violation::RefIdc {
                nal_type: unit_type,
            });
        }
        Ok(Self { unit_type, ref_idc })
    }

    /// True for NAL types an IDR access unit may hold (no 1-4).
    #[must_use]
    pub fn allowed_after_idr(unit_type: u8) -> bool {
        !matches!(unit_type, 1..=4)
    }
}

#[cfg(test)]
mod tests {
    use crate::common::{BitDepth, ChromaSampling, PictureSize, Validate, Violation};

    use super::{slice_rate, ConstraintFlags, Level, NalInfo, Params, Profile, StreamFormat};

    fn params_1080p() -> Params {
        Params {
            profile: Profile::High,
            level: Level::L41,
            size: PictureSize::new(1920, 1080).expect("hd"),
            chroma: ChromaSampling::Yuv420,
            depth: BitDepth::new(8).expect("8-bit"),
            fps: 30,
            bitrate_bps: 10_000_000,
            refs: 4,
            format: StreamFormat::Vcl,
        }
    }

    #[test]
    fn level_41_caps_anchor() {
        let caps = Level::L41.caps();
        assert_eq!(caps.idc, 41);
        assert_eq!(caps.mbps, 245_760);
        assert_eq!(caps.fs_mb, 8192);
        assert_eq!(caps.br_kbps, 50_000);
    }

    #[test]
    fn level_52_caps_anchor() {
        let caps = Level::L52.caps();
        assert_eq!(caps.mbps, 2_073_600);
        assert_eq!(caps.fs_mb, 36_864);
    }

    #[test]
    fn profile_from_idc_flags() {
        let plain = ConstraintFlags::new(0);
        assert_eq!(Profile::from_idc(100, plain), Some(Profile::High));
        let cs4 = ConstraintFlags::new(0b0000_1000);
        assert_eq!(Profile::from_idc(100, cs4), Some(Profile::ProgressiveHigh));
        assert_eq!(Profile::from_idc(200, plain), None);
    }

    #[test]
    fn valid_1080p_passes() {
        params_1080p().validate().expect("valid");
    }

    #[test]
    fn oversize_picture_fails() {
        let mut params = params_1080p();
        params.size = PictureSize::new(8192, 4320).expect("8k");
        assert!(matches!(
            params.validate(),
            Err(Violation::PictureTooLarge { .. })
        ));
    }

    #[test]
    fn bitrate_fails() {
        let mut params = params_1080p();
        params.bitrate_bps = 100_000_000;
        assert!(matches!(
            params.validate(),
            Err(Violation::BitrateExceeded { .. })
        ));
    }

    #[test]
    fn chroma_gate() {
        let mut params = params_1080p();
        params.chroma = ChromaSampling::Yuv444;
        assert_eq!(params.validate(), Err(Violation::ChromaNotAllowed));
    }

    #[test]
    fn bit_depth_gate() {
        let mut params = params_1080p();
        params.depth = BitDepth::new(10).expect("10-bit");
        assert!(matches!(
            params.validate(),
            Err(Violation::BitDepthNotAllowed { .. })
        ));
    }

    #[test]
    fn nal_header_rules() {
        assert!(NalInfo::validate(0x80).is_err());
        assert!(NalInfo::validate(0x10).is_err());
        let idr = NalInfo::validate(0x65).expect("IDR");
        assert_eq!(idr.unit_type, 5);
        NalInfo::validate(0x06).expect("SEI with ref_idc 0");
        assert!(NalInfo::validate(0x26).is_err());
        assert!(NalInfo::allowed_after_idr(5));
        assert!(!NalInfo::allowed_after_idr(1));
    }

    #[test]
    fn slice_rate_rows() {
        assert_eq!(slice_rate(Level::L3), Some(22));
        assert_eq!(slice_rate(Level::L4), Some(60));
        assert_eq!(slice_rate(Level::L41), Some(24));
        assert_eq!(slice_rate(Level::L2), None);
    }
}
