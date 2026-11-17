//! Typed VA-API tier failures and skip reasons.
//!
//! Portable: the driver status mapping lives in the Linux-only
//! `sys` module; this module holds only the types.

/// Machine declined by the VA-API tier, with the cause.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SkipReason {
    /// No DRM render node under `/dev/dri`.
    #[error("no drm render node")]
    NoRenderNode,
    /// No node initializes a VA-API display.
    #[error("no initializable va-api display")]
    NoDisplay,
    /// H.264 VLD missing on every display.
    #[error("no h264 vld entrypoint")]
    NoH264Vld,
    /// NV12 render target missing on every H.264 config.
    #[error("no nv12 render target")]
    NoNv12Target,
}

/// Hard VA-API tier failure.
#[derive(Debug, thiserror::Error)]
pub enum VaApiError {
    /// Machine declined; the tier cannot run here.
    #[error("va-api skipped: {0}")]
    Skipped(#[from] SkipReason),
    /// VA-API call failure, with the driver status text.
    #[error("va-api call failed with status {status} ({message})")]
    Call {
        /// Raw driver status.
        status: i32,
        /// Driver status text.
        message: String,
    },
    /// Bitstream the tier refuses: interlaced, FMO, odd chroma, or a gap it cannot bridge.
    #[error("unsupported stream: {0}")]
    Unsupported(String),
    /// Bitstream parse failure.
    #[error("parse failed: {0}")]
    Parse(String),
    /// Reference list names a picture the DPB cannot supply.
    #[error("broken reference list: {0}")]
    BrokenReference(String),
    /// DPB has no free slot for the next picture.
    #[error("dpb full")]
    DpbFull,
    /// Decoder has no free surface slot for the next picture.
    ///
    /// A live held surface pins its slot, so callers holding more
    /// frames than the pool holds exhaust the pool loudly here
    /// instead of corrupting a held picture.
    #[error("no free surface slot")]
    NoFreeSlot,
    /// Frame over the VRAM budget.
    #[error("frame over budget: {0}")]
    OverBudget(#[from] rust_video_caps::traits::OverBudget),
    /// Driver returned a zero extent.
    #[error("driver returned zero extent {width}x{height}")]
    BadExtent {
        /// Returned width in luma samples.
        width: u32,
        /// Returned height in luma samples.
        height: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::{SkipReason, VaApiError};

    #[test]
    fn skip_reasons_name_cause() {
        assert_eq!(SkipReason::NoRenderNode.to_string(), "no drm render node");
        assert_eq!(
            SkipReason::NoDisplay.to_string(),
            "no initializable va-api display"
        );
        assert_eq!(SkipReason::NoH264Vld.to_string(), "no h264 vld entrypoint");
        assert_eq!(
            SkipReason::NoNv12Target.to_string(),
            "no nv12 render target"
        );
    }

    #[test]
    fn skipped_wraps_reason() {
        let error = VaApiError::from(SkipReason::NoRenderNode);
        assert_eq!(error.to_string(), "va-api skipped: no drm render node");
    }

    #[test]
    fn call_names_status() {
        let error = VaApiError::Call {
            status: 7,
            message: "invalid buffer".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "va-api call failed with status 7 (invalid buffer)"
        );
    }

    #[test]
    fn unsupported_names_stream() {
        assert_eq!(
            VaApiError::Unsupported("fmo".to_owned()).to_string(),
            "unsupported stream: fmo"
        );
    }

    #[test]
    fn dpb_full_names_exhaustion() {
        assert_eq!(VaApiError::DpbFull.to_string(), "dpb full");
    }

    #[test]
    fn no_free_slot_names_exhaustion() {
        assert_eq!(VaApiError::NoFreeSlot.to_string(), "no free surface slot");
    }

    #[test]
    fn bad_extent_names_size() {
        let error = VaApiError::BadExtent {
            width: 0,
            height: 1080,
        };
        assert_eq!(error.to_string(), "driver returned zero extent 0x1080");
    }
}
