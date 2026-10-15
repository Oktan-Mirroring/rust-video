//! Software CPU codec backends behind the facade.
//!
//! Decode rides one ffmpeg sys layer; VP9 and AV1 encode ride their ffmpeg
//! wrappers. H.265 encode hides behind the `gpl` gate.

#[cfg(feature = "ffmpeg")]
pub mod av1;
pub mod error;
#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;
#[cfg(feature = "ffmpeg")]
mod ffmpeg_enc;
#[cfg(feature = "ffmpeg")]
mod ffmpeg_sys;
#[cfg(all(test, any(feature = "ffmpeg", feature = "h264-openh264")))]
mod fixtures;
#[cfg(feature = "h264-openh264")]
pub mod h264;
#[cfg(feature = "gpl")]
pub mod h265;
#[cfg(feature = "ffmpeg")]
pub mod vpx;

#[cfg(feature = "ffmpeg")]
pub use av1::Av1Encoder;
pub use error::SwError;
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::{
    Av1Decoder, Codec, FfmpegDecoder, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder,
};
#[cfg(feature = "h264-openh264")]
pub use h264::H264Encoder;
#[cfg(feature = "gpl")]
pub use h265::{H265Encoder, H265Preset};
#[cfg(feature = "ffmpeg")]
pub use vpx::Vp9Encoder;
