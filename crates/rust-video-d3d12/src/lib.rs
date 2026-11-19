//! Direct3D 12 video tier: hardware decode and encode behind the caps traits.
//!
//! Single-picture sessions over video command lists with loud fallbacks.

#[cfg(target_os = "windows")]
mod av1;
#[cfg(target_os = "windows")]
mod decode;
mod decoder;
#[cfg(target_os = "windows")]
mod decoder_os;
mod device;
#[cfg(target_os = "windows")]
mod device_os;
#[cfg(target_os = "windows")]
mod encode;
#[cfg(target_os = "windows")]
mod encoder;
mod error;
mod guid;
#[cfg(target_os = "windows")]
mod h264;
#[cfg(target_os = "windows")]
mod h265;
#[cfg(target_os = "windows")]
mod os;
#[cfg(not(target_os = "windows"))]
mod os_stub;
#[cfg(target_os = "windows")]
mod overlap;
#[cfg(target_os = "windows")]
mod pins;
#[cfg(target_os = "windows")]
mod readback;
#[cfg(target_os = "windows")]
mod scale;
mod seam;
#[cfg(target_os = "windows")]
mod submit;
#[cfg(target_os = "windows")]
mod texture;
#[cfg(target_os = "windows")]
mod transcode;

#[cfg(target_os = "windows")]
pub use av1::decoder::Av1Decoder;
#[cfg(not(target_os = "windows"))]
pub use decoder::Av1Decoder;
pub use decoder::{D3d12Frame, Mpeg2Decoder, Vc1Decoder, Vp9Decoder};
pub use device::D3d12Device;
#[cfg(target_os = "windows")]
pub use encoder::D3d12Encoder;
pub use error::{BrokenReferenceReason, D3d12Error, ParseReason, SkipReason, UnsupportedReason};
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub use h264::decoder::H264Decoder;
#[cfg(target_os = "windows")]
pub use h265::decoder::HevcDecoder;
#[cfg(target_os = "windows")]
pub(crate) use os::DeviceOs;
#[cfg(not(target_os = "windows"))]
pub(crate) use os_stub::DeviceOs;
#[cfg(target_os = "windows")]
pub use overlap::{
    Av1Front, H264Front, HevcFront, OverlapPump, OverlappedAv1Pump, OverlappedH264Pump,
    OverlappedHevcPump,
};
#[cfg(target_os = "windows")]
pub use texture::GpuSurface;
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 12 video tier.
pub struct D3d12;
