//! Transcode route vocabulary: GPU plan versus loud CPU fallback.

use std::fmt;

use crate::common::PictureSize;
use crate::frame::PixelFormat;
use crate::packet::Packet;

/// CPU fallback cause per failed GPU hop.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FallbackCause {
    /// GPU transcode without a share mechanism.
    ShareUnsupported,
    /// Decode texture without a share handle.
    NoSharedExport,
    /// Encoder without GPU texture import.
    EncoderCpuOnly,
    /// Decode and encode devices differ.
    DeviceMismatch,
    /// GPU scale missing for the size change.
    ScaleNeeded {
        /// Decoded picture size.
        from: PictureSize,
        /// Encode picture size.
        to: PictureSize,
    },
    /// GPU convert missing for the format change.
    ConvertNeeded {
        /// Decoded pixel layout.
        from: PixelFormat,
        /// Encode pixel layout.
        to: PixelFormat,
    },
}

impl fmt::Display for FallbackCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShareUnsupported => write!(f, "gpu transcode without share mechanism"),
            Self::NoSharedExport => write!(f, "decode texture without share handle"),
            Self::EncoderCpuOnly => write!(f, "encoder without gpu import"),
            Self::DeviceMismatch => write!(f, "decode and encode devices differ"),
            Self::ScaleNeeded { from, to } => write!(
                f,
                "gpu scale {}x{} to {}x{} unsupported",
                from.width(),
                from.height(),
                to.width(),
                to.height()
            ),
            Self::ConvertNeeded { from, to } => {
                write!(f, "gpu convert {from:?} to {to:?} unsupported")
            }
        }
    }
}

impl std::error::Error for FallbackCause {}

/// Transcode execution route.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Route {
    /// Device-resident textures, pixels never mapped.
    Gpu,
    /// Proven CPU copy path.
    Cpu,
}

/// GPU share mechanism behind transcode hops.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShareMechanism {
    /// Windows NT handle share.
    NtHandle,
    /// Linux DMA-BUF share.
    DmaBuf,
}

/// Optional format convert between decode and encode layouts.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Convert {
    /// Target layout, absent for passthrough.
    target: Option<PixelFormat>,
}

impl Convert {
    /// Passthrough convert without a target layout.
    #[must_use]
    pub const fn passthrough() -> Self {
        Self { target: None }
    }

    /// Convert to `target`.
    #[must_use]
    pub const fn to(target: PixelFormat) -> Self {
        Self {
            target: Some(target),
        }
    }

    /// Target layout, absent for passthrough.
    #[must_use]
    pub fn target(&self) -> Option<PixelFormat> {
        self.target
    }

    /// Output layout for `from`.
    #[must_use]
    pub fn output(&self, from: PixelFormat) -> PixelFormat {
        self.target.unwrap_or(from)
    }

    /// Convert miss for `from` when the target needs GPU convert.
    #[must_use]
    pub fn miss(&self, from: PixelFormat) -> Option<FallbackCause> {
        let to = self.output(from);
        (from != to).then_some(FallbackCause::ConvertNeeded { from, to })
    }
}

/// GPU transcode capabilities per hop.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuCaps {
    /// Share mechanism behind the hops, absent without GPU interop.
    share: Option<ShareMechanism>,
    /// Decode texture export behind the hop.
    decode_export: bool,
    /// Encoder texture import behind the hop.
    encode_import: bool,
}

impl GpuCaps {
    /// Capabilities behind transcode hops.
    ///
    /// # Arguments
    ///
    /// * `share` - share mechanism behind the hops, absent without GPU interop.
    /// * `decode_export` - decode texture export behind the hop.
    /// * `encode_import` - encoder texture import behind the hop.
    #[must_use]
    pub const fn new(
        share: Option<ShareMechanism>,
        decode_export: bool,
        encode_import: bool,
    ) -> Self {
        Self {
            share,
            decode_export,
            encode_import,
        }
    }

    /// Share mechanism behind the hops, absent without GPU interop.
    #[must_use]
    pub const fn share(&self) -> Option<ShareMechanism> {
        self.share
    }

    /// Decode texture export behind the hop.
    #[must_use]
    pub const fn decode_export(&self) -> bool {
        self.decode_export
    }

    /// Encoder texture import behind the hop.
    #[must_use]
    pub const fn encode_import(&self) -> bool {
        self.encode_import
    }

    /// True when every hop stays on the GPU.
    #[must_use]
    pub const fn ready(&self) -> bool {
        self.share.is_some() && self.decode_export && self.encode_import
    }
}

/// Transcode result: packets plus the executed plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// Coded packets.
    packets: Vec<Packet>,
    /// Executed plan.
    plan: Plan,
}

impl Outcome {
    /// Outcome over `packets` executed under `plan`.
    #[must_use]
    pub fn new(packets: Vec<Packet>, plan: Plan) -> Self {
        Self { packets, plan }
    }

    /// Coded packets.
    #[must_use]
    pub fn packets(&self) -> &[Packet] {
        &self.packets
    }

    /// Executed plan.
    #[must_use]
    pub fn plan(&self) -> Plan {
        self.plan
    }
}

