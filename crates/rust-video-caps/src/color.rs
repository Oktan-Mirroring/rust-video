//! Encode color vocabulary: primaries, transfer, matrix, range, HDR metadata.
//!
//! Coded values follow ITU-T H.264 Table E-1/E-2/E-3; the same ids ride
//! H.265, AV1, and VP9 colour signalling.

/// Colour primaries: chromaticity of the source primaries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ColorPrimaries {
    /// BT.709 primaries.
    Bt709,
    /// Unspecified primaries.
    Unspecified,
    /// BT.470M primaries.
    Bt470M,
    /// BT.470BG primaries.
    Bt470Bg,
    /// SMPTE 170M primaries.
    Smpte170M,
    /// SMPTE 240M primaries.
    Smpte240M,
    /// Generic film primaries.
    Film,
    /// BT.2020 primaries.
    Bt2020,
    /// SMPTE ST 428-1 primaries.
    SmpteSt428,
    /// SMPTE ST 431-2 primaries.
    SmpteSt431,
    /// SMPTE ST 432-1 primaries.
    SmpteSt432,
    /// JEDEC P22 phosphors.
    JedecP22,
}

impl ColorPrimaries {
    /// Coded `colour_primaries` value.
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Bt709 => 1,
            Self::Unspecified => 2,
            Self::Bt470M => 4,
            Self::Bt470Bg => 5,
            Self::Smpte170M => 6,
            Self::Smpte240M => 7,
            Self::Film => 8,
            Self::Bt2020 => 9,
            Self::SmpteSt428 => 10,
            Self::SmpteSt431 => 11,
            Self::SmpteSt432 => 12,
            Self::JedecP22 => 22,
        }
    }
}

impl Default for ColorPrimaries {
    /// BT.709: SDR baseline.
    fn default() -> Self {
        Self::Bt709
    }
}

/// Transfer characteristics: opto-electronic transfer function.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TransferCharacteristics {
    /// BT.709 transfer.
    Bt709,
    /// Unspecified transfer.
    Unspecified,
    /// Gamma 2.2 transfer.
    Gamma22,
    /// Gamma 2.8 transfer.
    Gamma28,
    /// SMPTE 170M transfer.
    Smpte170M,
    /// SMPTE 240M transfer.
    Smpte240M,
    /// Linear transfer.
    Linear,
    /// Logarithmic 100:1 transfer.
    Log100,
    /// Logarithmic square-root 100:1 transfer.
    LogSqrt,
    /// IEC 61966-2-4 transfer.
    Iec61966Part4,
    /// BT.1361 extended transfer.
    Bt1361,
    /// IEC 61966-2-1 transfer.
    Iec61966Part1,
    /// BT.2020 10-bit transfer.
    Bt2020Ten,
    /// BT.2020 12-bit transfer.
    Bt2020Twelve,
    /// SMPTE ST 2084 perceptual quantizer transfer.
    SmpteSt2084,
    /// SMPTE ST 428-1 transfer.
    SmpteSt428,
    /// ARIB STD-B67 hybrid log-gamma transfer.
    Hlg,
}

impl TransferCharacteristics {
    /// Coded `transfer_characteristics` value.
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Bt709 => 1,
            Self::Unspecified => 2,
            Self::Gamma22 => 4,
            Self::Gamma28 => 5,
            Self::Smpte170M => 6,
            Self::Smpte240M => 7,
            Self::Linear => 8,
            Self::Log100 => 9,
            Self::LogSqrt => 10,
            Self::Iec61966Part4 => 11,
            Self::Bt1361 => 12,
            Self::Iec61966Part1 => 13,
            Self::Bt2020Ten => 14,
            Self::Bt2020Twelve => 15,
            Self::SmpteSt2084 => 16,
            Self::SmpteSt428 => 17,
            Self::Hlg => 18,
        }
    }
}

impl Default for TransferCharacteristics {
    /// BT.709: SDR baseline.
    fn default() -> Self {
        Self::Bt709
    }
}

/// Matrix coefficients: luma/chroma derivation from primaries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MatrixCoefficients {
    /// Identity RGB matrix.
    Gbr,
    /// BT.709 matrix.
    Bt709,
    /// Unspecified matrix.
    Unspecified,
    /// FCC matrix.
    Fcc,
    /// BT.470BG matrix.
    Bt470Bg,
    /// SMPTE 170M matrix.
    Smpte170M,
    /// SMPTE 240M matrix.
    Smpte240M,
    /// `YCgCo` matrix.
    Ycgco,
    /// BT.2020 non-constant luminance matrix.
    Bt2020Ncl,
    /// BT.2020 constant luminance matrix.
    Bt2020Cl,
    /// SMPTE ST 2085 matrix.
    SmpteSt2085,
    /// Chroma-derived non-constant luminance matrix.
    ChromaDerivedNcl,
    /// Chroma-derived constant luminance matrix.
    ChromaDerivedCl,
    /// `ICtCp` matrix.
    Ictcp,
}

impl MatrixCoefficients {
    /// Coded `matrix_coefficients` value.
    #[must_use]
    pub fn idc(self) -> u8 {
        match self {
            Self::Gbr => 0,
            Self::Bt709 => 1,
            Self::Unspecified => 2,
            Self::Fcc => 4,
            Self::Bt470Bg => 5,
            Self::Smpte170M => 6,
            Self::Smpte240M => 7,
            Self::Ycgco => 8,
            Self::Bt2020Ncl => 9,
            Self::Bt2020Cl => 10,
            Self::SmpteSt2085 => 11,
            Self::ChromaDerivedNcl => 12,
            Self::ChromaDerivedCl => 13,
            Self::Ictcp => 14,
        }
    }
}

