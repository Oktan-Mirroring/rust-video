//! Direct3D 12 video tier: hardware decode and encode behind the caps traits.
//!
//! Single-picture sessions over video command lists with loud fallbacks.

mod bits;
mod decode;
mod decoder;
mod device;
mod dpb;
mod dxva;
mod encode;
mod encoder;
mod error;
mod guid;
mod h264;
mod hevc;
#[cfg(target_os = "windows")]
mod scale;
mod submit;
#[cfg(target_os = "windows")]
mod texture;
mod transcode;

pub use decoder::{
    Av1Decoder, D3d12Frame, H264Decoder, HevcDecoder, Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};
pub use device::D3d12Device;
pub use encoder::D3d12Encoder;
pub use error::{D3d12Error, SkipReason};
pub use guid::{Codec, Profile};
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 12 video tier.
pub struct D3d12;
