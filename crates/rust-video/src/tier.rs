//! Codec identity and tier resolution.

use std::fmt;

/// Host operating system family.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Platform {
    /// Windows.
    Windows,
    /// Linux.
    Linux,
    /// Android.
    Android,
    /// Any other OS.
    Other,
}

impl Platform {
    /// Hardware tier native to the platform, if any.
    #[must_use]
    fn hardware_tier(self) -> Option<Tier> {
        match self {
            Self::Windows => Some(Tier::D3d11),
            Self::Linux => Some(Tier::VaApi),
            Self::Android => Some(Tier::MediaCodec),
            Self::Other => None,
        }
    }
}

/// H.265 software backend maturity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SoftwareH265 {
    /// No realtime H.265 software backend.
    Absent,
    /// Realtime H.265 software backend present.
    Realtime,
}

/// Encode/decode backend tier.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Tier {
    /// Vulkan video.
    Vulkan,
    /// VA-API.
    VaApi,
    /// Direct3D 11.
    D3d11,
    /// Android `MediaCodec`.
    MediaCodec,
    /// Software.
    Software,
}

/// Tier selection policy.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TierPreference {
    /// First available tier in platform order.
    Auto,
    /// One tier or failure.
    Force(Tier),
}

/// Video codec.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VideoCodec {
    /// H.264 codec.
    H264,
    /// H.265 codec.
    H265,
    /// AV1 codec.
    Av1,
    /// VP9 codec.
    Vp9,
    /// VP8 codec.
    Vp8,
    /// AV2 codec.
    Av2,
}

/// Tier availability on one machine.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Capabilities {
    vulkan: bool,
    platform_hw: bool,
    software: bool,
    h265_sw: SoftwareH265,
}

impl Capabilities {
    /// No tier available.
    #[must_use]
    pub fn none() -> Self {
        Self {
            vulkan: false,
            platform_hw: false,
            software: false,
            h265_sw: SoftwareH265::Absent,
        }
    }

    /// Vulkan video available.
    #[must_use]
    pub fn with_vulkan(mut self) -> Self {
        self.vulkan = true;
        self
    }

    /// Platform hardware tier available.
    #[must_use]
    pub fn with_platform_hw(mut self) -> Self {
        self.platform_hw = true;
        self
    }

    /// Software backend available.
    #[must_use]
    pub fn with_software(mut self) -> Self {
        self.software = true;
        self
    }

    /// Realtime H.265 software backend available.
    #[must_use]
    pub fn with_h265_software(mut self) -> Self {
        self.h265_sw = SoftwareH265::Realtime;
        self
    }

    /// Tier usable for `codec` on `platform`.
    #[must_use]
    fn supports(&self, codec: VideoCodec, tier: Tier, platform: Platform) -> bool {
        let present = match tier {
            Tier::Vulkan => self.vulkan,
            Tier::Software => self.software,
            hardware => platform.hardware_tier() == Some(hardware) && self.platform_hw,
        };
        present
            && (tier != Tier::Software
                || codec != VideoCodec::H265
                || self.h265_sw == SoftwareH265::Realtime)
    }
}

/// Tier resolution failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TierError {
    codec: VideoCodec,
    preference: TierPreference,
}

impl TierError {
    /// Resolution failure for `codec` under `preference`.
    #[must_use]
    pub(crate) fn new(codec: VideoCodec, preference: TierPreference) -> Self {
        Self { codec, preference }
    }

    /// Codec the failed resolution targeted.
    #[must_use]
    pub fn codec(&self) -> VideoCodec {
        self.codec
    }

    /// Preference the failed resolution used.
    #[must_use]
    pub fn preference(&self) -> TierPreference {
        self.preference
    }
}

impl fmt::Display for TierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.preference {
            TierPreference::Force(tier) => {
                write!(f, "tier {tier:?} unavailable for codec {:?}", self.codec)
            }
            TierPreference::Auto => write!(f, "no tier available for codec {:?}", self.codec),
        }
    }
}

impl std::error::Error for TierError {}

/// Tier order for automatic selection: Vulkan, platform hardware, software.
///
/// # Arguments
///
/// * `platform` - host OS; its hardware tier fills the middle slot.
///
/// # Returns
///
/// Three tiers in probe order. Platforms without a hardware tier collapse the middle slot to software.
#[must_use]
pub fn auto_order(platform: Platform) -> [Tier; 3] {
    [
        Tier::Vulkan,
        platform.hardware_tier().unwrap_or(Tier::Software),
        Tier::Software,
    ]
}

/// Host OS family.
#[must_use]
pub fn host_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "android") {
        Platform::Android
    } else {
        Platform::Other
    }
}

