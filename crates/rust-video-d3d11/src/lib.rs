//! Direct3D 11 video tier: hardware decode and encode behind the caps traits.
//!
//! Hardware decode pixel parity against the software oracle is unverified.

mod bits;
mod decoder;
mod device;
mod dpb;
mod dxva;
mod encoder;
mod error;
mod guid;
mod h264;
mod mf;
mod session;
mod transcode;

pub use decoder::{
    Av1Decoder, D3d11Frame, H264Decoder, HevcDecoder, Mpeg2Decoder, Vc1Decoder, Vp9Decoder,
};
pub use device::D3d11Device;
pub use encoder::D3d11Encoder;
pub use error::{D3d11Error, SkipReason};
pub use guid::{Codec, Profile};
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 11 video tier.
pub struct D3d11;
