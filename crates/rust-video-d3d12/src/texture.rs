//! Device-resident NV12 texture behind GPU transcode.

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuFrame, PixelFormat};
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

/// NV12 texture on the GPU, pixels never mapped.
pub(crate) struct GpuTexture {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 output texture.
    resource: ID3D12Resource,
}

impl GpuTexture {
    /// Texture over `resource` at `generation`.
    pub(crate) fn new(resource: ID3D12Resource, size: PictureSize, generation: u64) -> Self {
        Self {
            identity: GpuFrame::new(size, PixelFormat::Nv12, generation),
            resource,
        }
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub(crate) fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Texture over `resource` at `size` keeping the generation.
    #[must_use]
    pub(crate) fn rescaled(&self, resource: ID3D12Resource, size: PictureSize) -> Self {
        Self {
            identity: GpuFrame::new(size, PixelFormat::Nv12, self.identity.generation()),
            resource,
        }
    }

    /// NV12 output texture.
    #[must_use]
    pub(crate) fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }
}
