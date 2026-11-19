//! Hardware decode plus device-resident surfaces behind the caps `Decoder` trait.

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::device::D3d11Device;
use crate::dpb::{Decoded, Dpb, Output};
use crate::error::{D3d11Error, ParseReason, UnsupportedReason};
use crate::frame::{D3d11Frame, FrameExport};
use crate::guid::Codec;
use crate::h264::{Crop, Ingest, Picture};
use crate::inflight::{FramePhases, FrameQueue, PhaseCapture, SplitDecoder, SplitParts};
use crate::pins::SlotPins;
use crate::readback::Readback;
use crate::session::Session;
use crate::session::SharedExport;
use crate::submit::SurfaceFence;

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::delivery::TrySendError;
use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::traits::SharedDevice;
use rust_video_caps::transcode::FallbackCause;
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
    slot: u8,
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

/// H.264 decoder over a D3D11 device.
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
    /// Submitted outputs plus completed frames awaiting poll.
    queue: FrameQueue<D3d11Frame>,
    /// Phase capture switch.
    capture: bool,
    /// Recorded per-frame phases.
    phases: Vec<FramePhases>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
    /// Copy worker behind launches.
    readback: Readback,
}

impl Drop for H264Decoder<'_> {
    fn drop(&mut self) {
        self.readback.shutdown();
    }
}

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
            queue: FrameQueue::new(),
            capture: false,
            phases: Vec::new(),
            finished: false,
            readback: Readback::spawn(),
        }
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs submitted before completion read.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.queue.max_observed()
    }

    /// Queue mechanics over the live session and queue.
    fn parts(&mut self) -> SplitParts<'_> {
        SplitParts::new(
            self.session.as_ref(),
            self.shared.as_ref(),
            &mut self.queue,
            &mut self.generation,
            self.capture,
            &mut self.phases,
            &self.readback,
        )
    }

    /// Pictures of `packet` submitted, without waiting on completion.
    fn submit_packet_inner(&mut self, packet: &Packet) -> Result<(), D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        for picture in &pictures {
            self.submit_picture_split(picture)?;
        }
        Ok(())
    }

    /// Decode `picture`, queueing its outputs for completion reads.
    fn submit_picture_split(&mut self, picture: &Picture) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.parts().drain_all()?;
            self.reconfigure_split(active)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        self.parts().make_room(Some(decoded.submission.slot))?;
        let submit_start = Instant::now();
        let outcome = self.submit_ordered(&decoded);
        let submit_end = Instant::now();
        if let Err(error) = outcome {
            self.reset_stream();
            self.queue.clear_pending();
            return Err(error);
        }
        for output in &decoded.outputs {
            self.queue
                .push(output.slot(), active, submit_start, submit_end);
        }
        Ok(())
    }

    /// `decoded` submitted plus its outputs copied, in H.264 order.
    fn submit_ordered(&self, decoded: &Decoded) -> Result<(), D3d11Error> {
        let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
        // Submit before any copy: outputs may name the submission slot.
        session.submit(&decoded.submission)?;
        for output in &decoded.outputs {
            session.copy_slot(output.slot())?;
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

    /// Session for `active`, draining the previous sequence into ready first.
    fn reconfigure_split(&mut self, active: Active) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&previous, &mut drained)?;
            self.queue.extend_ready(drained);
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
        let Some(dpb) = self.dpb.as_mut() else {
            return Ok(());
        };
        let slots: Vec<u8> = dpb.drain().iter().map(Output::slot).collect();
        let observed = slots.len();
        self.parts().read_live(active, &slots, frames)?;
        self.queue.observe(observed);
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
        let pins = self
            .dpb
            .as_ref()
            .ok_or(D3d11Error::NoSession)?
            .pins()
            .clone();
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any surface handoff: outputs may name the submission slot.
            if let Err(error) = session.submit(&decoded.submission) {
                failed = Some(error);
            } else if let Err(error) = session.copy_slot(decoded.submission.slot) {
                failed = Some(error);
            } else {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active, &pins) else {
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
        let pins = dpb.pins().clone();
        for output in dpb.drain() {
            let Some(surface) = emit_surface(session, &output, active, &pins) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                    cause: FallbackCause::NoSharedExport,
                }));
            };
            surfaces.push(surface);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the picture fails to upload or convert.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut surfaces = Vec::new();
        for picture in &pictures {
            self.surface_picture(picture, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the trailing picture fails to convert.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
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

impl Decoder for H264Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Err(error) = self.submit_packet_inner(packet) {
            self.queue.clear();
            return Err(error);
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture_split(&picture) {
                self.queue.clear();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

impl SplitDecoder for H264Decoder<'_> {
    type Frame = D3d11Frame;

    fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        self.parts()
            .sweep_ready()
            .map_err(|error| TrySendError::Failed { error })?;
        if !self.queue.admits() {
            return Err(TrySendError::Full { packet });
        }
        self.submit_packet_inner(&packet)
            .map_err(|error| TrySendError::Failed { error })
    }

    fn poll_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        self.parts().sweep_ready()?;
        Ok(self.queue.pop_ready())
    }

    fn wait_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        loop {
            self.parts().sweep_ready()?;
            if let Some(frame) = self.queue.pop_ready() {
                return Ok(Some(frame));
            }
            if self.finished && self.queue.is_drained() {
                return Ok(None);
            }
            if self.queue.front().is_some() {
                self.parts().drain_oldest()?;
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_input(&mut self) -> Result<(), D3d11Error> {
        if self.finished {
            return Ok(());
        }
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture_split(&picture) {
                self.queue.clear_pending();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear_pending();
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&active, &mut drained)?;
            self.queue.extend_ready(drained);
        }
        self.finished = true;
        Ok(())
    }

    fn depth(&self) -> usize {
        self.queue.pending()
    }

    fn max_depth(&self) -> usize {
        self.queue.max_observed()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

impl PhaseCapture for H264Decoder<'_> {
    fn capture_phases(&mut self, on: bool) {
        self.capture = on;
    }

    fn phases(&self) -> &[FramePhases] {
        &self.phases
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
fn emit_surface(
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

/// HEVC decoder over a D3D11 device.
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
    /// Submitted outputs plus completed frames awaiting poll.
    queue: FrameQueue<D3d11Frame>,
    /// Phase capture switch.
    capture: bool,
    /// Recorded per-frame phases.
    phases: Vec<FramePhases>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
    /// Copy worker behind launches.
    readback: Readback,
}

impl Drop for HevcDecoder<'_> {
    fn drop(&mut self) {
        self.readback.shutdown();
    }
}

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
            queue: FrameQueue::new(),
            capture: false,
            phases: Vec::new(),
            finished: false,
            readback: Readback::spawn(),
        }
    }

    /// Stream resets after mid-picture failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs submitted before completion read.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.queue.max_observed()
    }

    /// Queue mechanics over the live session and queue.
    fn parts(&mut self) -> SplitParts<'_> {
        SplitParts::new(
            self.session.as_ref(),
            self.shared.as_ref(),
            &mut self.queue,
            &mut self.generation,
            self.capture,
            &mut self.phases,
            &self.readback,
        )
    }

    /// Pictures of `packet` submitted, without waiting on completion.
    fn submit_packet_inner(&mut self, packet: &Packet) -> Result<(), D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        for picture in &pictures {
            self.submit_picture_split(picture)?;
        }
        Ok(())
    }

    /// Decode `picture`, queueing its outputs for completion reads.
    fn submit_picture_split(&mut self, picture: &crate::h265::Picture) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.parts().drain_all()?;
            self.reconfigure_split(active)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        self.parts().make_room(Some(decoded.submission.slot))?;
        let submit_start = Instant::now();
        let outcome = self.submit_ordered(&decoded);
        let submit_end = Instant::now();
        if let Err(error) = outcome {
            self.reset_stream();
            self.queue.clear_pending();
            return Err(error);
        }
        for output in &decoded.outputs {
            self.queue
                .push(output.slot(), active, submit_start, submit_end);
        }
        Ok(())
    }

    /// `decoded` submitted plus its outputs copied, in HEVC order.
    fn submit_ordered(&self, decoded: &crate::hevc_dpb::HevcDecoded) -> Result<(), D3d11Error> {
        let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
        // Priors precede the submit: it overwrites the submission
        // slot, which evicted priors may still name. The current
        // picture alone follows it.
        for (index, output) in decoded.outputs.iter().enumerate() {
            if Some(index) == decoded.current_index {
                continue;
            }
            session.copy_slot(output.slot())?;
        }
        session.submit_hevc(&decoded.submission)?;
        if let Some(index) = decoded.current_index {
            if let Some(output) = decoded.outputs.get(index) {
                session.copy_slot(output.slot())?;
            }
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

    /// Session for `active`, draining the previous sequence into ready first.
    fn reconfigure_split(&mut self, active: Active) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&previous, &mut drained)?;
            self.queue.extend_ready(drained);
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
        let crop = sps.crop.map(|crop| Crop {
            left: crop.left,
            right: crop.right,
            top: crop.top,
            bottom: crop.bottom,
        });
        Active::new(
            sps.id,
            sps.width,
            sps.height,
            crop,
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
        let Some(dpb) = self.dpb.as_mut() else {
            return Ok(());
        };
        let slots: Vec<u8> = dpb.drain().iter().map(Output::slot).collect();
        let observed = slots.len();
        self.parts().read_live(active, &slots, frames)?;
        self.queue.observe(observed);
        Ok(())
    }

    /// Decode `picture` into GPU `surfaces`.
    fn surface_picture(
        &mut self,
        picture: &crate::h265::Picture,
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
        picture: &crate::h265::Picture,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let active = self.sequence(picture)?;
        if self.active != Some(active) {
            self.reconfigure_surface(active, surfaces)?;
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(picture) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let pins = self
            .dpb
            .as_ref()
            .ok_or(D3d11Error::NoSession)?
            .pins()
            .clone();
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any surface handoff: outputs may name the submission slot.
            if let Err(error) = session.submit_hevc(&decoded.submission) {
                failed = Some(error);
            } else if let Err(error) = session.copy_slot(decoded.submission.slot) {
                failed = Some(error);
            } else {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active, &pins) else {
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
        let pins = dpb.pins().clone();
        for output in dpb.drain() {
            let Some(surface) = emit_surface(session, &output, active, &pins) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                    cause: FallbackCause::NoSharedExport,
                }));
            };
            surfaces.push(surface);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the picture fails to upload or convert.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        let pictures = self.ingest.ingest_packet(packet.data())?;
        let mut surfaces = Vec::new();
        for picture in &pictures {
            self.surface_picture(picture, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the trailing picture fails to convert.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
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

impl Decoder for HevcDecoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Err(error) = self.submit_packet_inner(packet) {
            self.queue.clear();
            return Err(error);
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture_split(&picture) {
                self.queue.clear();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

impl SplitDecoder for HevcDecoder<'_> {
    type Frame = D3d11Frame;

    fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        self.parts()
            .sweep_ready()
            .map_err(|error| TrySendError::Failed { error })?;
        if !self.queue.admits() {
            return Err(TrySendError::Full { packet });
        }
        self.submit_packet_inner(&packet)
            .map_err(|error| TrySendError::Failed { error })
    }

    fn poll_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        self.parts().sweep_ready()?;
        Ok(self.queue.pop_ready())
    }

    fn wait_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        loop {
            self.parts().sweep_ready()?;
            if let Some(frame) = self.queue.pop_ready() {
                return Ok(Some(frame));
            }
            if self.finished && self.queue.is_drained() {
                return Ok(None);
            }
            if self.queue.front().is_some() {
                self.parts().drain_oldest()?;
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_input(&mut self) -> Result<(), D3d11Error> {
        if self.finished {
            return Ok(());
        }
        if let Some(picture) = self.ingest.finish()? {
            if let Err(error) = self.submit_picture_split(&picture) {
                self.queue.clear_pending();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear_pending();
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&active, &mut drained)?;
            self.queue.extend_ready(drained);
        }
        self.finished = true;
        Ok(())
    }

    fn depth(&self) -> usize {
        self.queue.pending()
    }

    fn max_depth(&self) -> usize {
        self.queue.max_observed()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

impl PhaseCapture for HevcDecoder<'_> {
    fn capture_phases(&mut self, on: bool) {
        self.capture = on;
    }

    fn phases(&self) -> &[FramePhases] {
        &self.phases
    }
}

/// AV1 decoder over a D3D11 device.
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
    /// Submitted outputs plus completed frames awaiting poll.
    queue: FrameQueue<D3d11Frame>,
    /// Phase capture switch.
    capture: bool,
    /// Recorded per-frame phases.
    phases: Vec<FramePhases>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
    /// Copy worker behind launches.
    readback: Readback,
}

impl Drop for Av1Decoder<'_> {
    fn drop(&mut self) {
        self.readback.shutdown();
    }
}

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
            queue: FrameQueue::new(),
            capture: false,
            phases: Vec::new(),
            finished: false,
            readback: Readback::spawn(),
        }
    }

    /// Stream resets after mid-frame failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs submitted before completion read.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.queue.max_observed()
    }

    /// Queue mechanics over the live session and queue.
    fn parts(&mut self) -> SplitParts<'_> {
        SplitParts::new(
            self.session.as_ref(),
            self.shared.as_ref(),
            &mut self.queue,
            &mut self.generation,
            self.capture,
            &mut self.phases,
            &self.readback,
        )
    }

    /// Frames of `packet` submitted, without waiting on completion.
    fn submit_packet_inner(&mut self, packet: &Packet) -> Result<(), D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        for frame in &frames_in {
            self.submit_frame_split(frame)?;
        }
        Ok(())
    }

    /// Decode `frame`, queueing its outputs for completion reads.
    fn submit_frame_split(&mut self, frame: &crate::av1::Frame) -> Result<(), D3d11Error> {
        let active = self.frame_active(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.parts().drain_all()?;
            self.reconfigure_split(active)?;
        } else if live != Some(active) {
            self.active = Some(active);
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        let slot = decoded
            .submission
            .as_ref()
            .map(|submission| submission.slot);
        self.parts().make_room(slot)?;
        let submit_start = Instant::now();
        let outcome = self.submit_ordered(&decoded);
        let submit_end = Instant::now();
        if let Err(error) = outcome {
            self.reset_stream();
            self.queue.clear_pending();
            return Err(error);
        }
        for output in &decoded.outputs {
            self.queue
                .push(output.slot(), active, submit_start, submit_end);
        }
        Ok(())
    }

    /// `decoded` submitted plus its outputs copied, in AV1 order.
    fn submit_ordered(&self, decoded: &crate::av1_dpb::Av1Decoded) -> Result<(), D3d11Error> {
        let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
        // Priors precede the submit: it overwrites the submission
        // view, which evicted priors may still name. The current
        // frame alone follows it.
        for (index, output) in decoded.outputs.iter().enumerate() {
            if Some(index) == decoded.current_index {
                continue;
            }
            session.copy_slot(output.slot())?;
        }
        if let Some(submission) = decoded.submission.as_ref() {
            session.submit_av1(submission)?;
        }
        if let Some(index) = decoded.current_index {
            if let Some(output) = decoded.outputs.get(index) {
                session.copy_slot(output.slot())?;
            }
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

    /// Session for `active`, draining the previous sequence into ready first.
    fn reconfigure_split(&mut self, active: Active) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&previous, &mut drained)?;
            self.queue.extend_ready(drained);
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
        let slots: Vec<u8> = crate::av1_dpb::Av1Dpb::drain()
            .iter()
            .map(Output::slot)
            .collect();
        let observed = slots.len();
        self.parts().read_live(active, &slots, frames)?;
        self.queue.observe(observed);
        Ok(())
    }

    /// Decode `frame` into GPU `surfaces`.
    fn surface_frame(
        &mut self,
        frame: &crate::av1::Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let outcome = self.surface_inner(frame, surfaces);
        if outcome.is_err() {
            self.reset_stream();
        }
        outcome
    }

    /// Decode `frame` into GPU `surfaces` without resetting on failure.
    fn surface_inner(
        &mut self,
        frame: &crate::av1::Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let active = self.frame_active(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.reconfigure_surface(active, surfaces)?;
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
        let pins = self
            .dpb
            .as_ref()
            .ok_or(D3d11Error::NoSession)?
            .pins()
            .clone();
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any surface handoff: outputs may name the submission view.
            if let Some(submission) = decoded.submission.as_ref() {
                if let Err(error) = session.submit_av1(submission) {
                    failed = Some(error);
                } else if let Err(error) = session.copy_slot(submission.slot) {
                    failed = Some(error);
                }
            }
            if failed.is_none() {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active, &pins) else {
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
        let (Some(dpb), Some(session)) = (self.dpb.as_ref(), self.session.as_ref()) else {
            return Ok(());
        };
        let pins = dpb.pins().clone();
        for output in crate::av1_dpb::Av1Dpb::drain() {
            let Some(surface) = emit_surface(session, &output, active, &pins) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                    cause: FallbackCause::NoSharedExport,
                }));
            };
            surfaces.push(surface);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the frame fails to upload or convert.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut surfaces = Vec::new();
        for frame in &frames_in {
            self.surface_frame(frame, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the trailing frame fails to convert.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        let mut surfaces = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            self.surface_frame(&frame, &mut surfaces)?;
        }
        if let Some(active) = self.active {
            self.drain_live_surface(&active, &mut surfaces)?;
        }
        Ok(surfaces)
    }
}

