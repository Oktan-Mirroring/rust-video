//! Hardware tier capability queries.
//!
//! No vendor publishes fixed caps, so this module holds queried values,
//! never constants: Vulkan maxCodedExtent, VA-API driver limits, D3D11
//! per-adapter profiles, `MediaCodec` supported ranges.

use crate::common::Violation;

/// Encoder rate control mode (Vulkan Video).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RateControl {
    /// Driver default.
    Default,
    /// Rate control disabled.
    Disabled,
    /// Constant bitrate.
    Cbr,
    /// Variable bitrate.
    Vbr,
}

/// Runtime-queried hardware limits for one codec session.
pub struct HwCaps {
    /// Max picture samples the device reports.
    pub max_pic_samples: u64,
    /// Max axis samples the device reports.
    pub max_dim: u32,
    /// Max bitrate bps the device reports.
    pub max_bitrate_bps: u64,
}

impl HwCaps {
    /// Check a stream against the queried limits.
    ///
    /// # Errors
    ///
    /// Returns the first `Violation` found.
    pub fn supports(&self, samples: u64, axis: u32, bitrate_bps: u32) -> Result<(), Violation> {
        if samples > self.max_pic_samples {
            return Err(Violation::PictureTooLarge {
                actual: samples,
                max: self.max_pic_samples,
            });
        }
        if axis > self.max_dim {
            return Err(Violation::SizeRuleExceeded {
                actual: axis,
                max: u64::from(self.max_dim),
            });
        }
        if u64::from(bitrate_bps) > self.max_bitrate_bps {
            return Err(Violation::BitrateExceeded {
                actual: bitrate_bps,
                max: self.max_bitrate_bps,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::common::Violation;

    use super::HwCaps;

    fn caps_1080p() -> HwCaps {
        HwCaps {
            max_pic_samples: 2_359_296,
            max_dim: 4096,
            max_bitrate_bps: 20_000_000,
        }
    }

    #[test]
    fn fits_inside_limits() {
        caps_1080p()
            .supports(2_073_600, 1920, 10_000_000)
            .expect("fits");
    }

    #[test]
    fn oversize_fails() {
        assert!(matches!(
            caps_1080p().supports(8_912_896, 1920, 10_000_000),
            Err(Violation::PictureTooLarge { .. })
        ));
        assert!(matches!(
            caps_1080p().supports(2_073_600, 8192, 10_000_000),
            Err(Violation::SizeRuleExceeded { .. })
        ));
        assert!(matches!(
            caps_1080p().supports(2_073_600, 1920, 30_000_000),
            Err(Violation::BitrateExceeded { .. })
        ));
    }
}