/// Transcode plan: route plus fallback cause on the CPU route.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Plan {
    route: Route,
    cause: Option<FallbackCause>,
}

impl Plan {
    /// GPU plan without a fallback cause.
    #[must_use]
    pub const fn gpu() -> Self {
        Self {
            route: Route::Gpu,
            cause: None,
        }
    }

    /// CPU plan carrying `cause`.
    #[must_use]
    pub const fn cpu(cause: FallbackCause) -> Self {
        Self {
            route: Route::Cpu,
            cause: Some(cause),
        }
    }

    /// GPU plan when every hop of `caps` holds, else the CPU plan naming the first miss.
    #[must_use]
    pub const fn for_caps(caps: &GpuCaps) -> Self {
        Self::for_hops(caps.share(), caps.decode_export(), caps.encode_import())
    }

    /// GPU plan when every hop holds, else the CPU plan naming the first miss.
    #[must_use]
    pub const fn for_hops(
        share: Option<ShareMechanism>,
        decode_export: bool,
        encode_import: bool,
    ) -> Self {
        if share.is_none() {
            Self::cpu(FallbackCause::ShareUnsupported)
        } else if !decode_export {
            Self::cpu(FallbackCause::NoSharedExport)
        } else if !encode_import {
            Self::cpu(FallbackCause::EncoderCpuOnly)
        } else {
            Self::gpu()
        }
    }

    /// GPU plan when `decode` and `encode` name one device, else the CPU plan naming the mismatch.
    ///
    /// # Arguments
    ///
    /// * `decode` - decode device id.
    /// * `encode` - encode device id.
    #[must_use]
    pub const fn for_devices(decode: u64, encode: u64) -> Self {
        if decode == encode {
            Self::gpu()
        } else {
            Self::cpu(FallbackCause::DeviceMismatch)
        }
    }

    /// Execution route.
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }

    /// Fallback cause, present on the CPU route only.
    #[must_use]
    pub const fn cause(&self) -> Option<FallbackCause> {
        self.cause
    }
}

/// Optional scale target between decode and encode extents.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Scale {
    /// Target extent, absent for passthrough.
    target: Option<PictureSize>,
}

impl Scale {
    /// Passthrough scale without a target extent.
    #[must_use]
    pub const fn passthrough() -> Self {
        Self { target: None }
    }

    /// Scale to `target`.
    #[must_use]
    pub const fn to(target: PictureSize) -> Self {
        Self {
            target: Some(target),
        }
    }

    /// Target extent, absent for passthrough.
    #[must_use]
    pub fn target(&self) -> Option<PictureSize> {
        self.target
    }

    /// Output extent for `from`.
    #[must_use]
    pub fn output(&self, from: PictureSize) -> PictureSize {
        self.target.unwrap_or(from)
    }

    /// Scale miss for `from` when the target needs GPU scale.
    #[must_use]
    pub fn miss(&self, from: PictureSize) -> Option<FallbackCause> {
        let to = self.output(from);
        (from != to).then_some(FallbackCause::ScaleNeeded { from, to })
    }
}

#[cfg(test)]
mod tests {
    use super::{Convert, FallbackCause, GpuCaps, Outcome, Plan, Route, Scale, ShareMechanism};
    use crate::common::PictureSize;
    use crate::frame::PixelFormat;

    #[test]
    fn ready_needs_every_hop() {
        let share = Some(ShareMechanism::NtHandle);
        assert!(GpuCaps::new(share, true, true).ready());
        assert!(!GpuCaps::new(None, true, true).ready());
        assert!(!GpuCaps::new(share, false, true).ready());
        assert!(!GpuCaps::new(share, true, false).ready());
    }

    #[test]
    fn ready_accepts_either_share_mechanism() {
        for share in [ShareMechanism::NtHandle, ShareMechanism::DmaBuf] {
            assert!(GpuCaps::new(Some(share), true, true).ready());
        }
    }

    #[test]
    fn for_caps_matches_for_hops() {
        let caps = GpuCaps::new(Some(ShareMechanism::NtHandle), true, true);
        assert_eq!(Plan::for_caps(&caps), Plan::gpu());
        let caps = GpuCaps::new(Some(ShareMechanism::DmaBuf), false, true);
        assert_eq!(
            Plan::for_caps(&caps).cause(),
            Some(FallbackCause::NoSharedExport)
        );
    }

    #[test]
    fn gpu_plan_carries_no_cause() {
        let plan = Plan::gpu();
        assert_eq!(plan.route(), Route::Gpu);
        assert_eq!(plan.cause(), None);
    }

    #[test]
    fn cpu_plan_carries_cause() {
        let plan = Plan::cpu(FallbackCause::EncoderCpuOnly);
        assert_eq!(plan.route(), Route::Cpu);
        assert_eq!(plan.cause(), Some(FallbackCause::EncoderCpuOnly));
    }

