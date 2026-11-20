//! Android `MediaCodec` video tier.

#[cfg(target_os = "android")]
mod android;
mod bridge;
mod decoder;
mod device;
mod encoder;
mod error;
mod format;
mod overlap;
mod sdk;
mod seam;
#[cfg(not(target_os = "android"))]
mod seam_stub;
mod surface;

pub use bridge::{CodecEvent, EventQueue, PendingInputs};
pub use decoder::{DecodeState, MediaCodecDecoder, OutputLayout};
pub use device::MediaCodecDevice;
pub use encoder::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder};
pub use error::MediaCodecError;
pub use format::FormatSpec;
pub use overlap::{PipelineLedger, SubmitOutcome};
pub use sdk::{ApiLevel, ASYNC_NOTIFY_SDK, INPUT_SURFACE_SDK, MIN_SDK};
pub use surface::{MediaCodecSurface, SlotGuard, SurfaceSlots};
