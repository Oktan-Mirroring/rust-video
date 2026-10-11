//! VA-API decode tier: H.264 over DRM render nodes.

mod backend;
mod decoder;
mod device;
mod error;
mod ffi;
mod frontend;
mod params;

pub use decoder::{H264Decoder, VaApiFrame};
pub use device::VaApiDevice;
pub use error::{SkipReason, VaApiError};
