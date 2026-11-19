//! Windows OS state behind the portable d3d12 handles.

use crate::seam::Seam;
use crate::session::SharedExport;
use crate::submit::DeviceResources;

use rust_video_caps::frame::GpuExport;
use rust_video_caps::traits::SharedDevice;
use windows::Win32::Graphics::Direct3D12::ID3D12Device;
use windows::Win32::Media::MediaFoundation::ID3D12VideoDevice;

/// Windows device state behind the portable device.
pub(crate) struct DeviceOs {
    /// Logical device.
    device: ID3D12Device,
    /// Video device behind decode and encode queries.
    video: ID3D12VideoDevice,
    /// VRAM budget shared with decoders.
    shared: SharedDevice,
    /// Shared-texture export behind the adapter.
    shareable: bool,
}

impl DeviceOs {
    /// Windows device state over `device` and `video` with `shared` budget.
    ///
    /// # Arguments
    ///
    /// * `device` - logical device.
    /// * `video` - video device behind decode and encode queries.
    /// * `shared` - VRAM budget shared with decoders.
    pub(crate) fn new(
        device: ID3D12Device,
        video: ID3D12VideoDevice,
        shared: SharedDevice,
    ) -> Self {
        let shareable = DeviceResources::new(&device).probe_shared_export();
        Self {
            device,
            video,
            shared,
            shareable,
        }
    }

    /// Logical device behind the tier.
    pub(crate) fn device(&self) -> &ID3D12Device {
        &self.device
    }

    /// Video device behind decode and encode queries.
    pub(crate) fn video(&self) -> &ID3D12VideoDevice {
        &self.video
    }

    /// VRAM budget shared with decoders.
    pub(crate) fn shared(&self) -> &SharedDevice {
        &self.shared
    }

    /// Shared-texture export behind the adapter.
    pub(crate) fn shareable(&self) -> bool {
        self.shareable
    }
}

/// Windows export behind one decoded frame.
pub(crate) struct FrameOs {
    /// NT handle export over the pool texture.
    shared: SharedExport,
}

impl FrameOs {
    /// Boxed seam for `shared`, absent without NT-handle sharing.
    ///
    /// # Arguments
    ///
    /// * `shared` - NT handle export over the pool texture.
    pub(crate) fn wrap(shared: Option<SharedExport>) -> Option<Box<dyn Seam>> {
        shared.map(|shared| Box::new(Self { shared }) as Box<dyn Seam>)
    }
}

impl Seam for FrameOs {
    fn export(&self) -> GpuExport {
        GpuExport::NtHandle {
            handle: self.shared.handle(),
        }
    }
}
