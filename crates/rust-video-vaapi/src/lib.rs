//! VA-API decode tier over DRM render nodes.

#[cfg(target_os = "linux")]
mod av1;
#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod bitstream;
#[cfg(target_os = "linux")]
mod decoder;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod error;
#[cfg(target_os = "linux")]
mod ffi;
#[cfg(target_os = "linux")]
mod frontend;
#[cfg(target_os = "linux")]
mod h264;
#[cfg(target_os = "linux")]
mod h265;
#[cfg(target_os = "linux")]
mod params;
#[cfg(target_os = "linux")]
mod vp8;
#[cfg(target_os = "linux")]
mod vp9;

#[cfg(target_os = "linux")]
pub use av1::Av1Decoder;
#[cfg(target_os = "linux")]
pub use decoder::{H264Decoder, VaApiFrame};
#[cfg(target_os = "linux")]
pub use device::VaApiDevice;
#[cfg(target_os = "linux")]
pub use error::{SkipReason, VaApiError};
#[cfg(target_os = "linux")]
pub use h265::H265Decoder;
#[cfg(target_os = "linux")]
pub use vp8::Vp8Decoder;
#[cfg(target_os = "linux")]
pub use vp9::Vp9Decoder;
