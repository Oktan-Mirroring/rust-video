//! Narrow OS seam behind the portable d3d11 handles.

use rust_video_caps::frame::GpuExport;

/// Shared-texture export behind one decoded frame.
pub(crate) trait FrameExport: Send + Sync {
    /// Platform export handle for the frame.
    fn export(&self) -> GpuExport;
}
