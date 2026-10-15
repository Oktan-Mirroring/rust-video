//! Tier device behind the caps `Device` trait.

use crate::sdk::ApiLevel;

use rust_video_caps::traits::Device;

/// Probed `MediaCodec` tier.
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
