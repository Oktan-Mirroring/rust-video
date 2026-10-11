//! Android `MediaCodec` video tier.

mod bridge;
mod decoder;
mod device;
mod error;
mod format;
mod quirks;
mod sdk;

pub use bridge::{CodecEvent, EventQueue};
pub use decoder::{ranged_bytes, DecodeState, MediaCodecDecoder, OutputLayout};
pub use device::MediaCodecDevice;
pub use error::MediaCodecError;
pub use format::{pixel_format_for, FormatSpec};
pub use quirks::{Quirk, QuirkStatus, QuirkSyntax, QuirkTable};
pub use sdk::{ApiLevel, ASYNC_NOTIFY_SDK, MIN_SDK};