impl Decoder for Av1Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Err(error) = self.submit_packet_inner(packet) {
            self.queue.clear();
            return Err(error);
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                self.queue.clear();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame_split(&frame) {
                self.queue.clear();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

impl SplitDecoder for Av1Decoder<'_> {
    type Frame = D3d11Frame;

    fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        self.parts()
            .sweep_ready()
            .map_err(|error| TrySendError::Failed { error })?;
        if !self.queue.admits() {
            return Err(TrySendError::Full { packet });
        }
        self.submit_packet_inner(&packet)
            .map_err(|error| TrySendError::Failed { error })
    }

    fn poll_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        self.parts().sweep_ready()?;
        Ok(self.queue.pop_ready())
    }

    fn wait_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        loop {
            self.parts().sweep_ready()?;
            if let Some(frame) = self.queue.pop_ready() {
                return Ok(Some(frame));
            }
            if self.finished && self.queue.is_drained() {
                return Ok(None);
            }
            if self.queue.front().is_some() {
                self.parts().drain_oldest()?;
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_input(&mut self) -> Result<(), D3d11Error> {
        if self.finished {
            return Ok(());
        }
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame_split(&frame) {
                self.queue.clear_pending();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear_pending();
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&active, &mut drained)?;
            self.queue.extend_ready(drained);
        }
        self.finished = true;
        Ok(())
    }

    fn depth(&self) -> usize {
        self.queue.pending()
    }

    fn max_depth(&self) -> usize {
        self.queue.max_observed()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

impl PhaseCapture for Av1Decoder<'_> {
    fn capture_phases(&mut self, on: bool) {
        self.capture = on;
    }

    fn phases(&self) -> &[FramePhases] {
        &self.phases
    }
}

/// VP9 decoder over a D3D11 device.
pub struct Vp9Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d11Device,
    /// Frame ingest: uncompressed headers plus tile assembly.
    ingest: crate::vp9::Ingest,
    /// Live reference slots, created from the first frame.
    dpb: Option<crate::vp9_dpb::Vp9Dpb>,
    /// Live decode session, created from the first frame.
    session: Option<Session>,
    /// VRAM budget sized to the adapter memory.
    shared: Option<SharedDevice>,
    /// Sequence behind the live session.
    active: Option<Active>,
    /// Next frame generation number.
    generation: u64,
    /// Stream resets after mid-frame failures.
    resets: u64,
    /// Submitted outputs plus completed frames awaiting poll.
    queue: FrameQueue<D3d11Frame>,
    /// Phase capture switch.
    capture: bool,
    /// Recorded per-frame phases.
    phases: Vec<FramePhases>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
    /// Copy worker behind launches.
    readback: Readback,
}

