//! D3D11 device view without hardware: profiles, level, and name.

use crate::guid::{Codec, Profile};

use rust_video_caps::traits::Device;

/// D3D11 device with enumerated decoder profiles.
pub struct D3d11Device {
    /// Matched decoder profiles.
    profiles: Vec<Profile>,
    /// Negotiated feature level.
    level: i32,
    /// Adapter description.
    name: String,
}

impl D3d11Device {
    /// True when the adapter exposes a `codec` profile.
    #[must_use]
    pub fn supports(&self, codec: Codec) -> bool {
        self.profiles.iter().any(|profile| profile.codec() == codec)
    }

    /// Matched decoder profiles on the adapter.
    #[must_use]
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Negotiated feature level.
    #[must_use]
    pub fn feature_level(&self) -> i32 {
        self.level
    }
}

impl Device for D3d11Device {
    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::D3d11Device;
    use crate::guid::{Codec, Profile};

    use rust_video_caps::traits::Device;

    /// Device over `profiles` at `level` named `name`.
    fn device(profiles: Vec<Profile>, level: i32, name: &str) -> D3d11Device {
        D3d11Device {
            profiles,
            level,
            name: name.to_owned(),
        }
    }

    #[test]
    fn supports_matches_profile_codec() {
        let device = device(
            vec![Profile {
                codec: Codec::Hevc,
                name: "hevc-main",
            }],
            0xb100,
            "adapter",
        );
        assert!(device.supports(Codec::Hevc));
        assert!(!device.supports(Codec::H264));
        assert_eq!(device.profiles().len(), 1);
    }

    #[test]
    fn empty_device_supports_nothing() {
        let device = device(Vec::new(), 0xb000, "adapter");
        assert!(!device.supports(Codec::Av1));
        assert!(device.profiles().is_empty());
    }

    #[test]
    fn level_and_name_pass_through() {
        let device = device(Vec::new(), 0xb100, "adapter");
        assert_eq!(device.feature_level(), 0xb100);
        assert_eq!(device.name(), "adapter");
    }
}
