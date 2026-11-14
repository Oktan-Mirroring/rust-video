//! Android `MediaCodec` video tier.

mod bridge;
mod decoder;
mod device;
mod encoder;
mod error;
mod format;
mod quirks;
mod sdk;

pub use bridge::{CodecEvent, EventQueue, PendingInputs};
pub use decoder::{ranged_bytes, DecodeState, MediaCodecDecoder, OutputLayout};
pub use device::MediaCodecDevice;
pub use encoder::{EncodeSpec, EncodeState, InputLayout, MediaCodecEncoder};
pub use error::MediaCodecError;
pub use format::{pixel_format_for, FormatSpec};
pub use quirks::{Quirk, QuirkStatus, QuirkSyntax, QuirkTable};
pub use sdk::{ApiLevel, ASYNC_NOTIFY_SDK, MIN_SDK};
