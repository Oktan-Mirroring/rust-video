//! Direct3D 11 video tier: hardware decode and encode behind the caps traits.
//!
//! Hardware decode pixel parity against the software oracle is unverified.

#[cfg(target_os = "windows")]
mod bits;
mod decoder;
mod device;
#[cfg(target_os = "windows")]
mod dpb;
#[cfg(target_os = "windows")]
mod dxva;
mod encoder;
mod error;
mod guid;
#[cfg(target_os = "windows")]
mod h264;
#[cfg(target_os = "windows")]
mod mf;
#[cfg(target_os = "windows")]
mod scale;
#[cfg(target_os = "windows")]
mod session;
#[cfg(target_os = "windows")]
mod transcode;

#[cfg(target_os = "windows")]
pub use decoder::H264Decoder;
pub use decoder::{Av1Decoder, D3d11Frame, HevcDecoder, Mpeg2Decoder, Vc1Decoder, Vp9Decoder};
pub use device::D3d11Device;
#[cfg(target_os = "windows")]
pub use encoder::D3d11Encoder;
pub use error::{D3d11Error, SkipReason};
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 11 video tier.
pub struct D3d11;
