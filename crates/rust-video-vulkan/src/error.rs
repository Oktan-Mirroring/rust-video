//! Typed Vulkan tier failures and skip reasons.

/// Machine declined by the Vulkan tier, with the cause.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SkipReason {
    /// Loader library absent.
    #[error("vulkan loader absent")]
    LoaderAbsent,
    /// Loader enumerates no physical device.
    #[error("no physical device")]
    NoPhysicalDevice,
    /// Device API below the 1.3 video floor.
    #[error("api {found:#x} below the 1.3 floor")]
    BelowApiFloor {
        /// Packed API version the device reports.
        found: u32,
    },
    /// Required device extension string missing.
    #[error("extension {name} missing")]
    MissingExtension {
        /// Extension name the ladder looked for.
        name: String,
    },
    /// No queue family with video-decode capability.
    #[error("no video-decode queue family")]
    NoDecodeQueue,
    /// No H.264 profile reports decode capabilities.
    #[error("no H.264 decode capabilities")]
    NoH264Caps,
}

/// Hard Vulkan tier failure.
#[derive(Debug, thiserror::Error)]
pub enum VulkanError {
    /// Vulkan call failure.
    #[error("vulkan call failed: {0}")]
    Vulkan(#[from] ash::vk::Result),
    /// Loader string conversion failure.
    #[error("loader string conversion failed: {0}")]
    StringConversion(#[from] std::ffi::FromBytesUntilNulError),
    /// Decode engine failed to start.
    #[error("decode engine failed to start: {0}")]
    EngineInit(#[from] gpu_video::VulkanInitError),
    /// Decode engine rejected the input.
    #[error("decode failed: {0}")]
    Decode(#[from] gpu_video::DecoderError),
    /// Frame over the VRAM budget.
    #[error("frame over budget: {0}")]
    OverBudget(#[from] rust_video::OverBudget),
    /// Engine returned a zero extent.
    #[error("engine returned zero extent {width}x{height}")]
    BadExtent {
        /// Returned width in luma samples.
        width: u32,
        /// Returned height in luma samples.
        height: u32,
    },
    /// Engine panicked; the decoder is poisoned and unusable.
    #[error("decode engine faulted; decoder poisoned")]
    EngineFault,
}

#[cfg(test)]
mod tests {
    use super::{SkipReason, VulkanError};

    #[test]
    fn skip_reasons_name_cause() {
        assert_eq!(SkipReason::LoaderAbsent.to_string(), "vulkan loader absent");
        assert_eq!(
            SkipReason::NoPhysicalDevice.to_string(),
            "no physical device"
        );
        assert_eq!(
            SkipReason::NoDecodeQueue.to_string(),
            "no video-decode queue family"
        );
        assert_eq!(
            SkipReason::NoH264Caps.to_string(),
            "no H.264 decode capabilities"
        );
    }

    #[test]
    fn skip_reasons_carry_values() {
        let reason = SkipReason::BelowApiFloor { found: 0x40_0000 };
        assert!(reason.to_string().contains("1.3"));
        let reason = SkipReason::MissingExtension {
            name: "VK_KHR_video_queue".to_owned(),
        };
        assert!(reason.to_string().contains("VK_KHR_video_queue"));
    }

    #[test]
    fn bad_extent_names_size() {
        let error = VulkanError::BadExtent {
            width: 0,
            height: 1080,
        };
        assert_eq!(error.to_string(), "engine returned zero extent 0x1080");
    }
}
