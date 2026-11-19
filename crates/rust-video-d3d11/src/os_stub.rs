//! Off-target OS state: portable handles carry no platform resources.

/// Device OS state off-target: no resources.
///
/// The device type keeps its shape on every target; only Windows
/// constructs it.
pub(crate) type DeviceOs = ();

#[cfg(test)]
mod tests {
    use crate::device::D3d11Device;
    use crate::guid::{Codec, Profile};

    use rust_video_caps::traits::Device;

    /// Device over `profiles` at `level` named `name`.
    fn device(profiles: Vec<Profile>, level: i32, name: &str) -> D3d11Device {
        D3d11Device {
            profiles,
            level,
            name: name.to_owned(),
            os: (),
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