    #[test]
    fn cpu_plan_carries_every_cause() {
        let size = PictureSize::new(320, 240).expect("valid size");
        let causes = [
            FallbackCause::ShareUnsupported,
            FallbackCause::NoSharedExport,
            FallbackCause::EncoderCpuOnly,
            FallbackCause::DeviceMismatch,
            FallbackCause::ScaleNeeded {
                from: size,
                to: size,
            },
            FallbackCause::ConvertNeeded {
                from: PixelFormat::Nv12,
                to: PixelFormat::Rgba8,
            },
        ];
        for cause in causes {
            let plan = Plan::cpu(cause);
            assert_eq!(plan.route(), Route::Cpu);
            assert_eq!(plan.cause(), Some(cause));
        }
    }

    #[test]
    fn for_hops_names_first_miss() {
        let share = Some(ShareMechanism::NtHandle);
        assert_eq!(Plan::for_hops(share, true, true), Plan::gpu());
        assert_eq!(
            Plan::for_hops(None, true, true).cause(),
            Some(FallbackCause::ShareUnsupported)
        );
        assert_eq!(
            Plan::for_hops(share, false, true).cause(),
            Some(FallbackCause::NoSharedExport)
        );
        assert_eq!(
            Plan::for_hops(share, true, false).cause(),
            Some(FallbackCause::EncoderCpuOnly)
        );
    }

    #[test]
    fn for_devices_names_mismatch() {
        assert_eq!(Plan::for_devices(7, 7), Plan::gpu());
        assert_eq!(
            Plan::for_devices(7, 9).cause(),
            Some(FallbackCause::DeviceMismatch)
        );
    }

    #[test]
    fn causes_name_hop() {
        assert_eq!(
            FallbackCause::ShareUnsupported.to_string(),
            "gpu transcode without share mechanism"
        );
        assert_eq!(
            FallbackCause::NoSharedExport.to_string(),
            "decode texture without share handle"
        );
        assert_eq!(
            FallbackCause::EncoderCpuOnly.to_string(),
            "encoder without gpu import"
        );
        assert_eq!(
            FallbackCause::DeviceMismatch.to_string(),
            "decode and encode devices differ"
        );
    }

    #[test]
    fn scale_cause_names_sizes() {
        let from = PictureSize::new(1920, 1080).expect("valid size");
        let to = PictureSize::new(1280, 720).expect("valid size");
        let cause = FallbackCause::ScaleNeeded { from, to };
        assert_eq!(
            cause.to_string(),
            "gpu scale 1920x1080 to 1280x720 unsupported"
        );
    }

    #[test]
    fn convert_cause_names_formats() {
        let cause = FallbackCause::ConvertNeeded {
            from: PixelFormat::Nv12,
            to: PixelFormat::Rgba8,
        };
        assert_eq!(cause.to_string(), "gpu convert Nv12 to Rgba8 unsupported");
    }

    #[test]
    fn outcome_reports_packets_and_plan() {
        let outcome = Outcome::new(Vec::new(), Plan::gpu());
        assert!(outcome.packets().is_empty());
        assert_eq!(outcome.plan(), Plan::gpu());
        assert_eq!(outcome.plan().route(), Route::Gpu);
    }

    #[test]
    fn outcome_reports_cpu_plan() {
        let plan = Plan::cpu(FallbackCause::DeviceMismatch);
        let outcome = Outcome::new(Vec::new(), plan);
        assert!(outcome.packets().is_empty());
        assert_eq!(outcome.plan().route(), Route::Cpu);
        assert_eq!(outcome.plan().cause(), Some(FallbackCause::DeviceMismatch));
    }

    #[test]
    fn scale_passes_matching_extent_through() {
        let size = PictureSize::new(320, 240).expect("valid size");
        assert_eq!(Scale::passthrough().output(size), size);
        assert_eq!(Scale::to(size).output(size), size);
        assert_eq!(Scale::passthrough().target(), None);
        assert_eq!(Scale::to(size).target(), Some(size));
    }

    #[test]
    fn scale_miss_names_size_change() {
        let from = PictureSize::new(1920, 1080).expect("valid size");
        let to = PictureSize::new(1280, 720).expect("valid size");
        assert_eq!(Scale::passthrough().miss(from), None);
        assert_eq!(
            Scale::to(to).miss(from),
            Some(FallbackCause::ScaleNeeded { from, to })
        );
    }

    #[test]
    fn convert_passes_matching_layout_through() {
        assert_eq!(
            Convert::passthrough().output(PixelFormat::Nv12),
            PixelFormat::Nv12
        );
        assert_eq!(
            Convert::to(PixelFormat::Nv12).output(PixelFormat::Nv12),
            PixelFormat::Nv12
        );
        assert_eq!(Convert::passthrough().target(), None);
        assert_eq!(
            Convert::to(PixelFormat::P010).target(),
            Some(PixelFormat::P010)
        );
    }

    #[test]
    fn convert_miss_names_layout_change() {
        assert_eq!(Convert::passthrough().miss(PixelFormat::Nv12), None);
        assert_eq!(
            Convert::to(PixelFormat::Rgba8).miss(PixelFormat::Nv12),
            Some(FallbackCause::ConvertNeeded {
                from: PixelFormat::Nv12,
                to: PixelFormat::Rgba8,
            })
        );
    }
}
