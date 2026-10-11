//! Safe Rust interface over realtime video encode and decode.
//!
//! Codec backends and platform tiers behind one facade.

pub mod error;
pub mod frame;
pub mod packet;
pub mod tier;
pub mod traits;

pub use error::Error;
pub use frame::{CpuFrame, CpuReadback, FramePool, GpuExport, GpuFrame, PixelFormat, Plane};
pub use packet::Packet;
pub use tier::{
    auto_order, host_platform, resolve, Capabilities, Platform, SoftwareH265, Tier, TierError,
    TierPreference, VideoCodec,
};
pub use traits::{Decoder, Device, Encoder, Lease, OverBudget, SharedDevice};
