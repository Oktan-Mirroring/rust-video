//! H.264 decode plus probed shells behind the caps `Decoder` trait.

use crate::device::D3d11Device;
#[cfg(target_os = "windows")]
use crate::dpb::{Decoded, Dpb, Output};
use crate::error::{D3d11Error, ParseReason, SkipReason, UnsupportedReason};
use crate::guid::Codec;
#[cfg(target_os = "windows")]
use crate::h264::{Crop, Ingest, Picture};
#[cfg(target_os = "windows")]
use crate::session::Session;
#[cfg(target_os = "windows")]
use crate::session::SharedExport;

use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::SharedDevice;
use rust_video_caps::traits::{Decoder, Lease};
#[cfg(target_os = "windows")]
use rust_video_caps::transcode::FallbackCause;

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct D3d11Frame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
    /// Shared-texture export, absent without NT-handle sharing.
    #[cfg(target_os = "windows")]
    shared: Option<SharedExport>,
}

impl D3d11Frame {
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
    /// NT handle over the shared decode texture, or an opaque backend
    /// descriptor when the driver declines sharing.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        #[cfg(target_os = "windows")]
        if let Some(shared) = &self.shared {
            return GpuExport::NtHandle {
                handle: shared.handle(),
            };
        }
        GpuExport::Opaque { name: "d3d11" }
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

/// Device-resident decode surface without CPU access.
#[cfg(target_os = "windows")]
pub(crate) struct GpuSurface {
    /// Shared decode texture plus its NT handle.
    export: SharedExport,
    /// Two-plane export description behind the surface.
    layout: ExportLayout,
}

#[cfg(target_os = "windows")]
impl GpuSurface {
    /// Picture dimensions in luma samples.
    #[must_use]
    pub(crate) fn size(&self) -> PictureSize {
        self.layout.visible()
    }

    /// Shared decode texture plus its NT handle.
    #[must_use]
    pub(crate) fn export(&self) -> &SharedExport {
        &self.export
    }

    /// Two-plane export description behind the surface.
    #[must_use]
    pub(crate) fn layout(&self) -> &ExportLayout {
        &self.layout
    }
}

/// NV12 two-plane export description over one array slice.
#[cfg(target_os = "windows")]
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
}

#[cfg(target_os = "windows")]
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
        PictureSize::new(coded_width, coded_height)?;
        let (left, top) = crop.map_or((0, 0), |crop| (crop.left, crop.top));
        Some(Self {
            format: PixelFormat::Nv12,
            slice: slot,
            left,
            top,
            visible,
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
}

/// Pending outputs behind a mid-call consume.
#[cfg(target_os = "windows")]
const PENDING_FLUSH: usize = 8;

/// Active sequence behind the live session.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Active {
    /// Sequence set id.
    sps_id: u8,
    /// Coded width in luma samples.
    width: u32,
    /// Coded height in luma samples.
    height: u32,
    /// Visible rectangle, absent when uncropped.
    crop: Option<Crop>,
    /// Visible size in luma samples.
    visible: PictureSize,
}

#[cfg(target_os = "windows")]
impl Active {
    /// Active sequence for `sps_id` at `width` x `height` with `crop` and visible extent.
    fn new(
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
}

/// H.264 decoder over a D3D11 device.
#[cfg(target_os = "windows")]
pub struct H264Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d11Device,
    /// Annex B ingest: sets plus picture assembly.
    ingest: Ingest,
    /// Live picture buffer, created from the first sequence set.
    dpb: Option<Dpb>,
    /// Live decode session, created from the first sequence set.
    session: Option<Session>,
    /// VRAM budget sized to the adapter memory.
    shared: Option<SharedDevice>,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-picture failures.
    resets: u64,
    /// Peak outputs queued before readback in one call.
    inflight_max: usize,
}

#[cfg(target_os = "windows")]
impl<'a> H264Decoder<'a> {
    /// Decoder over `device`; the session starts on the first picture.
    #[must_use]
    pub fn open(device: &'a D3d11Device) -> Self {
        Self {
            device,
            ingest: Ingest::new(),
            dpb: None,
            session: None,
            shared: None,
            active: None,
            generation: 0,
            resets: 0,
            inflight_max: 0,
        }
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs queued before readback in one call.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.inflight_max
    }