impl Drop for Vp9Decoder<'_> {
    fn drop(&mut self) {
        self.readback.shutdown();
    }
}

impl<'a> Vp9Decoder<'a> {
    /// Decoder over `device`; the session starts on the first frame.
    #[must_use]
    pub fn open(device: &'a D3d11Device) -> Self {
        Self {
            device,
            ingest: crate::vp9::Ingest::new(),
            dpb: None,
            session: None,
            shared: None,
            active: None,
            generation: 0,
            resets: 0,
            queue: FrameQueue::new(),
            capture: false,
            phases: Vec::new(),
            finished: false,
            readback: Readback::spawn(),
        }
    }

    /// Stream resets after mid-frame failures.
    #[must_use]
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Peak outputs submitted before completion read.
    #[must_use]
    pub fn max_inflight(&self) -> usize {
        self.queue.max_observed()
    }

    /// Queue mechanics over the live session and queue.
    fn parts(&mut self) -> SplitParts<'_> {
        SplitParts::new(
            self.session.as_ref(),
            self.shared.as_ref(),
            &mut self.queue,
            &mut self.generation,
            self.capture,
            &mut self.phases,
            &self.readback,
        )
    }

    /// Frames of `packet` submitted, without waiting on completion.
    fn submit_packet_inner(&mut self, packet: &Packet) -> Result<(), D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        for frame in &frames_in {
            self.submit_frame_split(frame)?;
        }
        Ok(())
    }

    /// Decode `frame`, queueing its outputs for completion reads.
    fn submit_frame_split(&mut self, frame: &crate::vp9::Frame) -> Result<(), D3d11Error> {
        let active = self.frame_active(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.parts().drain_all()?;
            self.reconfigure_split(active)?;
        } else if live != Some(active) {
            self.active = Some(active);
        }
        let active = self.active.ok_or(D3d11Error::NoSession)?;
        let decoded = match self.decode_dpb(frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        let slot = decoded
            .submission
            .as_ref()
            .map(|submission| submission.slot);
        self.parts().make_room(slot)?;
        let submit_start = Instant::now();
        let outcome = self.submit_ordered(&decoded);
        let submit_end = Instant::now();
        if let Err(error) = outcome {
            self.reset_stream();
            self.queue.clear_pending();
            return Err(error);
        }
        for output in &decoded.outputs {
            self.queue
                .push(output.slot(), active, submit_start, submit_end);
        }
        Ok(())
    }

    /// `decoded` submitted plus its outputs copied, in VP9 order.
    fn submit_ordered(&self, decoded: &crate::vp9_dpb::Vp9Decoded) -> Result<(), D3d11Error> {
        let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
        // Priors precede the submit: it overwrites the submission
        // view, which evicted priors may still name. The current
        // frame alone follows it.
        for (index, output) in decoded.outputs.iter().enumerate() {
            if Some(index) == decoded.current_index {
                continue;
            }
            session.copy_slot(output.slot())?;
        }
        if let Some(submission) = decoded.submission.as_ref() {
            session.submit_vp9(submission)?;
        }
        if let Some(index) = decoded.current_index {
            if let Some(output) = decoded.outputs.get(index) {
                session.copy_slot(output.slot())?;
            }
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

    /// Session for `active`, draining the previous sequence into ready first.
    fn reconfigure_split(&mut self, active: Active) -> Result<(), D3d11Error> {
        if let Some(previous) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&previous, &mut drained)?;
            self.queue.extend_ready(drained);
        }
        self.open_session(active)
    }

    /// Session plus reference slots for `active`.
    fn open_session(&mut self, active: Active) -> Result<(), D3d11Error> {
        let session = Session::open_codec(self.device, Codec::Vp9, active.width, active.height)?;
        self.shared = Some(self.device.shared().clone());
        self.session = Some(session);
        self.dpb = Some(crate::vp9_dpb::Vp9Dpb::new());
        self.active = Some(active);
        Ok(())
    }

    /// Active sequence behind `frame`'s header.
    fn frame_active(&self, frame: &crate::vp9::Frame) -> Result<Active, D3d11Error> {
        let header = &frame.header;
        if header.show_existing_frame() {
            return self.active.ok_or(D3d11Error::NoSession);
        }
        Active::new(
            0,
            header.frame_width,
            header.frame_height,
            None,
            header.frame_width,
            header.frame_height,
        )
    }

    /// Submission plus display-order outputs for `frame`.
    fn decode_dpb(
        &mut self,
        frame: &crate::vp9::Frame,
    ) -> Result<crate::vp9_dpb::Vp9Decoded, D3d11Error> {
        self.dpb
            .as_mut()
            .ok_or(D3d11Error::NoSession)?
            .decode(frame)
    }

    /// Live DPB outputs into `frames`.
    fn drain_live(
        &mut self,
        active: &Active,
        frames: &mut Vec<D3d11Frame>,
    ) -> Result<(), D3d11Error> {
        let slots: Vec<u8> = crate::vp9_dpb::Vp9Dpb::drain()
            .iter()
            .map(Output::slot)
            .collect();
        let observed = slots.len();
        self.parts().read_live(active, &slots, frames)?;
        self.queue.observe(observed);
        Ok(())
    }

    /// Decode `frame` into GPU `surfaces`.
    fn surface_frame(
        &mut self,
        frame: &crate::vp9::Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let outcome = self.surface_inner(frame, surfaces);
        if outcome.is_err() {
            self.reset_stream();
        }
        outcome
    }

    /// Decode `frame` into GPU `surfaces` without resetting on failure.
    fn surface_inner(
        &mut self,
        frame: &crate::vp9::Frame,
        surfaces: &mut Vec<GpuSurface>,
    ) -> Result<(), D3d11Error> {
        let active = self.frame_active(frame)?;
        let live = self.active;
        if live.is_none_or(|live| (live.width, live.height) != (active.width, active.height)) {
            self.reconfigure_surface(active, surfaces)?;
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
        let pins = self
            .dpb
            .as_ref()
            .ok_or(D3d11Error::NoSession)?
            .pins()
            .clone();
        let mut failed = None;
        {
            let session = self.session.as_ref().ok_or(D3d11Error::NoSession)?;
            // Submit before any surface handoff: outputs may name the submission view.
            if let Some(submission) = decoded.submission.as_ref() {
                if let Err(error) = session.submit_vp9(submission) {
                    failed = Some(error);
                } else if let Err(error) = session.copy_slot(submission.slot) {
                    failed = Some(error);
                }
            }
            if failed.is_none() {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active, &pins) else {
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
        let (Some(dpb), Some(session)) = (self.dpb.as_ref(), self.session.as_ref()) else {
            return Ok(());
        };
        let pins = dpb.pins().clone();
        for output in crate::vp9_dpb::Vp9Dpb::drain() {
            let Some(surface) = emit_surface(session, &output, active, &pins) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Fallback {
                    cause: FallbackCause::NoSharedExport,
                }));
            };
            surfaces.push(surface);
        }
        Ok(())
    }

    /// GPU surfaces decoded from `packet`, possibly none.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the frame fails to upload or convert.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        let mut surfaces = Vec::new();
        for frame in &frames_in {
            self.surface_frame(frame, &mut surfaces)?;
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the trailing frame fails to convert.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        let mut surfaces = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            self.surface_frame(&frame, &mut surfaces)?;
        }
        if let Some(active) = self.active {
            self.drain_live_surface(&active, &mut surfaces)?;
        }
        Ok(surfaces)
    }
}

