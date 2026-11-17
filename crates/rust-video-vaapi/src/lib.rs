//! VA-API tiers over DRM render nodes: decode, encode, and transcode.

// Portable modules compile on every target; off Linux no public item
// reaches them, so each allows dead code.
#[allow(dead_code)]
mod av1;
#[cfg(target_os = "linux")]
mod backend;
#[allow(dead_code)]
mod bitstream;
#[cfg(target_os = "linux")]
mod buffer;
#[cfg(target_os = "linux")]
mod decoder;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod encoder;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod ffi;
#[allow(dead_code)]
mod frontend;
#[allow(dead_code)]
mod h264;
#[allow(dead_code)]
mod h265;
#[allow(dead_code)]
mod params;
#[cfg(target_os = "linux")]
mod scale;
#[allow(dead_code)]
mod seam;
#[cfg(target_os = "linux")]
mod sys;
#[cfg(target_os = "linux")]
mod transcode;
#[cfg(target_os = "linux")]
mod vp8;
#[cfg(target_os = "linux")]
mod vp9;

#[cfg(target_os = "linux")]
pub use av1::Av1Decoder;
#[cfg(target_os = "linux")]
pub use backend::{Backend, SubmitStats};
#[cfg(target_os = "linux")]
pub use decoder::{H264Decoder, VaApiFrame};
#[cfg(target_os = "linux")]
pub use device::{DisplayHandle, VaApiDevice};
#[cfg(target_os = "linux")]
pub use encoder::{EncodeCodec, VaApiEncoder};
#[cfg(target_os = "linux")]
pub use error::{SkipReason, VaApiError};
#[cfg(target_os = "linux")]
pub use h265::H265Decoder;
#[cfg(target_os = "linux")]
pub use scale::{ScaledSurface, Scaler};
#[cfg(target_os = "linux")]
pub use seam::{ContextId, SurfaceId, VaSeam};
#[cfg(target_os = "linux")]
pub use transcode::Transcoder;
#[cfg(target_os = "linux")]
pub use vp8::Vp8Decoder;
#[cfg(target_os = "linux")]
pub use vp9::Vp9Decoder;
