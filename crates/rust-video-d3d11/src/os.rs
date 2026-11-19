//! Windows OS state behind the portable d3d11 handles.

use std::sync::Arc;

use crate::submit::SubmitLock;

use rust_video_caps::traits::SharedDevice;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11VideoContext, ID3D11VideoDevice,
};

/// Windows device state behind the portable device.
pub(crate) struct DeviceOs {
    /// Logical device.
    pub(crate) device: ID3D11Device,
    /// Immediate context.
    pub(crate) context: ID3D11DeviceContext,
    /// Video device behind decode creation.
    pub(crate) video_device: ID3D11VideoDevice,
    /// Video context behind frame submission.
    pub(crate) video_context: ID3D11VideoContext,
    /// VRAM budget shared with decoders.
    pub(crate) shared: SharedDevice,
    /// Single-submitter lock over the immediate context.
    pub(crate) submit: Arc<SubmitLock>,
    /// Shared-texture export behind the adapter.
    pub(crate) shareable: bool,
}

impl DeviceOs {
    /// Logical device behind the tier.
    pub(crate) fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Immediate context behind copies and readback.
    pub(crate) fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Video device behind decode creation.
    pub(crate) fn video_device(&self) -> &ID3D11VideoDevice {
        &self.video_device
    }

    /// Video context behind frame submission.
    pub(crate) fn video_context(&self) -> &ID3D11VideoContext {
        &self.video_context
    }

    /// VRAM budget shared with decoders.
    pub(crate) fn shared(&self) -> &SharedDevice {
        &self.shared
    }

    /// Single-submitter lock over the immediate context.
    pub(crate) fn submit(&self) -> &Arc<SubmitLock> {
        &self.submit
    }

    /// Shared-texture export behind the adapter.
    pub(crate) fn shareable(&self) -> bool {
        self.shareable
    }
}
