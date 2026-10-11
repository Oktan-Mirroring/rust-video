//! Software CPU codec backends behind the facade.
//!
//! Each backend wraps one threaded SIMD C library.

pub mod error;

#[cfg(feature = "av1-dav1d")]
pub mod av1;
#[cfg(feature = "h264-ffmpeg")]
pub mod ffmpeg;
#[cfg(feature = "h264-ffmpeg")]
mod ffmpeg_sys;
#[cfg(feature = "h264-openh264")]
pub mod h264;
#[cfg(feature = "vpx")]
pub mod vpx;

#[cfg(feature = "av1-dav1d")]
pub use av1::Av1Decoder;
pub use error::SwError;
#[cfg(feature = "h264-ffmpeg")]
pub use ffmpeg::H264Decoder;
#[cfg(feature = "h264-openh264")]
pub use h264::H264Encoder;
#[cfg(feature = "vpx")]
pub use vpx::{Vp8Decoder, Vp9Decoder, Vp9Encoder};
