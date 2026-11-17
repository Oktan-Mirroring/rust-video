//! Tier device behind the caps `Device` trait.

use crate::sdk::ApiLevel;

use rust_video_caps::traits::Device;

/// Probed `MediaCodec` tier.
///
/// Native mapping, in tier idiom: Android exposes no decoder-device object,
/// so the device identity is the [`ApiLevel`], held by value because it is
/// `Copy`. The queue/context object is the live `MediaCodec` behind the
/// decoder's `codec()` handle, and the image object is the `CpuFrame` behind
/// each surface's `frame()` handle. Threading and lifetime rules live on the
/// decoder and the surface; this probe only names the tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaCodecDevice {
    /// Device level on Android, absent elsewhere.
    level: Option<ApiLevel>,
}

impl MediaCodecDevice {
    /// Tier name.
    const NAME: &'static str = "mediacodec";

    /// Device probed on this machine.
    #[cfg(target_os = "android")]
    #[must_use]
    pub fn probe() -> Self {
        Self {
            level: Some(ApiLevel::current()),
        }
    }

    /// True on a supported device floor.
    #[must_use]
    pub fn available(&self) -> bool {
        self.level.is_some_and(ApiLevel::meets_min)
    }

    /// Device level, if probed.
    #[must_use]
    pub fn api_level(&self) -> Option<ApiLevel> {
        self.level
    }
}

impl Device for MediaCodecDevice {
    /// Tier name.
    fn name(&self) -> &str {
        Self::NAME
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "android")]
    use super::MediaCodecDevice;

    #[cfg(target_os = "android")]
    use rust_video_caps::traits::Device;

    #[cfg(target_os = "android")]
    #[test]
    fn probe_names_tier() {
        assert_eq!(MediaCodecDevice::probe().name(), "mediacodec");
    }
}
