//! Device-resident NV12 surface behind GPU decode and transcode.

use crate::error::D3d12Error;
use crate::pins::SlotPin;
use crate::session::SharedExport;
use crate::submit::Fence;

use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat, Surface};
use windows::Win32::Graphics::Direct3D12::{ID3D12Fence, ID3D12Resource};

/// Device-resident decode surface without CPU access.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
///
/// Each live surface pins its decoder pool slot until dropped;
/// decoders report [`NoFreeSlot`](crate::error::D3d12Error::NoFreeSlot) instead
/// of overwriting a held picture.
///
/// Threading: a single thread needs nothing. Surfaces share nothing mutable,
/// and the device and command queues are free-threaded, so threads sharing
/// one device need no extra serialization.
pub struct GpuSurface {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 output texture.
    resource: ID3D12Resource,
    /// Geometry plus pool slot behind the picture.
    layout: SurfaceLayout,
    /// Pool pin, absent for derived textures.
    pin: Option<SlotPin>,
    /// Completion fence, absent once the GPU signaled.
    wait: Option<SurfaceWait>,
    /// Shared-texture export, absent without NT-handle sharing.
    shared: Option<SharedExport>,
}

impl GpuSurface {
    /// Surface over `resource` with `layout` at `generation`.
    pub(crate) fn new(
        resource: ID3D12Resource,
        layout: SurfaceLayout,
        generation: u64,
        pin: Option<SlotPin>,
        wait: Option<SurfaceWait>,
        shared: Option<SharedExport>,
    ) -> Self {
        Self {
            identity: GpuFrame::new(layout.visible, PixelFormat::Nv12, generation),
            resource,
            layout,
            pin,
            wait,
            shared,
        }
    }

    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Pixel layout behind the surface.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.identity.format()
    }

    /// Image array layer behind the picture.
    ///
    /// Single-layer textures always report layer 0; the decoder pool
    /// slot stays internal.
    #[must_use]
    pub fn slice(&self) -> u32 {
        if let Some(pin) = &self.pin {
            debug_assert_eq!(u32::from(pin.slot()), self.layout.slot);
        }
        0
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.layout.coded
    }

    /// Crop left and top offsets in luma samples.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        self.layout.origin
    }

    /// Platform export handle for the surface.
    ///
    /// NT handle over the pool texture, or an opaque backend
    /// descriptor when the driver declines sharing.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        if let Some(shared) = &self.shared {
            return GpuExport::NtHandle {
                handle: shared.handle(),
            };
        }
        GpuExport::Opaque { name: "d3d12" }
    }

    /// NV12 output texture.
    #[must_use]
    pub fn texture(&self) -> &ID3D12Resource {
        &self.resource
    }

    /// Surface over a caller-owned shader target at `size`.
    ///
    /// The target leaves no pool slot, fence, or share behind: it reads
    /// back through the encoder only, never through pool accounting.
    #[must_use]
    pub fn wrap(resource: ID3D12Resource, size: PictureSize) -> Self {
        Self {
            identity: GpuFrame::new(size, PixelFormat::Nv12, 0),
            resource,
            layout: SurfaceLayout::new(size, size, (0, 0), 0),
            pin: None,
            wait: None,
            shared: None,
        }
    }

    /// Surface completion through the fence event.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget.
    pub fn try_wait(&self) -> Result<(), D3d12Error> {
        if let Some(wait) = &self.wait {
            wait.try_wait()?;
        }
        Ok(())
    }

    /// Texture over `resource` at `size` keeping the generation.
    #[must_use]
    pub(crate) fn rescaled(&self, resource: ID3D12Resource, size: PictureSize) -> Self {
        Self {
            identity: GpuFrame::new(size, PixelFormat::Nv12, self.identity.generation()),
            resource,
            layout: SurfaceLayout {
                visible: size,
                coded: size,
                origin: (0, 0),
                slot: self.layout.slot,
            },
            pin: None,
            wait: None,
            shared: None,
        }
    }
}

impl Surface for GpuSurface {
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

/// NV12 surface geometry plus pool slot.
pub(crate) struct SurfaceLayout {
    /// Visible extent in luma samples.
    visible: PictureSize,
    /// Full texture extent in luma samples.
    coded: PictureSize,
    /// Crop left and top offsets in luma samples.
    origin: (u32, u32),
    /// Pool slot behind the picture.
    slot: u32,
}

impl SurfaceLayout {
    /// Layout for pool `slot` at `visible` inside the `coded` extent.
    pub(crate) fn new(
        visible: PictureSize,
        coded: PictureSize,
        origin: (u32, u32),
        slot: u32,
    ) -> Self {
        Self {
            visible,
            coded,
            origin,
            slot,
        }
    }
}

/// Completion fence behind one surface.
pub(crate) struct SurfaceWait {
    /// Fence behind the surface ticket.
    fence: ID3D12Fence,
    /// Fence value the queue signals.
    value: u64,
}

impl SurfaceWait {
    /// Wait for `value` on `fence`.
    pub(crate) fn new(fence: ID3D12Fence, value: u64) -> Self {
        Self { fence, value }
    }

    /// Surface completion through a transient event.
    pub(crate) fn try_wait(&self) -> Result<(), D3d12Error> {
        Fence::wait_on(&self.fence, self.value)
    }
}
