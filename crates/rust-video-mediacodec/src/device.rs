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
    #[must_use]
    pub fn probe() -> Self {
        Self {
            #[cfg(target_os = "android")]
            level: Some(ApiLevel::current()),
            #[cfg(not(target_os = "android"))]
            level: None,
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
    use super::MediaCodecDevice;

    use rust_video_caps::traits::Device;

    #[test]
    fn probe_names_tier() {
        assert_eq!(MediaCodecDevice::probe().name(), "mediacodec");
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn probe_absent_off_android() {
        let device = MediaCodecDevice::probe();
        assert!(!device.available());
        assert!(device.api_level().is_none());
    }
}
