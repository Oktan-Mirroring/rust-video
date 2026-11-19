//! Shared sequence state behind the live codec decoders.

use std::sync::Arc;

use crate::dpb::Output;
use crate::error::{D3d11Error, ParseReason};
use crate::frame::D3d11Frame;
use crate::h264::Crop;
use crate::pins::SlotPins;
use crate::seam::FrameExport;
use crate::session::Session;
use crate::texture::{ExportLayout, GpuSurface};

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuFrame, PixelFormat};
use rust_video_caps::traits::SharedDevice;

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