impl Default for MatrixCoefficients {
    /// BT.709: SDR baseline.
    fn default() -> Self {
        Self::Bt709
    }
}

/// Color range: quantized levels carrying luma and chroma.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ColorRange {
    /// Studio range: 16-235 luma, 16-240 chroma.
    Limited,
    /// Full range: 0-255.
    Full,
}

impl Default for ColorRange {
    /// Limited: BT.709 SDR levels.
    fn default() -> Self {
        Self::Limited
    }
}

/// Mastering display colour volume: integer chromaticity and luminance.
///
/// Chromaticity in 1/50000 units, luminance in 1/10000 cd/m2 units,
/// matching the mastering display SEI coding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MasteringDisplay {
    /// Red x chromaticity.
    pub red_x: u16,
    /// Red y chromaticity.
    pub red_y: u16,
    /// Green x chromaticity.
    pub green_x: u16,
    /// Green y chromaticity.
    pub green_y: u16,
    /// Blue x chromaticity.
    pub blue_x: u16,
    /// Blue y chromaticity.
    pub blue_y: u16,
    /// White point x chromaticity.
    pub white_x: u16,
    /// White point y chromaticity.
    pub white_y: u16,
    /// Maximum luminance.
    pub max_luminance: u32,
    /// Minimum luminance.
    pub min_luminance: u32,
}

/// Content light level: integer peak and average luminance in cd/m2.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentLight {
    /// Maximum content light level.
    pub max_cll: u16,
    /// Maximum frame-average light level.
    pub max_fall: u16,
}

/// Color description: primaries, transfer, matrix, range, HDR metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ColorConfig {
    /// Color primaries.
    pub primaries: ColorPrimaries,
    /// Transfer characteristics.
    pub transfer: TransferCharacteristics,
    /// Matrix coefficients.
    pub matrix: MatrixCoefficients,
    /// Color range.
    pub range: ColorRange,
    /// Mastering display, present for HDR.
    pub mastering: Option<MasteringDisplay>,
    /// Content light, present for HDR.
    pub light: Option<ContentLight>,
}

impl ColorConfig {
    /// True with mastering or light metadata attached.
    #[must_use]
    pub fn is_hdr(&self) -> bool {
        self.mastering.is_some() || self.light.is_some()
    }
}

impl Default for ColorConfig {
    /// BT.709 SDR: 709 primaries/transfer/matrix, limited range, no HDR.
    fn default() -> Self {
        Self {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristics::Bt709,
            matrix: MatrixCoefficients::Bt709,
            range: ColorRange::Limited,
            mastering: None,
            light: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorConfig, ColorPrimaries, ColorRange, ContentLight, MasteringDisplay,
        MatrixCoefficients, TransferCharacteristics,
    };

    #[test]
    fn default_is_bt709_sdr() {
        let color = ColorConfig::default();
        assert_eq!(color.primaries, ColorPrimaries::Bt709);
        assert_eq!(color.transfer, TransferCharacteristics::Bt709);
        assert_eq!(color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(color.range, ColorRange::Limited);
        assert_eq!(color.mastering, None);
        assert_eq!(color.light, None);
        assert!(!color.is_hdr());
    }

    #[test]
    fn primaries_idc_anchors() {
        assert_eq!(ColorPrimaries::Bt709.idc(), 1);
        assert_eq!(ColorPrimaries::Unspecified.idc(), 2);
        assert_eq!(ColorPrimaries::Bt2020.idc(), 9);
        assert_eq!(ColorPrimaries::SmpteSt432.idc(), 12);
        assert_eq!(ColorPrimaries::JedecP22.idc(), 22);
    }

    #[test]
    fn transfer_idc_anchors() {
        assert_eq!(TransferCharacteristics::Bt709.idc(), 1);
        assert_eq!(TransferCharacteristics::Linear.idc(), 8);
        assert_eq!(TransferCharacteristics::SmpteSt2084.idc(), 16);
        assert_eq!(TransferCharacteristics::Hlg.idc(), 18);
    }

    #[test]
    fn matrix_idc_anchors() {
        assert_eq!(MatrixCoefficients::Gbr.idc(), 0);
        assert_eq!(MatrixCoefficients::Bt709.idc(), 1);
        assert_eq!(MatrixCoefficients::Bt2020Ncl.idc(), 9);
        assert_eq!(MatrixCoefficients::Ictcp.idc(), 14);
    }

    #[test]
    fn mastering_stores_integer_volume() {
        let mastering = MasteringDisplay {
            red_x: 34_000,
            red_y: 16_000,
            green_x: 13_250,
            green_y: 37_500,
            blue_x: 7_500,
            blue_y: 3_000,
            white_x: 15_625,
            white_y: 16_450,
            max_luminance: 10_000_000,
            min_luminance: 1,
        };
        assert_eq!(mastering.red_x, 34_000);
        assert_eq!(mastering.max_luminance, 10_000_000);
        assert_eq!(mastering.min_luminance, 1);
    }

    #[test]
    fn light_stores_integer_levels() {
        let light = ContentLight {
            max_cll: 1000,
            max_fall: 400,
        };
        assert_eq!(light.max_cll, 1000);
        assert_eq!(light.max_fall, 400);
    }

    #[test]
    fn hdr_metadata_marks_hdr() {
        let color = ColorConfig {
            light: Some(ContentLight {
                max_cll: 1000,
                max_fall: 400,
            }),
            ..Default::default()
        };
        assert!(color.is_hdr());
    }
}
