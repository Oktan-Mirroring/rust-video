//! Direct3D 12 video tier: hardware decode and encode behind the caps traits.
//!
//! Single-picture sessions over video command lists with loud fallbacks.

mod bits;
mod decode;
mod decoder;
mod device;
mod dxva;
mod encode;
mod encoder;
mod error;
mod guid;
mod h264;
mod hevc;
mod submit;

pub use decoder::{
    Av1Decoder, D3d12Frame, H264Decoder, HevcDecoder, Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};
pub use device::D3d12Device;
pub use encoder::D3d12Encoder;
pub use error::{D3d12Error, SkipReason};
pub use guid::{Codec, Profile};

/// Direct3D 12 video tier.
pub struct D3d12;