    /// Decode `picture`, queueing its outputs for readback into `pending`.
    fn submit_picture(
        &mut self,
        picture: &Picture,
        pending: &mut Vec<(u8, Active)>,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.consume_pending(pending, frames)?;
            pending.clear();
            self.reconfigure(active, frames)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = self.decode_dpb(picture);
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any copy: outputs may name the submission slot.
            if let Err(error) = session.submit(&decoded.submission) {
                failed = Some(error);
            } else {
                for output in &decoded.outputs {
                    if let Err(error) = session.copy_slot(output.slot()) {
                        failed = Some(error);
                        break;
                    }
                    pending.push((output.slot(), active));
                }
            }
        }
        if let Some(error) = failed {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// Readback for queued `pending` outputs into `frames`.
    fn consume_pending(
        &mut self,
        pending: &[(u8, Active)],
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let (Some(session), Some(shared)) = (self.session.as_ref(), self.shared.as_ref()) else {
            return Err(D3d11Error::NoSession);
        };
        for (slot, active) in pending {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, buffer, and sequence after a mid-picture failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.shared = None;
        self.active = None;
        self.resets = self.resets.wrapping_add(1);
    }

    /// Peak outputs queued before readback against `depth`.
    fn note_inflight(&mut self, depth: usize) {
        self.inflight_max = self.inflight_max.max(depth);
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(
        &mut self,
        active: Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            self.drain_live(&previous, frames)?;
        }
        self.open_session(active)
    }

    /// Session plus picture buffer for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d11Error> {
        let session = Session::open(self.device, active.width, active.height)?;
        self.shared = Some(self.device.shared().clone());
        self.session = Some(session);
        let sps = self
            .ingest
            .sps(active.sps_id)
            .ok_or(D3d11Error::NoSession)?;
        self.dpb = Some(Dpb::new(sps));
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `picture`'s parameter sets.
    fn sequence(&self, picture: &Picture) -> Result<Active, D3d11Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d11Error::NoSession)?;
        let sps = self
            .ingest
            .sps(pps.seq_parameter_set_id)
            .ok_or(D3d11Error::NoSession)?;
        Active::new(
            sps.seq_parameter_set_id,
            sps.width(),
            sps.height(),
            sps.crop,
            sps.visible_width(),
            sps.visible_height(),
        )
    }

    /// Submission plus display-order outputs for `picture`.
    fn decode_dpb(&mut self, picture: &Picture) -> Result<Decoded, D3d11Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d11Error::NoSession)?;
        let sps = self
            .ingest
            .sps(pps.seq_parameter_set_id)
            .ok_or(D3d11Error::NoSession)?;
        self.dpb
            .as_mut()
            .ok_or(D3d11Error::NoSession)?
            .decode(picture, sps, pps)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let (Some(dpb), Some(session), Some(shared)) = (
            self.dpb.as_mut(),
            self.session.as_ref(),
            self.shared.as_ref(),
        ) else {
            return Ok(());
        };
        let slots: Vec<u8> = dpb.drain().iter().map(Output::slot).collect();
        for slot in &slots {
            session.copy_slot(*slot)?;
        }
        for slot in &slots {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        self.note_inflight(slots.len());
        Ok(())
    }

    /// Decode `picture` into GPU `surfaces`.
    fn surface_picture(
        &mut self,
        picture: &Picture,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let outcome = self.surface_inner(picture, surfaces);
        if outcome.is_err() {
            self.reset_stream();
        }
        outcome
    }

    /// Decode `picture` into GPU `surfaces` without resetting on failure.
    fn surface_inner(
        &mut self,
        picture: &Picture,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.reconfigure_surface(active, surfaces)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = self.decode_dpb(picture);
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any surface handoff: outputs may name the submission slot.
            if let Err(error) = session.submit(&decoded.submission) {
                failed = Some(error);
            } else {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active) else {
                        failed = Some(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                            cause: FallbackCause::NoSharedExport,
                        }));
                        break;
                    };
                    surfaces.push(surface);
                }
            }
        }
        if let Some(error) = failed {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure_surface(
        &mut self,
        active: Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            self.drain_live_surface(&previous, surfaces)?;
        }
        self.open_session(active)
    }

    /// Live DPB outputs into GPU `surfaces`.
    fn drain_live_surface(
        &mut self,
        active: &Active,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_ref()) else {
            return Ok(());
        };
        for output in dpb.drain() {
            let Some(surface) = emit_surface(session, &output, active) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                    cause: FallbackCause::NoSharedExport,
                }));
            };
            surfaces.push(surface);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    pub(crate) fn decode_surface(
        &mut self,
        packet: &Packet,
    ) -> Result<Vec<GpuSurface>, D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut surfaces = Vec::new();
        for picture in &pictures {
            self.surface_picture(picture, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    pub(crate) fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        let mut surfaces = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            self.surface_picture(&picture, &mut surfaces)?;
        }
        if let Some(active) = self.active {
            self.drain_live_surface(&active, &mut surfaces)?;
        }
        Ok(surfaces)
    }
}

