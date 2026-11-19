//! VA-API tiers over DRM render nodes: decode, encode, and transcode.

// Portable modules compile on every target; off Linux no public item
// reaches them, so each allows dead code.
#[allow(dead_code)]
mod av1;
#[cfg(target_os = "linux")]
mod av1_decoder;
#[allow(dead_code)]
mod av1_frontend;
#[allow(dead_code)]
mod av1_params;
#[cfg(target_os = "linux")]
mod backend;
#[cfg(target_os = "linux")]
mod buffer;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod encoder;
#[allow(dead_code)]
mod error;
#[allow(dead_code)]
mod ffi;
#[cfg(test)]
mod fixtures;
#[allow(dead_code)]
mod h264;
#[cfg(target_os = "linux")]
mod h264_decoder;
#[allow(dead_code)]
mod h264_dpb;
#[allow(dead_code)]
mod h264_frontend;
#[allow(dead_code)]
mod h264_params;
#[allow(dead_code)]
mod h264_picture;
#[allow(dead_code)]
mod h265;
#[cfg(target_os = "linux")]
mod h265_decoder;
#[allow(dead_code)]
mod h265_dpb;
#[allow(dead_code)]
mod h265_frontend;
#[allow(dead_code)]
mod h265_params;
#[allow(dead_code)]
mod h265_picture;
#[allow(dead_code)]
mod inflight;
#[cfg(target_os = "linux")]
mod readback;
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
#[allow(dead_code)]
mod vp9;
#[cfg(target_os = "linux")]
mod vp9_decoder;
#[allow(dead_code)]
mod vp9_frontend;
#[allow(dead_code)]
mod vp9_lookups;
#[allow(dead_code)]
mod vp9_params;

#[cfg(target_os = "linux")]
pub use av1::Av1Decoder;
#[cfg(target_os = "linux")]
pub use backend::{Backend, PhaseStats};
#[cfg(target_os = "linux")]
pub use device::{DisplayHandle, VaApiDevice};
#[cfg(target_os = "linux")]
pub use encoder::{EncodeCodec, GopShape, VaApiEncoder};
#[cfg(target_os = "linux")]
pub use error::{SkipReason, VaApiError};
#[cfg(target_os = "linux")]
pub use h264_decoder::{H264Decoder, VaApiFrame};
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
