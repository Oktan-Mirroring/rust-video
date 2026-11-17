//! Decoded frames plus probed decoder shells behind the caps `Decoder` trait.

use crate::device::D3d12Device;
use crate::error::{D3d12Error, SkipReason, UnsupportedReason};
use crate::guid::Codec;
use crate::seam::Seam;

use bytes::Bytes;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Lease};

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
        let size = self.identity.size();
        let luma = u64::from(size.width()) * u64::from(size.height());
        let y_len = usize::try_from(luma).ok()?;
        let uv_len = y_len / 2;
        if self.data.len() < y_len + uv_len {
            return None;
        }
        let y = self.data.slice(..y_len);
        let uv = self.data.slice(y_len..y_len + uv_len);
        let stride = size.width();
        CpuFrame::new(
            size,
            PixelFormat::Nv12,
            vec![Plane::new(y, stride)?, Plane::new(uv, stride)?],
        )
    }
}

/// Probed decoder shell; the picture builder is pending.
macro_rules! shell {
    ($decoder:ident, $codec:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device behind the profile probe.
            device: &'a D3d12Device,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; decode reports the pending builder.
            #[must_use]
            pub fn open(device: &'a D3d12Device) -> Self {
                Self { device }
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d12Frame;
            type Error = D3d12Error;

            fn decode(&mut self, _packet: &Packet) -> Result<Vec<D3d12Frame>, D3d12Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                Err(D3d12Error::Unsupported(
                    UnsupportedReason::PictureBuilderPending {
                        codec: Codec::$codec,
                    },
                ))
            }

            fn flush(&mut self) -> Result<Vec<D3d12Frame>, D3d12Error> {
                Ok(Vec::new())
            }
        }
    };
}

#[cfg(not(target_os = "windows"))]
shell!(
    Av1Decoder,
    Av1,
    "AV1 decoder; the picture builder is pending."
);
shell!(
    Mpeg2Decoder,
    Mpeg2,
    "MPEG-2 decoder; the picture builder is pending."
);
shell!(
    Vc1Decoder,
    Vc1,
    "VC-1 decoder; the picture builder is pending."
);
shell!(
    Vp9Decoder,
    Vp9,
    "VP9 decoder; the picture builder is pending."
);

#[cfg(test)]
mod tests {
    use super::D3d12Frame;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
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
}
