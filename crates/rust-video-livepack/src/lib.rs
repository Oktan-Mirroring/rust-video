//! Live TS and fMP4/CMAF packaging for coded video.
//!
//! Streaming-first: bounded memory, no full-file buffering.

mod codec;
mod fmp4;
mod queue;
mod time;
mod ts;

pub use codec::{AccessUnit, AnnexB, Codec, KeyframeGate, KeyframeState, PacketAction};
pub use fmp4::{
    Av1Config, AvcConfig, ConfigError, Fmp4Error, FragmentMux, HevcConfig, TrackConfig,
};
pub use queue::{BoundedQueue, FrameSlot, Policy, PushError};
pub use time::{ClockTracker, Pcr, Ts90k, PCR_EXT_MAX, PCR_TICKS_PER_SEC, STC_WRAP, TICKS_PER_SEC};
pub use ts::{TsDemux, TsDemuxError, TsMux, TsMuxConfig, TsMuxError};
