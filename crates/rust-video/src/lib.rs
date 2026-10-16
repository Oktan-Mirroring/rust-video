//! Safe Rust interface over realtime video encode and decode.
//!
//! Codec backends and platform tiers behind one facade.

pub mod error;
pub mod tier;

pub use rust_video_caps::{frame, packet, traits, transcode};

pub use error::Error;
pub use frame::{CpuFrame, CpuReadback, FramePool, GpuExport, GpuFrame, PixelFormat, Plane};
pub use packet::Packet;
pub use tier::{
    auto_order, host_platform, resolve, Capabilities, Platform, SoftwareH265, Tier, TierError,
    TierPreference, VideoCodec,
};
pub use traits::{Decoder, Device, Encoder, Lease, OverBudget, SharedDevice};
pub use transcode::{FallbackCause, Outcome, Plan, Route, Scale};

/// Shared byte buffer.
pub use bytes::Bytes;

#[cfg(feature = "av1")]
/// AV1 codec backend.
pub use rust_video_av1 as av1;
#[cfg(feature = "av2")]
/// AV2 codec backend.
pub use rust_video_av2 as av2;
#[cfg(feature = "d3d11")]
/// Direct3D 11 platform tier.
pub use rust_video_d3d11 as d3d11;
#[cfg(feature = "d3d12")]
/// Direct3D 12 platform tier.
pub use rust_video_d3d12 as d3d12;
#[cfg(feature = "h264")]
/// H.264 codec backend.
pub use rust_video_h264 as h264;
#[cfg(feature = "h265")]
/// H.265 codec backend.
pub use rust_video_h265 as h265;
#[cfg(feature = "mediacodec")]
/// Android `MediaCodec` platform tier.
pub use rust_video_mediacodec as mediacodec;
#[cfg(feature = "vaapi")]
/// VA-API platform tier.
pub use rust_video_vaapi as vaapi;
#[cfg(feature = "vp9")]
/// VP9 codec backend.
pub use rust_video_vp9 as vp9;
#[cfg(feature = "vulkan")]
/// Vulkan platform tier.
pub use rust_video_vulkan as vulkan;

#[cfg(test)]
mod tests {
    #[test]
    fn bytes_reexport_names_shared_buffer() {
        let bytes = crate::Bytes::from_static(&[1, 2, 3]);
        assert_eq!(&bytes[..], &[1, 2, 3]);
    }

    #[cfg(feature = "av1")]
    #[test]
    fn av1_reexport_resolves() {
        assert!(core::any::type_name::<crate::av1::Av1>().contains("Av1"));
    }

    #[cfg(feature = "av2")]
    #[test]
    fn av2_reexport_resolves() {
        assert!(core::any::type_name::<crate::av2::Av2>().contains("Av2"));
    }

    #[cfg(feature = "d3d11")]
    #[test]
    fn d3d11_reexport_resolves() {
        assert!(core::any::type_name::<crate::d3d11::D3d11>().contains("D3d11"));
    }

    #[cfg(feature = "d3d12")]
    #[test]
    fn d3d12_reexport_resolves() {
        assert!(core::any::type_name::<crate::d3d12::D3d12>().contains("D3d12"));
    }

    #[cfg(feature = "h264")]
    #[test]
    fn h264_reexport_resolves() {
        assert!(core::any::type_name::<crate::h264::H264>().contains("H264"));
    }

    #[cfg(feature = "h265")]
    #[test]
    fn h265_reexport_resolves() {
        assert!(core::any::type_name::<crate::h265::H265>().contains("H265"));
    }

    #[cfg(feature = "mediacodec")]
    #[test]
    fn mediacodec_reexport_resolves() {
        assert!(
            core::any::type_name::<crate::mediacodec::MediaCodecDevice>()
                .contains("MediaCodecDevice")
        );
    }

    #[cfg(vaapi_available)]
    #[test]
    fn vaapi_reexport_resolves() {
        assert!(core::any::type_name::<crate::vaapi::VaApiDevice>().contains("VaApiDevice"));
    }

    #[cfg(feature = "vp9")]
    #[test]
    fn vp9_reexport_resolves() {
        assert!(core::any::type_name::<crate::vp9::Vp9>().contains("Vp9"));
    }

    #[cfg(feature = "vulkan")]
    #[test]
    fn vulkan_reexport_resolves() {
        assert!(core::any::type_name::<crate::vulkan::VulkanDevice>().contains("VulkanDevice"));
    }
}
