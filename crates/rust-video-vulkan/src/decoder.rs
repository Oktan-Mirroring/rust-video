//! H.264 decode behind the facade `Decoder` trait.

use crate::device::VulkanDevice;
use crate::error::VulkanError;
use crate::inflight::{Inflight, Signaled};

use bytes::Bytes;
use rust_video::{
    CpuFrame, Decoder, GpuExport, GpuFrame, Lease, Packet, PixelFormat, Plane, SharedDevice,
};
use rust_video_caps::common::PictureSize;

/// H.264 decoder over the gpu-video engine.
pub struct H264Decoder {
    /// Decode engine, synchronous per call.
    engine: gpu_video::BytesDecoder,
    /// VRAM budget leased per frame.
    shared: SharedDevice,
    /// One signaled fence per submitted frame.
    inflight: Inflight<Signaled>,
    /// Next frame generation number.
    generation: u64,
    /// Latched when the engine panics; the engine is never touched again.
    poisoned: bool,
}

impl H264Decoder {
    /// Per-fence drain timeout in nanoseconds on flush.
    const DRAIN_TIMEOUT_NS: u64 = 100_000_000;

    /// Decoder over the negotiated `device`.
    ///
    /// The engine builds its own instance and logical device:
    /// gpu-video 0.4.0 exposes no external-device import, so the
    /// negotiated device gates creation while the session lives on
    /// the engine's.
    /// <https://docs.rs/gpu-video/0.4.0/gpu_video/struct.VulkanDevice.html>
    ///
    /// # Errors
    ///
    /// Returns `Err` when the engine cannot start on this machine.
    pub fn open(device: &VulkanDevice) -> Result<Self, VulkanError> {
        let instance = gpu_video::VulkanInstance::new()?;
        let descriptor = gpu_video::parameters::VulkanAdapterDescriptor {
            supports_decoding: true,
            supports_encoding: false,
        };
        let adapter = instance.create_adapter(&descriptor)?;
        let engine =
            adapter.create_device(&gpu_video::parameters::VulkanDeviceDescriptor::default())?;
        let decoder =
            engine.create_bytes_decoder_h264(gpu_video::parameters::DecoderParameters::default())?;
        Ok(Self {
            engine: decoder,
            shared: device.shared().clone(),
            inflight: Inflight::new(),
            generation: 0,
            poisoned: false,
        })
    }

    /// Engine call guarded against engine panics on hostile input.
    ///
    /// gpu-video 0.4.0 unwraps a `None` NAL parse on degenerate tails
    /// (`parser/nalu_parser.rs:27` in the published source); the guard
    /// turns that into `EngineFault` and poisons the decoder.
    fn guarded<T>(
        &mut self,
        op: impl FnOnce(&mut gpu_video::BytesDecoder) -> Result<T, gpu_video::DecoderError>,
    ) -> Result<T, VulkanError> {
        if self.poisoned {
            return Err(VulkanError::EngineFault);
        }
        // Sound: a caught panic latches `poisoned`, so the engine state
        // the panic may have corrupted is never observed again.
        let caught =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| op(&mut self.engine)));
        let Ok(result) = caught else {
            self.poisoned = true;
            return Err(VulkanError::EngineFault);
        };
        Ok(result?)
    }

    /// Engine frames wrapped with identity and budget leases.
    fn wrap(
        &mut self,
        frames: Vec<gpu_video::OutputFrame<gpu_video::RawFrameData>>,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::with_capacity(frames.len());
        for frame in frames {
            let size = PictureSize::new(frame.data.width, frame.data.height).ok_or(
                VulkanError::BadExtent {
                    width: frame.data.width,
                    height: frame.data.height,
                },
            )?;
            let data = Bytes::from(frame.data.frame);
            let lease = self.shared.try_acquire(data.len() as u64)?;
            let identity = GpuFrame::new(size, PixelFormat::Nv12, self.generation);
            self.generation = self.generation.wrapping_add(1);
            self.inflight.submit(Signaled);
            out.push(VulkanFrame {
                identity,
                data,
                lease,
            });
        }
        Ok(out)
    }
}

impl Decoder for H264Decoder {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.inflight.reap()?;
        let pts = packet
            .pts()
            .map(|stamp| u64::try_from(stamp.as_micros()).unwrap_or(u64::MAX));
        let chunk = gpu_video::EncodedInputChunk {
            data: packet.data(),
            pts,
        };
        let frames = self.guarded(|engine| engine.decode(chunk))?;
        self.wrap(frames)
    }

    fn flush(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frames = self.guarded(gpu_video::BytesDecoder::flush)?;
        let out = self.wrap(frames)?;
        self.inflight.drain(Self::DRAIN_TIMEOUT_NS)?;
        Ok(out)
    }
}

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct VulkanFrame {
    /// Facade identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
}

impl VulkanFrame {
    /// Facade identity of the frame.
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
    /// Opaque backend descriptor: the 0.4.0 engine readbacks to CPU,
    /// so no platform handle exists to hand out. Infallible by shape,
    /// never `BackendAbsent`.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "vulkan" }
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

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video::{GpuExport, GpuFrame, PixelFormat, SharedDevice};
    use rust_video_caps::common::PictureSize;

    use super::VulkanFrame;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> VulkanFrame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        VulkanFrame {
            identity,
            data: bytes,
            lease,
        }
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("vulkan", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn frame_past_budget_fails() {
        let shared = SharedDevice::new("vulkan", 10);
        let size = PictureSize::new(4, 2).expect("nonzero extent");
        let over = shared.try_acquire(11);
        assert!(over.is_err());
        assert_eq!(size.width(), 4);
    }

    #[test]
    fn export_names_backend_without_handle() {
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(
            held.export(),
            GpuExport::Opaque { name: "vulkan" }
        ));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("vulkan", 100);
        let mut data = vec![1; 8];
        data.extend_from_slice(&[2; 4]);
        let held = frame(&shared, 4, 2, data);
        let cpu = held.to_cpu().expect("full nv12");
        assert_eq!(cpu.format(), PixelFormat::Nv12);
        assert_eq!(cpu.planes().len(), 2);
        assert_eq!(cpu.planes()[0].data(), &[1; 8]);
        assert_eq!(cpu.planes()[0].stride(), 4);
        assert_eq!(cpu.planes()[1].data(), &[2; 4]);
        assert_eq!(cpu.planes()[1].stride(), 4);
    }

    #[test]
    fn to_cpu_rejects_truncated_bytes() {
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }
}
