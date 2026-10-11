//! Software CPU codec backends behind the facade.
//!
//! Decode rides one ffmpeg sys layer; each encoder wraps its own library.

pub mod error;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;
#[cfg(feature = "ffmpeg")]
mod ffmpeg_sys;
#[cfg(feature = "h264-openh264")]
pub mod h264;
#[cfg(feature = "vpx")]
pub mod vpx;

pub use error::SwError;
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::{
    Av1Decoder, Codec, FfmpegDecoder, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder,
};
#[cfg(feature = "h264-openh264")]
pub use h264::H264Encoder;
#[cfg(feature = "vpx")]
pub use vpx::Vp9Encoder;
