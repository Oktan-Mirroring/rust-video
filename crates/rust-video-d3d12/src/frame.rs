//! Decoded frame: NV12 bytes with identity, lease, and export.

use crate::seam::Seam;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Surface};
use rust_video_caps::traits::Lease;

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct D3d12Frame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
    /// Platform export, absent without NT-handle sharing.
    os: Option<Box<dyn Seam>>,
}

impl D3d12Frame {
    /// Decoded frame over leased bytes with identity and export.
    ///
    /// # Arguments
    ///
    /// * `identity` - frame size, format, and generation.
    /// * `data` - NV12 frame bytes in row-major order.
    /// * `lease` - VRAM budget held for the bytes.
    /// * `os` - platform export, absent without NT-handle sharing.
    // Unused off-target: only the Windows module and tests construct frames.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn new(
        identity: GpuFrame,
        data: Bytes,
        lease: Lease,
        os: Option<Box<dyn Seam>>,
    ) -> Self {
        Self {
            identity,
            data,
            lease,
            os,
        }
    }

    /// Frame identity.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Pixel layout behind the frame.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.identity.format()
    }

    /// Image array layer behind the picture.
    #[must_use]
    pub fn slice(&self) -> u32 {
        0
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.identity.size()
    }

    /// Crop left and top offsets in luma samples.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        (0, 0)
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
    /// NT handle over the shared pool texture, or an opaque backend
    /// descriptor when the driver declines sharing.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        if let Some(os) = &self.os {
            os.export()
        } else {
            GpuExport::Opaque { name: "d3d12" }
        }
    }

    /// CPU copy split into NV12 planes.
    #[must_use]
    pub fn to_cpu(&self) -> Option<CpuFrame> {
        CpuFrame::from_nv12(self.identity.size(), &self.data)
    }
}

impl Surface for D3d12Frame {
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
    use super::D3d12Frame;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat, Surface};
    use rust_video_caps::traits::SharedDevice;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> D3d12Frame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        D3d12Frame::new(identity, bytes, lease, None)
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("d3d12", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn export_without_sharing_names_backend() {
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(held.export(), GpuExport::Opaque { name: "d3d12" }));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("d3d12", 100);
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
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }

    #[test]
    fn frame_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<D3d12Frame>();
    }

    #[test]
    fn surface_accessors_delegate_to_identity() {
        fn assert_surface<T: Surface>() {}
        assert_surface::<D3d12Frame>();
        let shared = SharedDevice::new("d3d12", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.size().width(), 4);
        assert_eq!(held.size().height(), 2);
        assert_eq!(held.format(), PixelFormat::Nv12);
        assert_eq!(held.slice(), 0);
        assert_eq!(held.coded().width(), 4);
        assert_eq!(held.coded().height(), 2);
        assert_eq!(held.origin(), (0, 0));
    }
}
