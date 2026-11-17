//! Android `MediaCodec` video tier.

#[cfg(target_os = "android")]
mod android;
mod bridge;
mod decoder;
mod device;
mod encoder;
mod error;
mod format;
mod sdk;
mod seam;
mod surface;

pub use bridge::{CodecEvent, EventQueue, PendingInputs};
pub use decoder::{ranged_bytes, DecodeState, MediaCodecDecoder, OutputLayout};
pub use device::MediaCodecDevice;
pub use encoder::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder};
pub use error::MediaCodecError;
pub use format::{pixel_format_for, FormatSpec};
pub use sdk::{ApiLevel, ASYNC_NOTIFY_SDK, MIN_SDK};
pub use surface::{MediaCodecSurface, SlotGuard, SurfaceSlots};
