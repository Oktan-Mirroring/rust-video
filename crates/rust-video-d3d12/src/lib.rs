//! Direct3D 12 video tier: hardware decode and encode behind the caps traits.
//!
//! Single-picture sessions over video command lists with loud fallbacks.

#[cfg(target_os = "windows")]
mod bits;
#[cfg(target_os = "windows")]
mod decode;
mod decoder;
mod device;
#[cfg(target_os = "windows")]
mod dpb;
#[cfg(target_os = "windows")]
mod dxva;
#[cfg(target_os = "windows")]
mod encode;
mod encoder;
mod error;
mod guid;
#[cfg(target_os = "windows")]
mod h264;
#[cfg(target_os = "windows")]
mod hevc;
#[cfg(target_os = "windows")]
mod scale;
#[cfg(target_os = "windows")]
mod submit;
#[cfg(target_os = "windows")]
mod texture;
#[cfg(target_os = "windows")]
mod transcode;

pub use decoder::{Av1Decoder, D3d12Frame, Mpeg2Decoder, Vc1Decoder, Vp9Decoder};
#[cfg(target_os = "windows")]
pub use decoder::{H264Decoder, HevcDecoder};
pub use device::D3d12Device;
#[cfg(target_os = "windows")]
pub use encoder::D3d12Encoder;
pub use error::{D3d12Error, SkipReason};
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 12 video tier.
pub struct D3d12;
