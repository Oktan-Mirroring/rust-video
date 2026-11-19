//! Software surface contract over CPU frames.
//!
//! Decode lands in [`CpuFrame`]s, whose planes are readable inline.
//! The contract below fills the gaps the caps type leaves: the
//! single-layer provenance, the opaque ticket, and the trivial wait.

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport};

/// Software surface contract: geometry, provenance, ticket, and wait.
///
/// [`CpuFrame`] already carries the visible [`size`](CpuFrame::size)
/// and [`format`](CpuFrame::format); this trait adds the rest. CPU
/// frames hold their pixels inline, so the answers collapse:
///
/// * [`slice`](Self::slice) is always zero: one layer, no array.
/// * [`coded`](Self::coded) always matches the visible size:
///   libavcodec crops internally, planes start decoded.
/// * [`origin`](Self::origin) is always zero: planes start at the
///   visible origin.
/// * [`export`](Self::export) is always opaque: CPU memory has no
///   importable handle, so readers take the planes directly.
/// * [`wait`](Self::wait) returns immediately: frames are complete
///   at return.
///
/// Pinning is vacuous: frames own their bytes outright, so holding
/// a frame pins no decoder slot and decoding never exhausts one.
/// There is no device or queue to share; the only native object is
/// the codec context behind the decoder, and decoders move between
/// threads by hand-off as `Send` without `Sync`.
pub trait SwSurface {
    /// Array layer of this picture: always zero.
    #[must_use]
    fn slice(&self) -> u32;

    /// Full texture extent: always the visible size.
    #[must_use]
    fn coded(&self) -> PictureSize;

    /// Crop left/top in luma samples: always zero.
    #[must_use]
    fn origin(&self) -> (u32, u32);

    /// Native ticket: always opaque, CPU frames have no handle.
    ///
    /// Readers take the frame planes directly instead of importing
    /// this ticket.
    #[must_use]
    fn export(&self) -> GpuExport;

    /// Block until the frame completes: returns immediately.
    fn wait(&self);
}

impl SwSurface for CpuFrame {
    fn slice(&self) -> u32 {
        0
    }

    fn coded(&self) -> PictureSize {
        self.size()
    }

    fn origin(&self) -> (u32, u32) {
        (0, 0)
    }

    fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "sw" }
    }

    fn wait(&self) {}
}
