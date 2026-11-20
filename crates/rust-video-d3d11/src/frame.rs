//! Decoded frame: NV12 bytes with identity, lease, and export.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Surface};
use rust_video_caps::traits::Lease;

use crate::seam::FrameExport;

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct D3d11Frame {
    /// Frame identity: size, format, generation.
    pub(crate) identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    pub(crate) data: Bytes,
    /// VRAM budget held for the bytes.
    pub(crate) lease: Lease,
    /// Shared-texture export behind the seam, absent without NT-handle sharing.
    pub(crate) export: Option<Box<dyn FrameExport>>,
}

impl D3d11Frame {
    /// Frame identity.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
    }

    /// NV12 frame bytes in row-major order.
    #[must_use]
    pub fn nv12(&self) -> &[u8] {
        &self.data
    }

    /// Budget bytes the frame holds.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// Platform export handle for the frame.
    ///
    /// NT handle over the shared decode texture, or an opaque backend
    /// descriptor when the driver declines sharing.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        if let Some(export) = &self.export {
            return export.export();
        }
        GpuExport::Opaque { name: "d3d11" }
    }

    /// CPU copy split into NV12 planes.
    #[must_use]
    pub fn to_cpu(&self) -> Option<CpuFrame> {
        CpuFrame::from_nv12(self.identity.size(), &self.data)
    }

    /// Visible picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Pixel layout.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.identity.format()
    }

    /// Array layer of this picture.
    #[must_use]
    pub fn slice(&self) -> u32 {
        0
    }

    /// Full texture extent.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.size()
    }

    /// Crop left/top in luma samples.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        (0, 0)
    }
}

impl Surface for D3d11Frame {
    fn size(&self) -> PictureSize {
        self.size()
    }

    fn format(&self) -> PixelFormat {
        self.format()
    }

    fn slice(&self) -> u32 {
        self.slice()
    }

    fn coded(&self) -> PictureSize {
        self.coded()
    }

    fn origin(&self) -> (u32, u32) {
        self.origin()
    }

    fn export(&self) -> GpuExport {
        self.export()
    }
}

#[cfg(test)]
mod tests {
    use super::D3d11Frame;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> D3d11Frame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        D3d11Frame {
            identity,
            data: bytes,
            lease,
            export: None,
        }
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("d3d11", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn export_without_sharing_names_backend() {
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(held.export(), GpuExport::Opaque { name: "d3d11" }));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("d3d11", 100);
        let mut data = vec![1; 8];
        data.extend_from_slice(&[2; 4]);
        let held = frame(&shared, 4, 2, data);
        let cpu = held.to_cpu().expect("full nv12");
        assert_eq!(cpu.format(), PixelFormat::Nv12);
        assert_eq!(cpu.planes().len(), 2);
        assert_eq!(cpu.planes()[0].data(), &[1; 8]);
        assert_eq!(cpu.planes()[1].data(), &[2; 4]);
    }

    #[test]
    fn to_cpu_rejects_truncated_bytes() {
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }

    #[test]
    fn surface_reports_full_frame_readback() {
        use rust_video_caps::frame::Surface;

        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert_eq!(held.size(), held.identity().size());
        assert_eq!(held.format(), PixelFormat::Nv12);
        assert_eq!(held.slice(), 0);
        assert_eq!(held.coded(), held.size());
        assert_eq!(held.origin(), (0, 0));
        let surface: &dyn Surface = &held;
        assert_eq!(surface.size(), held.size());
        assert!(matches!(surface.export(), GpuExport::Opaque { .. }));
    }
}
