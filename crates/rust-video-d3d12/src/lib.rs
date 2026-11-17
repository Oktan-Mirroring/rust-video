//! Direct3D 12 video tier: hardware decode and encode behind the caps traits.
//!
//! Single-picture sessions over video command lists with loud fallbacks.

#[cfg(target_os = "windows")]
mod bits;
#[cfg(target_os = "windows")]
mod decode;
mod decoder;
#[cfg(target_os = "windows")]
mod decoder_os;
#[cfg(target_os = "windows")]
mod delivery;
mod device;
#[cfg(target_os = "windows")]
mod device_os;
#[cfg(target_os = "windows")]
mod dpb;
#[cfg(target_os = "windows")]
mod dxva;
#[cfg(target_os = "windows")]
mod encode;
#[cfg(target_os = "windows")]
mod encoder;
mod error;
mod guid;
#[cfg(target_os = "windows")]
mod h264;
#[cfg(target_os = "windows")]
mod hevc;
#[cfg(target_os = "windows")]
mod hevc_dpb;
#[cfg(target_os = "windows")]
mod os;
#[cfg(not(target_os = "windows"))]
mod os_stub;
#[cfg(target_os = "windows")]
mod pins;
#[cfg(target_os = "windows")]
mod scale;
mod seam;
#[cfg(target_os = "windows")]
mod submit;
#[cfg(target_os = "windows")]
mod texture;
#[cfg(target_os = "windows")]
mod transcode;

pub use decoder::{Av1Decoder, D3d12Frame, Mpeg2Decoder, Vc1Decoder, Vp9Decoder};
#[cfg(target_os = "windows")]
pub use decoder_os::{H264Decoder, HevcDecoder, OverlappedH264Pump, PhaseTiming};
#[cfg(target_os = "windows")]
pub use delivery::DecoderPump;
pub use device::D3d12Device;
#[cfg(target_os = "windows")]
pub use encoder::D3d12Encoder;
pub use error::{BrokenReferenceReason, D3d12Error, ParseReason, SkipReason, UnsupportedReason};
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub(crate) use os::DeviceOs;
#[cfg(not(target_os = "windows"))]
pub(crate) use os_stub::DeviceOs;
#[cfg(target_os = "windows")]
pub use texture::GpuSurface;
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 12 video tier.
pub struct D3d12;
