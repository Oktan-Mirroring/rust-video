//! Shared sequence state behind the live codec decoders.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{D3d12Error, ParseReason, UnsupportedReason};
use crate::frame::D3d12Frame;
use crate::h264::frontend::Crop;
use crate::os::FrameOs;
use crate::session::{DecodeSession, SharedExport};
use crate::submit::Ticket;
use crate::texture::SurfaceLayout;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{GpuFrame, PixelFormat};
use rust_video_caps::reason::ParseReason as SharedParse;
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
    pub(crate) crop: Option<Crop>,
    /// Visible size in luma samples.
    pub(crate) visible: PictureSize,
}

impl Active {
    /// True when `other` shares the coded extent.
    pub(crate) fn same_extent(self, other: Active) -> bool {
        self.width == other.width && self.height == other.height
    }
}

/// Queued sync output with its submit-side phases.
pub(crate) struct SyncPending {
    /// Pool slot behind the output.
    pub(crate) slot: u8,
    /// Copy ticket behind the queued download.
    pub(crate) ticket: Ticket,
    /// Sequence behind the output.
    pub(crate) active: Active,
    /// Submit instant.
    pub(crate) submitted_at: Instant,
    /// Frontend plus submit plus download-record CPU.
    pub(crate) submit: Duration,
    /// Video engine wait.
    pub(crate) engine: Duration,
}

/// Surface layout for pool `slot` under `active`.
pub(crate) fn surface_layout(active: &Active, slot: u8) -> Result<SurfaceLayout, D3d12Error> {
    let coded = PictureSize::new(active.width, active.height).ok_or(D3d12Error::Parse(
        ParseReason::Parse(SharedParse::EmptyCodedSize),
    ))?;
    let origin = active.crop.map_or((0, 0), |crop| (crop.left, crop.top));
    Ok(SurfaceLayout::new(
        active.visible,
        coded,
        origin,
        u32::from(slot),
    ))
}

/// NT-handle export for pool `slot`, honoring the session crop.
pub(crate) fn session_surface_export(
    session: &DecodeSession,
    slot: u8,
) -> Option<Arc<SharedExport>> {
    if session.cropped() {
        session.shared_handle_crop(slot)
    } else {
        session.shared_handle(slot)
    }
}

/// Visible NV12 bytes of coded `nv12` under `active`.
///
/// AV1 sessions decode at the sequence maximum; frames below it crop
/// the right and bottom pad.
///
/// # Errors
///
/// Returns `Err` past the picture cap, or when `nv12` falls short of
/// the coded extent.
pub(crate) fn visible_nv12(nv12: Bytes, active: &Active) -> Result<Bytes, D3d12Error> {
    let too_large = || D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge);
    let short = || D3d12Error::Unsupported(UnsupportedReason::ReadbackShortOfExtent);
    let visible_width = usize::try_from(active.visible.width()).map_err(|_| too_large())?;
    let visible_height = usize::try_from(active.visible.height()).map_err(|_| too_large())?;
    let coded_width = usize::try_from(active.width).map_err(|_| too_large())?;
    let coded_height = usize::try_from(active.height).map_err(|_| too_large())?;
    if visible_width == coded_width && visible_height == coded_height {
        return Ok(nv12);
    }
    let luma = coded_width.checked_mul(coded_height).ok_or_else(short)?;
    if nv12.len() != luma + luma / 2 {
        return Err(short());
    }
    let mut visible = Vec::with_capacity(visible_width * visible_height * 3 / 2);
    for row in 0..visible_height {
        let start = row.checked_mul(coded_width).ok_or_else(short)?;
        visible.extend_from_slice(nv12.get(start..start + visible_width).ok_or_else(short)?);
    }
    for row in 0..visible_height / 2 {
        let start = luma + row.checked_mul(coded_width).ok_or_else(short)?;
        visible.extend_from_slice(nv12.get(start..start + visible_width).ok_or_else(short)?);
    }
    Ok(Bytes::from(visible))
}

/// Frame over tight NV12 `bytes` at `generation`.
pub(crate) fn emit_frame(
    budget: &SharedDevice,
    bytes: Bytes,
    export: Option<Arc<SharedExport>>,
    active: &Active,
    generation: u64,
) -> Result<D3d12Frame, D3d12Error> {
    let luma = u64::from(active.visible.width()) * u64::from(active.visible.height());
    let y_len = usize::try_from(luma)
        .map_err(|_| D3d12Error::Unsupported(UnsupportedReason::PictureTooLarge))?;
    if bytes.len() != y_len + y_len / 2 {
        return Err(D3d12Error::Unsupported(
            UnsupportedReason::ReadbackShortOfExtent,
        ));
    }
    let identity = GpuFrame::new(active.visible, PixelFormat::Nv12, generation);
    let lease = budget.try_acquire(bytes.len() as u64)?;
    Ok(D3d12Frame::new(
        identity,
        bytes,
        lease,
        FrameOs::wrap(export),
    ))
}