/// Tier for `codec` under `pref` given `caps` on `platform`.
///
/// # Arguments
///
/// * `codec` - codec to encode or decode.
/// * `pref` - automatic order or one forced tier.
/// * `caps` - tier availability on this machine.
/// * `platform` - host OS; selects the hardware tier.
///
/// # Errors
///
/// Returns `Err` when the forced tier is missing or H.265 software lacks a realtime backend, or when automatic selection finds no usable tier.
///
/// # Returns
///
/// Selected tier.
pub fn resolve(
    codec: VideoCodec,
    pref: TierPreference,
    caps: &Capabilities,
    platform: Platform,
) -> Result<Tier, TierError> {
    match pref {
        TierPreference::Force(tier) => {
            if caps.supports(codec, tier, platform) {
                Ok(tier)
            } else {
                Err(TierError::new(codec, pref))
            }
        }
        TierPreference::Auto => {
            for tier in auto_order(platform) {
                if caps.supports(codec, tier, platform) {
                    return Ok(tier);
                }
            }
            Err(TierError::new(codec, pref))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        auto_order, host_platform, resolve, Capabilities, Platform, Tier, TierError,
        TierPreference, VideoCodec,
    };

    #[test]
    fn auto_order_windows() {
        assert_eq!(
            auto_order(Platform::Windows),
            [Tier::Vulkan, Tier::D3d11, Tier::Software]
        );
    }

    #[test]
    fn auto_order_linux() {
        assert_eq!(
            auto_order(Platform::Linux),
            [Tier::Vulkan, Tier::VaApi, Tier::Software]
        );
    }

    #[test]
    fn auto_order_android() {
        assert_eq!(
            auto_order(Platform::Android),
            [Tier::Vulkan, Tier::MediaCodec, Tier::Software]
        );
    }

    #[test]
    fn auto_order_other_collapses_middle_slot() {
        assert_eq!(
            auto_order(Platform::Other),
            [Tier::Vulkan, Tier::Software, Tier::Software]
        );
    }

    #[test]
    fn host_platform_matches_build_target() {
        let expected = if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "android") {
            Platform::Android
        } else {
            Platform::Other
        };
        assert_eq!(host_platform(), expected);
    }

    #[test]
    fn auto_prefers_vulkan() {
        let caps = Capabilities::none()
            .with_vulkan()
            .with_platform_hw()
            .with_software();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::Vulkan)
        );
    }

    #[test]
    fn auto_falls_back_to_platform_hardware() {
        let caps = Capabilities::none().with_platform_hw().with_software();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::VaApi)
        );
    }

    #[test]
    fn auto_falls_back_to_software() {
        let caps = Capabilities::none().with_software();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::Software)
        );
    }

    #[test]
    fn auto_without_tiers_fails() {
        let caps = Capabilities::none();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Err(TierError::new(VideoCodec::H264, TierPreference::Auto))
        );
    }

    #[test]
    fn force_present_tier_succeeds() {
        let caps = Capabilities::none().with_platform_hw();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Force(Tier::VaApi),
                &caps,
                Platform::Linux
            ),
            Ok(Tier::VaApi)
        );
    }

    #[test]
    fn force_missing_vulkan_fails() {
        let caps = Capabilities::none().with_software();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Force(Tier::Vulkan),
                &caps,
                Platform::Linux
            ),
            Err(TierError::new(
                VideoCodec::H264,
                TierPreference::Force(Tier::Vulkan)
            ))
        );
    }

    #[test]
    fn force_foreign_hardware_tier_fails() {
        let caps = Capabilities::none().with_platform_hw();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Force(Tier::VaApi),
                &caps,
                Platform::Windows
            ),
            Err(TierError::new(
                VideoCodec::H264,
                TierPreference::Force(Tier::VaApi)
            ))
        );
    }

    #[test]
    fn h265_software_needs_realtime_backend_auto() {
        let caps = Capabilities::none().with_software();
        assert_eq!(
            resolve(
                VideoCodec::H265,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Err(TierError::new(VideoCodec::H265, TierPreference::Auto))
        );
        let caps = Capabilities::none().with_software().with_h265_software();
        assert_eq!(
            resolve(
                VideoCodec::H265,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::Software)
        );
    }

    #[test]
    fn h265_software_needs_realtime_backend_forced() {
        let pref = TierPreference::Force(Tier::Software);
        let caps = Capabilities::none().with_software();
        assert_eq!(
            resolve(VideoCodec::H265, pref, &caps, Platform::Linux),
            Err(TierError::new(VideoCodec::H265, pref))
        );
        let caps = Capabilities::none().with_software().with_h265_software();
        assert_eq!(
            resolve(VideoCodec::H265, pref, &caps, Platform::Linux),
            Ok(Tier::Software)
        );
    }

    #[test]
    fn h265_gate_leaves_hardware_tiers_open() {
        let caps = Capabilities::none().with_vulkan().with_platform_hw();
        assert_eq!(
            resolve(
                VideoCodec::H265,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::Vulkan)
        );
    }

    #[test]
    fn h265_gate_ignores_other_codecs() {
        let caps = Capabilities::none().with_software();
        assert_eq!(
            resolve(
                VideoCodec::H264,
                TierPreference::Auto,
                &caps,
                Platform::Linux
            ),
            Ok(Tier::Software)
        );
    }

    #[test]
    fn tier_error_accessors_report_fields() {
        let err = TierError::new(VideoCodec::H264, TierPreference::Force(Tier::VaApi));
        assert_eq!(err.codec(), VideoCodec::H264);
        assert_eq!(err.preference(), TierPreference::Force(Tier::VaApi));
        let err = TierError::new(VideoCodec::H265, TierPreference::Auto);
        assert_eq!(err.codec(), VideoCodec::H265);
        assert_eq!(err.preference(), TierPreference::Auto);
    }

    #[test]
    fn tier_error_display_names_forced_tier() {
        let err = TierError::new(VideoCodec::H264, TierPreference::Force(Tier::VaApi));
        assert_eq!(format!("{err}"), "tier VaApi unavailable for codec H264");
    }

    #[test]
    fn tier_error_display_auto() {
        let err = TierError::new(VideoCodec::H265, TierPreference::Auto);
        assert_eq!(format!("{err}"), "no tier available for codec H265");
    }
}
