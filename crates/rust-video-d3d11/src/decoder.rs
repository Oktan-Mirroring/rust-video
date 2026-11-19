//! Shared decode surfaces, sequence state, and codec re-exports.

use std::sync::Arc;

pub use crate::av1::Av1Decoder;
use crate::dpb::Output;
use crate::error::{D3d11Error, ParseReason};
use crate::frame::{D3d11Frame, FrameExport};
use crate::h264::Crop;
pub use crate::h264::H264Decoder;
pub use crate::h265::HevcDecoder;
use crate::pins::SlotPins;
use crate::session::Session;
use crate::session::SharedExport;
use crate::submit::SurfaceFence;
pub use crate::vp9::Vp9Decoder;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
use rust_video_caps::traits::SharedDevice;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// Device-resident decode surface without CPU access.
///
/// A single thread needs nothing. Threads sharing one device hold
/// [`SubmitLock`](crate::SubmitLock) around their own direct context calls
/// to serialize with crate submissions; [`wait`](Self::wait) never holds it.
///
/// A surface borrows nothing: it holds shared ownership of its decode
/// texture, its completion fence, and its decoder slot pin.
///
/// A live surface keeps its own resources alive: dropping the decoder never invalidates outstanding surfaces, and dropping a surface releases its slot.
pub struct GpuSurface {
    /// Shared decode texture plus its NT handle.
    export: SharedExport,
    /// Two-plane export description behind the surface.
    layout: ExportLayout,
    /// Completion fence behind the submission.
    fence: SurfaceFence,
    /// Decoder slot pins; the surface pin releases on drop.
    pins: Arc<SlotPins>,
    /// Pinned decoder slot.
    pub(crate) slot: u8,
}

impl GpuSurface {
    /// Picture dimensions in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.layout.visible()
    }

    /// Pixel layout behind the surface.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.layout.format()
    }

    /// Array layer behind the picture.
    #[must_use]
    pub fn slice(&self) -> u32 {
        u32::from(self.layout.slice())
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.layout.coded()
    }

    /// Crop left and top in luma samples.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        (self.layout.left(), self.layout.top())
    }

    /// Portable export behind the surface.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::NtHandle {
            handle: self.export.handle(),
        }
    }

    /// Decode texture behind the surface.
    ///
    /// Valid while the surface lives; the array layer is [`slice`](Self::slice).
    #[must_use]
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.export
    }

    /// Completion of the submission behind the surface.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the GPU misses the wait budget or reports failure.
    pub fn wait(&self) -> Result<(), D3d11Error> {
        self.fence.wait()
    }
}

impl Drop for GpuSurface {
    /// Decoder slot pin release.
    fn drop(&mut self) {
        self.pins.unpin(self.slot);
    }
}

/// NV12 two-plane export description over one array slice.
pub(crate) struct ExportLayout {
    /// Pixel layout; NV12 stores luma plus one chroma plane.
    format: PixelFormat,
    /// Array slice behind the picture.
    slice: u8,
    /// Visible left offset in luma samples.
    left: u32,
    /// Visible top offset in luma samples.
    top: u32,
    /// Visible extent in luma samples.
    visible: PictureSize,
    /// Full texture extent in luma samples.
    coded: PictureSize,
}

impl ExportLayout {
    /// Layout for `slot` at `visible` inside the coded extent, absent when empty.
    #[must_use]
    pub(crate) fn for_output(
        coded_width: u32,
        coded_height: u32,
        slot: u8,
        crop: Option<Crop>,
        visible: PictureSize,
    ) -> Option<Self> {
        let coded = PictureSize::new(coded_width, coded_height)?;
        let (left, top) = crop.map_or((0, 0), |crop| (crop.left, crop.top));
        Some(Self {
            format: PixelFormat::Nv12,
            slice: slot,
            left,
            top,
            visible,
            coded,
        })
    }

    /// Pixel layout behind the export.
    #[must_use]
    pub(crate) fn format(&self) -> PixelFormat {
        self.format
    }

    /// Array slice behind the picture.
    #[must_use]
    pub(crate) fn slice(&self) -> u8 {
        self.slice
    }

    /// Visible left offset in luma samples.
    #[must_use]
    pub(crate) fn left(&self) -> u32 {
        self.left
    }

    /// Visible top offset in luma samples.
    #[must_use]
    pub(crate) fn top(&self) -> u32 {
        self.top
    }

