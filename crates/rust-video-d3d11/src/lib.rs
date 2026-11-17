//! Direct3D 11 video tier: hardware decode and encode behind the caps traits.
//!
//! Hardware decode matches the ffmpeg software oracle byte-exact
//! (`tests/e2e.rs hardware_matches_software_oracle`).

#[cfg(target_os = "windows")]
mod av1;
#[cfg(target_os = "windows")]
mod av1_dpb;
#[cfg(target_os = "windows")]
mod bits;
#[cfg(target_os = "windows")]
mod decoder;
#[cfg(target_os = "windows")]
mod delivery;
#[cfg(target_os = "windows")]
mod device;
#[cfg(not(target_os = "windows"))]
mod device_portable;
#[cfg(target_os = "windows")]
mod dpb;
#[cfg(target_os = "windows")]
mod dxva;
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
mod hevc_dpb;
#[cfg(target_os = "windows")]
mod inflight;
#[cfg(target_os = "windows")]
mod mf;
#[cfg(target_os = "windows")]
mod pins;
#[cfg(target_os = "windows")]
mod scale;
#[cfg(target_os = "windows")]
mod session;
mod shell;
#[cfg(target_os = "windows")]
mod submit;
#[cfg(target_os = "windows")]
mod transcode;
#[cfg(target_os = "windows")]
mod vp9;

#[cfg(target_os = "windows")]
pub use decoder::{Av1Decoder, GpuSurface, H264Decoder, HevcDecoder};
#[cfg(target_os = "windows")]
pub use delivery::DecoderPump;
#[cfg(target_os = "windows")]
pub use device::D3d11Device;
#[cfg(not(target_os = "windows"))]
pub use device_portable::D3d11Device;
#[cfg(target_os = "windows")]
pub use encoder::D3d11Encoder;
pub use error::{BrokenReferenceReason, D3d11Error, ParseReason, SkipReason, UnsupportedReason};
pub use frame::D3d11Frame;
pub use guid::{Codec, Profile};
#[cfg(target_os = "windows")]
pub use inflight::{FramePhases, OverlappedPump, PhaseCapture, SplitDecoder};
#[cfg(target_os = "windows")]
pub use mf::Vp9Tuning;
pub use shell::{Mpeg2Decoder, Vc1Decoder, Vp9Decoder};
#[cfg(target_os = "windows")]
pub use submit::SubmitLock;
#[cfg(target_os = "windows")]
pub use transcode::Transcoder;

pub use rust_video_caps::transcode::GpuCaps;

/// Direct3D 11 video tier.
pub struct D3d11;
