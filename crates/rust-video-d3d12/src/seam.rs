//! Narrow OS seam behind the portable d3d12 handles.

use rust_video_caps::frame::GpuExport;

/// Platform behavior behind portable d3d12 handles.
///
/// Implemented once, by the Windows OS module. Portable state holds
/// the implementation boxed so no portable module names a platform type.
pub(crate) trait Seam: Send + Sync {
    /// Platform export handle for a decoded frame.
    fn export(&self) -> GpuExport;
}