    /// Visible extent in luma samples.
    #[must_use]
    pub(crate) fn visible(&self) -> PictureSize {
        self.visible
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub(crate) fn coded(&self) -> PictureSize {
        self.coded
    }
}

/// Active sequence behind the live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Active {
    /// Sequence set id.
    pub(crate) sps_id: u8,
    /// Coded width in luma samples.
    pub(crate) width: u32,
    /// Coded height in luma samples.
    pub(crate) height: u32,
    /// Visible rectangle, absent when uncropped.
    crop: Option<Crop>,
    /// Visible size in luma samples.
    visible: PictureSize,
}

impl Active {
    /// Active sequence for `sps_id` at `width` x `height` with `crop` and visible extent.
    pub(crate) fn new(
        sps_id: u8,
        width: u32,
        height: u32,
        crop: Option<Crop>,
        visible_width: u32,
        visible_height: u32,
    ) -> Result<Self, D3d11Error> {
        let visible = PictureSize::new(visible_width, visible_height)
            .ok_or(D3d11Error::Parse(ParseReason::CropEmptiesPicture))?;
        Ok(Self {
            sps_id,
            width,
            height,
            crop,
            visible,
        })
    }

    /// Visible rectangle, absent when uncropped.
    pub(crate) fn crop(&self) -> Option<Crop> {
        self.crop
    }
}

/// Frame for copied `slot`.
pub(crate) fn emit_frame(
    session: &Session,
    shared: &SharedDevice,
    slot: u8,
    active: &Active,
    generation: u64,
) -> Result<D3d11Frame, D3d11Error> {
    let data = session.read_slot(slot, active.crop)?;
    emit_frame_bytes(session, shared, active, generation, data)
}

/// Frame over already-read `data`.
pub(crate) fn emit_frame_bytes(
    session: &Session,
    shared: &SharedDevice,
    active: &Active,
    generation: u64,
    data: Bytes,
) -> Result<D3d11Frame, D3d11Error> {
    let lease = shared.try_acquire(data.len() as u64)?;
    let export = session.export().map(|shared| {
        let export: Box<dyn FrameExport> = Box::new(shared);
        export
    });
    Ok(D3d11Frame {
        identity: GpuFrame::new(active.visible, PixelFormat::Nv12, generation),
        data,
        lease,
        export,
    })
}

/// Surface for `output`, absent without NT-handle sharing.
pub(crate) fn emit_surface(
    session: &Session,
    output: &Output,
    active: &Active,
    pins: &Arc<SlotPins>,
) -> Option<GpuSurface> {
    let (export, slot, fence) = session.surface(output.slot())?;
    let layout = ExportLayout::for_output(
        active.width,
        active.height,
        slot,
        active.crop,
        active.visible,
    )?;
    pins.pin(slot);
    Some(GpuSurface {
        export,
        layout,
        fence,
        pins: Arc::clone(pins),
        slot,
    })
}

#[cfg(test)]
mod tests {
    use super::ExportLayout;
    use crate::h264::Crop;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::PixelFormat;

    #[test]
    fn layout_names_both_planes() {
        let visible = PictureSize::new(1920, 1080).expect("nonzero extent");
        let layout = ExportLayout::for_output(1920, 1088, 3, None, visible).expect("nonzero coded");
        assert_eq!(layout.format(), PixelFormat::Nv12);
        assert_eq!(layout.format().plane_count(), 2);
        assert_eq!(layout.slice(), 3);
        assert_eq!((layout.left(), layout.top()), (0, 0));
        assert_eq!(layout.visible(), visible);
        assert_eq!(
            layout.coded(),
            PictureSize::new(1920, 1088).expect("nonzero coded")
        );
    }

    #[test]
    fn layout_carries_crop_origin() {
        let visible = PictureSize::new(1920, 1080).expect("nonzero extent");
        let crop = Crop {
            left: 0,
            right: 0,
            top: 0,
            bottom: 8,
        };
        let layout =
            ExportLayout::for_output(1920, 1088, 0, Some(crop), visible).expect("nonzero coded");
        assert_eq!((layout.left(), layout.top()), (0, 0));
        assert_eq!(layout.visible(), visible);
        assert_eq!(
            layout.coded(),
            PictureSize::new(1920, 1088).expect("nonzero coded")
        );
    }

    #[test]
    fn layout_rejects_empty_coded() {
        let visible = PictureSize::new(1920, 1080).expect("nonzero extent");
        assert!(ExportLayout::for_output(0, 1088, 0, None, visible).is_none());
    }
}