impl Decoder for Vp9Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Err(error) = self.submit_packet_inner(packet) {
            self.queue.clear();
            return Err(error);
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn flush(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                self.queue.clear();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame_split(&frame) {
                self.queue.clear();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear();
            self.reset_stream();
            return Err(error);
        }
        self.parts().take_ready(&mut frames);
        if let Some(active) = self.active {
            self.drain_live(&active, &mut frames)?;
        }
        Ok(frames)
    }
}

impl SplitDecoder for Vp9Decoder<'_> {
    type Frame = D3d11Frame;

    fn submit_packet(&mut self, packet: Packet) -> Result<(), TrySendError<D3d11Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        self.parts()
            .sweep_ready()
            .map_err(|error| TrySendError::Failed { error })?;
        if !self.queue.admits() {
            return Err(TrySendError::Full { packet });
        }
        self.submit_packet_inner(&packet)
            .map_err(|error| TrySendError::Failed { error })
    }

    fn poll_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        self.parts().sweep_ready()?;
        Ok(self.queue.pop_ready())
    }

    fn wait_frame(&mut self) -> Result<Option<D3d11Frame>, D3d11Error> {
        loop {
            self.parts().sweep_ready()?;
            if let Some(frame) = self.queue.pop_ready() {
                return Ok(Some(frame));
            }
            if self.finished && self.queue.is_drained() {
                return Ok(None);
            }
            if self.queue.front().is_some() {
                self.parts().drain_oldest()?;
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_input(&mut self) -> Result<(), D3d11Error> {
        if self.finished {
            return Ok(());
        }
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            if let Err(error) = self.submit_frame_split(&frame) {
                self.queue.clear_pending();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.queue.clear_pending();
            self.reset_stream();
            return Err(error);
        }
        if let Some(active) = self.active {
            let mut drained = Vec::new();
            self.drain_live(&active, &mut drained)?;
            self.queue.extend_ready(drained);
        }
        self.finished = true;
        Ok(())
    }

    fn depth(&self) -> usize {
        self.queue.pending()
    }

    fn max_depth(&self) -> usize {
        self.queue.max_observed()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }
}

impl PhaseCapture for Vp9Decoder<'_> {
    fn capture_phases(&mut self, on: bool) {
        self.capture = on;
    }

    fn phases(&self) -> &[FramePhases] {
        &self.phases
    }
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

    #[test]
    fn pinned_surface_keeps_pixels_under_pressure() {
        use rust_video_caps::traits::Encoder;

        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let Ok(mut encoder) = crate::encoder::D3d11Encoder::open(&device, crate::guid::Codec::H264)
        else {
            return;
        };
        let mut decoder = super::H264Decoder::open(&device);
        let first = rust_video_harness::Synth::noise(320, 240, 0);
        let mut held = None;
        for packet in encoder.encode(&first).expect("encode runs") {
            let mut surfaces = decoder.decode_surface(&packet).expect("decode runs");
            if held.is_none() {
                held = surfaces.pop();
            }
        }
        let held = held.expect("first picture surfaces");
        let slot = held.slot;
        let crop = decoder.active.expect("live sequence").crop;
        decoder
            .session
            .as_ref()
            .expect("live session")
            .copy_slot(slot)
            .expect("copy runs");
        let before = decoder
            .session
            .as_ref()
            .expect("live session")
            .read_slot(slot, crop)
            .expect("read runs");
        for seed in 1..20u64 {
            let frame = rust_video_harness::Synth::noise(320, 240, seed);
            for packet in encoder.encode(&frame).expect("encode runs") {
                decoder.decode_surface(&packet).expect("decode runs");
            }
        }
        for packet in encoder.flush().expect("encode drains") {
            decoder.decode_surface(&packet).expect("decode runs");
        }
        decoder.flush_surface().expect("decode drains");
        decoder
            .session
            .as_ref()
            .expect("live session")
            .copy_slot(slot)
            .expect("copy runs");
        let after = decoder
            .session
            .as_ref()
            .expect("live session")
            .read_slot(slot, crop)
            .expect("read runs");
        assert_eq!(before.as_ref(), after.as_ref());
    }
}
