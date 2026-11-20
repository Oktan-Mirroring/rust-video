//! Direct3D 11 video tier: hardware decode and encode behind the caps traits.
//!
//! Hardware decode matches the ffmpeg software oracle byte-exact
//! (`tests/uniform.rs h264_hardware_matches_software_oracle`).

#[cfg(target_os = "windows")]
mod av1;
#[cfg(target_os = "windows")]
mod decoder;
mod device;
#[cfg(target_os = "windows")]
mod device_os;
#[cfg(target_os = "windows")]
mod dpb;
#[cfg(target_os = "windows")]
mod encode;
#[cfg(target_os = "windows")]
mod encoder;
mod error;
mod frame;
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
mod session;
mod shell;
#[cfg(target_os = "windows")]
mod submit;
#[cfg(target_os = "windows")]
mod texture;
#[cfg(target_os = "windows")]
mod transcode;
#[cfg(target_os = "windows")]
mod vp9;

#[cfg(target_os = "windows")]
pub use av1::Av1Decoder;
pub use device::D3d11Device;
#[cfg(target_os = "windows")]
pub use encode::Vp9Tuning;
#[cfg(target_os = "windows")]
pub use encoder::D3d11Encoder;
pub use error::{BrokenReferenceReason, D3d11Error, ParseReason, SkipReason, UnsupportedReason};
pub use frame::D3d11Frame;
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub use h264::H264Decoder;
#[cfg(target_os = "windows")]
pub use h265::HevcDecoder;
#[cfg(target_os = "windows")]
pub(crate) use os::DeviceOs;
#[cfg(not(target_os = "windows"))]
pub(crate) use os_stub::DeviceOs;
#[cfg(target_os = "windows")]
pub use overlap::OverlappedPump;
#[cfg(not(target_os = "windows"))]
pub use shell::Vp9Decoder;
pub use shell::{Mpeg2Decoder, Vc1Decoder};
#[cfg(target_os = "windows")]
pub use submit::SubmitLock;
#[cfg(target_os = "windows")]
pub use texture::GpuSurface;
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;
#[cfg(target_os = "windows")]
pub use vp9::Vp9Decoder;

/// Direct3D 11 video tier.
pub struct D3d11;