#[cfg(target_os = "windows")]
impl Decoder for H264Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for picture in &pictures {
            if let Err(error) = self.submit_picture(picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture(&picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

/// Frame for copied `slot`.
#[cfg(target_os = "windows")]
fn emit_frame(
    session: &Session,
    shared: &SharedDevice,
    slot: u8,
    active: &Active,
    generation: u64,
) -> Result<D3d11Frame, D3d11Error> {
    let data = session.read_slot(slot, active.crop)?;
    let lease = shared.try_acquire(data.len() as u64)?;
    Ok(D3d11Frame {
        identity: GpuFrame::new(active.visible, PixelFormat::Nv12, generation),
        data,
        lease,
        shared: session.export(),
    })
}

/// Surface for `output`, absent without NT-handle sharing.
#[cfg(target_os = "windows")]
fn emit_surface(session: &Session, output: &Output, active: &Active) -> Option<GpuSurface> {
    let (export, slot) = session.surface(output.slot())?;
    let layout = ExportLayout::for_output(
        active.width,
        active.height,
        slot,
        active.crop,
        active.visible,
    )?;
    Some(GpuSurface { export, layout })
}

/// HEVC decoder over a D3D11 device.
#[cfg(target_os = "windows")]
pub struct HevcDecoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d11Device,
    /// Annex B ingest: sets plus picture assembly.
    ingest: crate::h265::Ingest,
    /// Live picture buffer, created from the first sequence set.
    dpb: Option<crate::hevc_dpb::HevcDpb>,
    /// Live decode session, created from the first sequence set.
    session: Option<Session>,
    /// VRAM budget sized to the adapter memory.
    shared: Option<SharedDevice>,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-picture failures.
    resets: u64,
    /// Peak outputs queued before readback in one call.
    inflight_max: usize,
}

#[cfg(target_os = "windows")]
impl<'a> HevcDecoder<'a> {
    /// Decoder over `device`; the session starts on the first picture.
    #[must_use]
    pub fn open(device: &'a D3d11Device) -> Self {
        Self {
            device,
            ingest: crate::h265::Ingest::new(),
            dpb: None,
            session: None,
            shared: None,
            active: None,
            generation: 0,
            resets: 0,
            inflight_max: 0,
        }
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs queued before readback in one call.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.inflight_max
    }

    /// Decode `picture`, queueing its outputs for readback into `pending`.
    fn submit_picture(
        &mut self,
        picture: &crate::h265::Picture,
        pending: &mut Vec<(u8, Active)>,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.consume_pending(pending, frames)?;
            pending.clear();
            self.reconfigure(active, frames)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        // Consume before submit when the submission reuses a slot with
        // unread outputs, or when the queue exceeds the overlap window:
        // staging lanes and the driver queue are both bounded.
        let collision = pending
            .iter()
            .any(|(slot, _)| *slot == decoded.submission.slot);
        if collision || pending.len() >= PENDING_FLUSH {
            self.note_inflight(pending.len());
            if let Err(error) = self.consume_pending(pending, frames) {
                self.reset_stream();
                return Err(error);
            }
            pending.clear();
        }
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Priors precede the submit: it overwrites the submission
            // slot, which evicted priors may still name. The current
            // picture alone follows it.
            for (index, output) in decoded.outputs.iter().enumerate() {
                if Some(index) == decoded.current_index {
                    continue;
                }
                if let Err(error) = session.copy_slot(output.slot()) {
                    failed = Some(error);
                    break;
                }
            }
            if failed.is_none() {
                if let Err(error) = session.submit_hevc(&decoded.submission) {
                    failed = Some(error);
                }
            }
            if failed.is_none() {
                if let Some(index) = decoded.current_index {
                    if let Some(output) = decoded.outputs.get(index) {
                        if let Err(error) = session.copy_slot(output.slot()) {
                            failed = Some(error);
                        }
                    }
                }
            }
            if failed.is_none() {
                for output in &decoded.outputs {
                    pending.push((output.slot(), active));
                }
            }
        }
        if let Some(error) = failed {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// Readback for queued `pending` outputs into `frames`.
    fn consume_pending(
        &mut self,
        pending: &[(u8, Active)],
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let (Some(session), Some(shared)) = (self.session.as_ref(), self.shared.as_ref()) else {
            return Err(D3d11Error::NoSession);
        };
        for (slot, active) in pending {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, buffer, and sequence after a mid-picture failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.shared = None;
        self.active = None;
        self.resets = self.resets.wrapping_add(1);
    }

    /// Peak outputs queued before readback against `depth`.
    fn note_inflight(&mut self, depth: usize) {
        self.inflight_max = self.inflight_max.max(depth);
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(
        &mut self,
        active: Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            self.drain_live(&previous, frames)?;
        }
        self.open_session(active)
    }

    /// Session plus picture buffer for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d11Error> {
        let session = Session::open_codec(self.device, Codec::Hevc, active.width, active.height)?;
        self.shared = Some(self.device.shared().clone());
        self.session = Some(session);
        let sps = self
            .ingest
            .sps(active.sps_id)
            .ok_or(D3d11Error::NoSession)?;
        self.dpb = Some(crate::hevc_dpb::HevcDpb::new(sps));
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `picture`'s parameter sets.
    fn sequence(&self, picture: &crate::h265::Picture) -> Result<Active, D3d11Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d11Error::NoSession)?;
        let sps = self.ingest.sps(pps.sps_id).ok_or(D3d11Error::NoSession)?;
        Active::new(
            sps.id,
            sps.width,
            sps.height,
            sps.crop,
            sps.visible_width(),
            sps.visible_height(),
        )
    }

    /// Submission plus display-order outputs for `picture`.
    fn decode_dpb(
        &mut self,
        picture: &crate::h265::Picture,
    ) -> Result<crate::hevc_dpb::HevcDecoded, D3d11Error> {
        let pps = self
            .ingest
            .pps(picture.pps_id())
            .ok_or(D3d11Error::NoSession)?;
        let sps = self.ingest.sps(pps.sps_id).ok_or(D3d11Error::NoSession)?;
        self.dpb
            .as_mut()
            .ok_or(D3d11Error::NoSession)?
            .decode(picture, sps, pps)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let (Some(dpb), Some(session), Some(shared)) = (
            self.dpb.as_mut(),
            self.session.as_ref(),
            self.shared.as_ref(),
        ) else {
            return Ok(());
        };
        let slots: Vec<u8> = dpb.drain().iter().map(Output::slot).collect();
        for slot in &slots {
            session.copy_slot(*slot)?;
        }
        for slot in &slots {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        self.note_inflight(slots.len());
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Decoder for HevcDecoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for picture in &pictures {
            if let Err(error) = self.submit_picture(picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture(&picture, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

/// AV1 decoder over a D3D11 device.
#[cfg(target_os = "windows")]
pub struct Av1Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d11Device,
    /// OBU ingest: sequence header plus frame assembly.
    ingest: crate::av1::Ingest,
    /// Live reference slots, created from the first sequence header.
    dpb: Option<crate::av1_dpb::Av1Dpb>,
    /// Live decode session, created from the first sequence header.
    session: Option<Session>,
    /// VRAM budget sized to the adapter memory.
    shared: Option<SharedDevice>,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-frame failures.
    resets: u64,
    /// Peak outputs queued before readback in one call.
    inflight_max: usize,
}

#[cfg(target_os = "windows")]
impl<'a> Av1Decoder<'a> {
    /// Decoder over `device`; the session starts on the first frame.
    #[must_use]
    pub fn open(device: &'a D3d11Device) -> Self {
        Self {
            device,
            ingest: crate::av1::Ingest::new(),
            dpb: None,
            session: None,
            shared: None,
            active: None,
            generation: 0,
            resets: 0,
            inflight_max: 0,
        }
    }

    /// Stream resets after mid-frame failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs queued before readback in one call.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.inflight_max
    }

    /// Decode `frame`, queueing its outputs for readback into `pending`.
    fn submit_frame(
        &mut self,
        frame: &crate::av1::Frame,
        pending: &mut Vec<(u8, Active)>,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let active = self.frame_active(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.consume_pending(pending, frames)?;
            pending.clear();
            self.reconfigure(active, frames)?;
        } else if live != Some(active) {
            self.active = Some(active);
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        // Consume before submit when the submission reuses a view with
        // unread outputs, or when the queue exceeds the overlap window:
        // staging lanes and the driver queue are both bounded.
        let collision = decoded
            .submission
            .as_ref()
            .is_some_and(|submission| pending.iter().any(|(slot, _)| *slot == submission.slot));
        if collision || pending.len() >= PENDING_FLUSH {
            self.note_inflight(pending.len());
            if let Err(error) = self.consume_pending(pending, frames) {
                self.reset_stream();
                return Err(error);
            }
            pending.clear();
        }
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Priors precede the submit: it overwrites the submission
            // view, which evicted priors may still name. The current
            // frame alone follows it.
            for (index, output) in decoded.outputs.iter().enumerate() {
                if Some(index) == decoded.current_index {
                    continue;
                }
                if let Err(error) = session.copy_slot(output.slot()) {
                    failed = Some(error);
                    break;
                }
            }
            if failed.is_none() {
                if let Some(submission) = decoded.submission.as_ref() {
                    if let Err(error) = session.submit_av1(submission) {
                        failed = Some(error);
                    }
                }
            }
            if failed.is_none() {
                if let Some(index) = decoded.current_index {
                    if let Some(output) = decoded.outputs.get(index) {
                        if let Err(error) = session.copy_slot(output.slot()) {
                            failed = Some(error);
                        }
                    }
                }
            }
            if failed.is_none() {
                for output in &decoded.outputs {
                    pending.push((output.slot(), active));
                }
            }
        }
        if let Some(error) = failed {
            self.reset_stream();
            return Err(error);
        }
        Ok(())
    }

    /// Readback for queued `pending` outputs into `frames`.
    fn consume_pending(
        &mut self,
        pending: &[(u8, Active)],
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let (Some(session), Some(shared)) = (self.session.as_ref(), self.shared.as_ref()) else {
            return Err(D3d11Error::NoSession);
        };
        for (slot, active) in pending {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        Ok(())
    }

    /// Drop the live session, slots, and sequence after a mid-frame failure.
    fn reset_stream(&mut self) {
        self.dpb = None;
        self.session = None;
        self.shared = None;
        self.active = None;
        self.ingest.reset_refs();
        self.resets = self.resets.wrapping_add(1);
    }

    /// Peak outputs queued before readback against `depth`.
    fn note_inflight(&mut self, depth: usize) {
        self.inflight_max = self.inflight_max.max(depth);
    }

    /// Session for `active`, draining the previous sequence first.
    fn reconfigure(
        &mut self,
        active: Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            self.drain_live(&previous, frames)?;
        }
        self.open_session(active)
    }

    /// Session plus reference slots for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d11Error> {
        let session = Session::open_codec(self.device, Codec::Av1, active.width, active.height)?;
        self.shared = Some(self.device.shared().clone());
        self.session = Some(session);
        self.dpb = Some(crate::av1_dpb::Av1Dpb::new());
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `frame`'s header and sequence header.
    fn frame_active(&self, frame: &crate::av1::Frame) -> Result<Active, D3d11Error> {
        let seq = self.ingest.seq().ok_or(D3d11Error::NoSession)?;
        let header = &frame.header;
        if header.show_existing_frame() {
            return self.active.ok_or(D3d11Error::NoSession);
        }
        let width = seq.max_frame_width();
        let height = seq.max_frame_height();
        if header.frame_width > width || header.frame_height > height {
            return Err(D3d11Error::Parse(
                ParseReason::FrameSizeAboveSequenceMaximum,
            ));
        }
        let crop = if header.frame_width == width && header.frame_height == height {
            None
        } else {
            Some(Crop {
                left: 0,
                right: width - header.frame_width,
                top: 0,
                bottom: height - header.frame_height,
            })
        };
        Active::new(
            0,
            width,
            height,
            crop,
            header.frame_width,
            header.frame_height,
        )
    }

    /// Submission plus display-order outputs for `frame`.
    fn decode_dpb(
        &mut self,
        frame: &crate::av1::Frame,
    ) -> Result<crate::av1_dpb::Av1Decoded, D3d11Error> {
        let seq = self.ingest.seq().ok_or(D3d11Error::NoSession)?;
        self.dpb
            .as_mut()
            .ok_or(D3d11Error::NoSession)?
            .decode(frame, seq)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let (Some(session), Some(shared)) = (self.session.as_ref(), self.shared.as_ref()) else {
            return Ok(());
        };
        let slots: Vec<u8> = crate::av1_dpb::Av1Dpb::drain()
            .iter()
            .map(Output::slot)
            .collect();
        for slot in &slots {
            session.copy_slot(*slot)?;
        }
        for slot in &slots {
            let frame = emit_frame(session, shared, *slot, active, self.generation)?;
            self.generation = self.generation.wrapping_add(1);
            frames.push(frame);
        }
        self.note_inflight(slots.len());
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Decoder for Av1Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        for frame in &frames_in {
            if let Err(error) = self.submit_frame(frame, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        let mut pending = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame(&frame, &mut pending, &mut frames) {
                pending.clear();
                return Err(error);
            }
        }
        self.note_inflight(pending.len());
        if let Err(error) = self.consume_pending(&pending, &mut frames) {
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

/// Probed decoder shell; the picture builder is pending.
macro_rules! shell {
    ($decoder:ident, $codec:ident, $doc:literal) => {
        #[doc = $doc]
        pub struct $decoder<'a> {
            /// Device behind the profile probe.
            device: &'a D3d11Device,
        }

        impl<'a> $decoder<'a> {
            /// Decoder over `device`; decode reports the pending builder.
            #[must_use]
            pub fn open(device: &'a D3d11Device) -> Self {
                Self { device }
            }
        }

        impl Decoder for $decoder<'_> {
            type Frame = D3d11Frame;
            type Error = D3d11Error;

            fn decode(&mut self, _packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
                if !self.device.supports(Codec::$codec) {
                    return Err(SkipReason::ProfileUnsupported {
                        codec: Codec::$codec,
                    }
                    .into());
                }
                Err(D3d11Error::Unsupported(
                    UnsupportedReason::PictureBuilderPending {
                        codec: Codec::$codec,
                    },
                ))
            }

            fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
                Ok(Vec::new())
            }
        }
    };
}

shell!(
    Vp9Decoder,
    Vp9,
    "VP9 decoder; the picture builder is pending."
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

#[cfg(test)]
mod tests {
    use super::D3d11Frame;
    #[cfg(target_os = "windows")]
    use super::ExportLayout;
    #[cfg(target_os = "windows")]
    use crate::h264::Crop;

    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    /// Frame over `data` leased against `shared`.
    fn frame(shared: &SharedDevice, width: u32, height: u32, data: Vec<u8>) -> D3d11Frame {
        let size = PictureSize::new(width, height).expect("nonzero extent");
        let identity = GpuFrame::new(size, PixelFormat::Nv12, 3);
        let bytes = Bytes::from(data);
        let lease = shared
            .try_acquire(bytes.len() as u64)
            .expect("within budget");
        D3d11Frame {
            identity,
            data: bytes,
            lease,
            #[cfg(target_os = "windows")]
            shared: None,
        }
    }

    #[test]
    fn frame_holds_lease_against_budget() {
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![7; 12]);
        assert_eq!(held.lease_bytes(), 12);
        assert_eq!(shared.used(), 12);
        assert_eq!(held.nv12(), &[7; 12]);
        assert_eq!(held.identity().generation(), 3);
    }

    #[test]
    fn dropped_frame_releases_budget() {
        let shared = SharedDevice::new("d3d11", 100);
        {
            let _held = frame(&shared, 4, 2, vec![0; 12]);
            assert_eq!(shared.used(), 12);
        }
        assert_eq!(shared.used(), 0);
    }

    #[test]
    fn export_without_sharing_names_backend() {
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert!(matches!(held.export(), GpuExport::Opaque { name: "d3d11" }));
    }

    #[test]
    fn to_cpu_splits_nv12_planes() {
        let shared = SharedDevice::new("d3d11", 100);
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
        let shared = SharedDevice::new("d3d11", 100);
        let held = frame(&shared, 4, 2, vec![0; 11]);
        assert!(held.to_cpu().is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn layout_names_both_planes() {
        let visible = PictureSize::new(1920, 1080).expect("nonzero extent");
        let layout = ExportLayout::for_output(1920, 1088, 3, None, visible).expect("nonzero coded");
        assert_eq!(layout.format(), PixelFormat::Nv12);
        assert_eq!(layout.format().plane_count(), 2);
        assert_eq!(layout.slice(), 3);
        assert_eq!((layout.left(), layout.top()), (0, 0));
        assert_eq!(layout.visible(), visible);
    }

    #[cfg(target_os = "windows")]
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
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn layout_rejects_empty_coded() {
        let visible = PictureSize::new(1920, 1080).expect("nonzero extent");
        assert!(ExportLayout::for_output(0, 1088, 0, None, visible).is_none());
    }
}
