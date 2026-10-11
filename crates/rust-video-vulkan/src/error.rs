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
    /// No H.265 profile reports decode capabilities.
    #[error("no H.265 decode capabilities")]
    NoH265Caps,
    /// No queue family with video-encode capability.
    #[error("no video-encode queue family")]
    NoEncodeQueue,
    /// No H.264 profile reports encode capabilities.
    #[error("no H.264 encode capabilities")]
    NoH264EncodeCaps,
}

/// Annex-B bitstream the parser rejects.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum ParseError {
    /// NAL ends before the syntax element completes.
    #[error("truncated NAL in {syntax}")]
    Truncated {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Exp-Golomb code exceeds 32 bits.
    #[error("exp-golomb overflow in {syntax}")]
    Overflow {
        /// Syntax element under parse.
        syntax: &'static str,
    },
    /// Legal syntax the tier does not run.
    #[error("unsupported {what}")]
    Unsupported {
        /// Construct the tier declines.
        what: &'static str,
    },
    /// Slice cites a parameter set never seen.
    #[error("slice cites unknown parameter set {id}")]
    MissingSet {
        /// Cited set id.
        id: u32,
    },
    /// Syntax value outside its field width.
    #[error("value out of range in {syntax}")]
    OutOfRange {
        /// Syntax element under parse.
        syntax: &'static str,
    },
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
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(#[from] ParseError),
    /// Frame over the VRAM budget.
    #[error("frame over budget: {0}")]
    OverBudget(#[from] rust_video::OverBudget),
    /// Session returned a zero extent.
    #[error("session returned zero extent {width}x{height}")]
    BadExtent {
        /// Returned width in luma samples.
        width: u32,
        /// Returned height in luma samples.
        height: u32,
    },
    /// Negotiation declined the machine; no backend to run on.
    #[error("vulkan backend absent: {0}")]
    BackendAbsent(#[from] SkipReason),
    /// Stream level above the session cap.
    #[error("stream level {stream} above session cap {max}")]
    UnsupportedLevel {
        /// Level the stream carries.
        stream: u8,
        /// Highest level the session runs.
        max: u8,
    },
    /// No 8-bit 4:2:0 video picture format on the adapter.
    #[error("no yuv420 video picture format")]
    NoVideoFormat,
    /// Fence wait elapsed without signaling.
    #[error("fence wait timed out")]
    Timeout,
    /// Stream demand above the adapter caps.
    #[error("stream {what} above adapter caps")]
    OverCaps {
        /// Capped demand the stream exceeds.
        what: &'static str,
    },
    /// Picture arrived before any parameter set built a session.
    #[error("picture before session")]
    NoSession,
    /// Encoder input outside the NV12 contract.
    #[error("encoder wants NV12 input, got {format:?}")]
    UnsupportedInput {
        /// Pixel layout the caller supplied.
        format: rust_video::PixelFormat,
    },
}

#[cfg(test)]
mod tests {
    use super::{ParseError, SkipReason, VulkanError};

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
        assert_eq!(
            SkipReason::NoH265Caps.to_string(),
            "no H.265 decode capabilities"
        );
        assert_eq!(
            SkipReason::NoEncodeQueue.to_string(),
            "no video-encode queue family"
        );
        assert_eq!(
            SkipReason::NoH264EncodeCaps.to_string(),
            "no H.264 encode capabilities"
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
        assert_eq!(error.to_string(), "session returned zero extent 0x1080");
    }

    #[test]
    fn backend_absent_carries_skip_reason() {
        let error = VulkanError::from(SkipReason::NoEncodeQueue);
        assert!(matches!(
            error,
            VulkanError::BackendAbsent(SkipReason::NoEncodeQueue)
        ));
        assert_eq!(
            error.to_string(),
            "vulkan backend absent: no video-encode queue family"
        );
    }

    #[test]
    fn parse_error_names_syntax() {
        let error = VulkanError::from(ParseError::Truncated { syntax: "sps" });
        assert_eq!(error.to_string(), "parse failed: truncated NAL in sps");
        let error = VulkanError::from(ParseError::Overflow { syntax: "pps" });
        assert_eq!(
            error.to_string(),
            "parse failed: exp-golomb overflow in pps"
        );
        let error = VulkanError::from(ParseError::Unsupported {
            what: "field pictures",
        });
        assert_eq!(
            error.to_string(),
            "parse failed: unsupported field pictures"
        );
        let error = VulkanError::from(ParseError::MissingSet { id: 3 });
        assert_eq!(
            error.to_string(),
            "parse failed: slice cites unknown parameter set 3"
        );
        let error = VulkanError::from(ParseError::OutOfRange { syntax: "sps" });
        assert_eq!(error.to_string(), "parse failed: value out of range in sps");
    }

    #[test]
    fn session_errors_name_cause() {
        let error = VulkanError::UnsupportedLevel {
            stream: 41,
            max: 31,
        };
        assert_eq!(error.to_string(), "stream level 41 above session cap 31");
        assert_eq!(
            VulkanError::NoVideoFormat.to_string(),
            "no yuv420 video picture format"
        );
        assert_eq!(VulkanError::Timeout.to_string(), "fence wait timed out");
        let error = VulkanError::OverCaps { what: "dpb slots" };
        assert_eq!(error.to_string(), "stream dpb slots above adapter caps");
        assert_eq!(VulkanError::NoSession.to_string(), "picture before session");
    }

    #[test]
    fn unsupported_input_names_format() {
        let error = VulkanError::UnsupportedInput {
            format: rust_video::PixelFormat::Rgba8,
        };
        assert_eq!(error.to_string(), "encoder wants NV12 input, got Rgba8");
    }
}
