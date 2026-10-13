//! Safe Rust interface over realtime video encode and decode.
//!
//! Codec backends and platform tiers behind one facade.

pub mod error;
pub mod frame;
pub mod packet;
pub mod tier;
pub mod traits;

pub use error::Error;
pub use frame::{CpuFrame, CpuReadback, FramePool, GpuExport, GpuFrame, PixelFormat, Plane};
pub use packet::Packet;
pub use tier::{
    auto_order, host_platform, resolve, Capabilities, Platform, SoftwareH265, Tier, TierError,
    TierPreference, VideoCodec,
};
pub use traits::{Decoder, Device, Encoder, Lease, OverBudget, SharedDevice};

/// Shared byte buffer.
pub use bytes::Bytes;

#[cfg(feature = "codec-av1")]
/// AV1 codec backend.
pub use rust_video_av1 as av1;
#[cfg(feature = "codec-av2")]
/// AV2 codec backend.
pub use rust_video_av2 as av2;
#[cfg(feature = "tier-d3d11")]
/// Direct3D 11 platform tier.
pub use rust_video_d3d11 as d3d11;
#[cfg(feature = "codec-h264")]
/// H.264 codec backend.
pub use rust_video_h264 as h264;
#[cfg(feature = "codec-h265")]
/// H.265 codec backend.
pub use rust_video_h265 as h265;
#[cfg(feature = "codec-vp9")]
/// VP9 codec backend.
pub use rust_video_vp9 as vp9;

#[cfg(test)]
mod tests {
    #[test]
    fn bytes_reexport_names_shared_buffer() {
        let bytes = crate::Bytes::from_static(&[1, 2, 3]);
        assert_eq!(&bytes[..], &[1, 2, 3]);
    }

    #[cfg(feature = "codec-av1")]
    #[test]
    fn codec_av1_reexport_resolves() {
        assert!(core::any::type_name::<crate::av1::Av1>().contains("Av1"));
    }

    #[cfg(feature = "codec-av2")]
    #[test]
    fn codec_av2_reexport_resolves() {
        assert!(core::any::type_name::<crate::av2::Av2>().contains("Av2"));
    }

    #[cfg(feature = "tier-d3d11")]
    #[test]
    fn tier_d3d11_reexport_resolves() {
        assert!(core::any::type_name::<crate::d3d11::D3d11>().contains("D3d11"));
    }

    #[cfg(feature = "codec-h264")]
    #[test]
    fn codec_h264_reexport_resolves() {
        assert!(core::any::type_name::<crate::h264::H264>().contains("H264"));
    }

    #[cfg(feature = "codec-h265")]
    #[test]
    fn codec_h265_reexport_resolves() {
        assert!(core::any::type_name::<crate::h265::H265>().contains("H265"));
    }

    #[cfg(feature = "codec-vp9")]
    #[test]
    fn codec_vp9_reexport_resolves() {
        assert!(core::any::type_name::<crate::vp9::Vp9>().contains("Vp9"));
    }
}
