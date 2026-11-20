//! Safe Rust interface over realtime video encode and decode.
//!
//! Codec backends and platform tiers behind one facade.

pub mod error;
pub mod tier;

pub use error::Error;
pub use tier::{Capabilities, Platform, SoftwareH265, Tier, TierError, TierPreference, VideoCodec};

pub use rust_video_caps::common::PictureSize;
pub use rust_video_caps::frame::{
    CpuFrame, CpuReadback, FramePool, GpuExport, GpuFrame, PixelFormat, Plane,
};
pub use rust_video_caps::packet::Packet;
pub use rust_video_caps::traits::{Decoder, Device, Encoder, Lease, OverBudget, SharedDevice};
pub use rust_video_caps::transcode::{FallbackCause, Plan, Route, Scale};
