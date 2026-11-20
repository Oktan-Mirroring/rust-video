//! Decode behind the caps `Decoder` trait.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ash::vk;
use bytes::{Bytes, BytesMut};
use rust_video_caps::common::PictureSize;
use rust_video_caps::delivery::{PhaseTiming, SubmitStats};
use rust_video_caps::frame::{CpuFrame, FramePool, GpuExport, GpuFrame, PixelFormat, Surface};
use rust_video_caps::traits::{Lease, SharedDevice};

use crate::au::{AccessUnit, AuBoundary};
pub use crate::av1::Av1Decoder;
use crate::dpb::{Picture, ReferenceId, ReorderBuffer, SlotBook};
use crate::error::{ParseError, VulkanError};
pub use crate::h264::H264Decoder;
pub use crate::h265::H265Decoder;
use crate::raw::{DeviceExport, ExportPair, ExtDevice, OwnedDevice, EXPORT_MECHANISM};
use crate::readback::Readback;
use crate::session::decode::{
    picture_resource, reference_slot, DpbMeta, ExportTarget, PendingDecode, Session, Submission,
};

/// Native NV12 planes behind one GPU surface: images plus allocations.
///
/// Borrowed from the frame through [`VulkanFrame::planes`]: the handles
/// stay valid while the frame lives and die with it. No ownership moves;
/// external readers must neither destroy nor free them. Sync needs no
/// semaphore: collection waits the submit fence, so [`VulkanFrame::wait`]
/// observes a completed decode.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VulkanPlanes {
    /// Luma image.
    y_image: vk::Image,
    /// Chroma image.
    uv_image: vk::Image,
    /// Luma backing allocation.
    y_memory: vk::DeviceMemory,
    /// Chroma backing allocation.
    uv_memory: vk::DeviceMemory,
}

impl VulkanPlanes {
    /// Luma image.
    #[must_use]
    pub fn y_image(&self) -> vk::Image {
        self.y_image
    }

    /// Chroma image.
    #[must_use]
    pub fn uv_image(&self) -> vk::Image {
        self.uv_image
    }

    /// Luma backing allocation.
    #[must_use]
    pub fn y_memory(&self) -> vk::DeviceMemory {
        self.y_memory
    }

    /// Chroma backing allocation.
    #[must_use]
    pub fn uv_memory(&self) -> vk::DeviceMemory {
        self.uv_memory
    }
}

/// DPB placement of one staged surface.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceLayout {
    /// DPB array layer the picture decoded into.
    slice: u32,
    /// Full texture extent in luma samples.
    coded: PictureSize,
    /// Crop left and top in luma samples.
    origin: (u32, u32),
}

impl SurfaceLayout {
    /// Layout over `slice`, `coded`, and `origin`.
    pub(crate) fn new(slice: u32, coded: PictureSize, origin: (u32, u32)) -> Self {
        Self {
            slice,
            coded,
            origin,
        }
    }
}

/// Decoded surface: NV12 bytes or native planes, with identity and a lease.
///
/// Threading: the decoders stay on their creating thread while queue
/// submission serializes on the device queue lock; a frame itself is
/// `Send` plus `Sync`, so threads share frames freely and hold
/// [`crate::device::VulkanDevice::queue_lock`] across their own submits on
/// [`crate::device::VulkanDevice::queue`].
///
/// Lifetime: the frame owns its pixels or planes plus its budget lease
/// and borrows nothing. A GPU frame pins its export slot until dropped;
/// native handles from [`planes`](Self::planes) die with the frame.
pub struct VulkanFrame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
    /// Device export behind the frame, absent without one.
    device: Option<DeviceExport>,
    /// DPB placement of the surface.
    layout: SurfaceLayout,
    /// Native planes behind a GPU frame, absent on the readback path.
    planes: Option<VulkanPlanes>,
}

impl VulkanFrame {
    /// Frame identity.
    #[must_use]
    pub fn identity(&self) -> GpuFrame {
        self.identity
    }

    /// Visible picture size in luma samples.
    #[must_use]
    pub fn size(&self) -> PictureSize {
        self.identity.size()
    }

    /// Pixel layout of the frame.
    #[must_use]
    pub fn format(&self) -> PixelFormat {
        self.identity.format()
    }

    /// DPB array layer the picture decoded into.
    #[must_use]
    pub fn slice(&self) -> u32 {
        self.layout.slice
    }

    /// Full texture extent in luma samples.
    #[must_use]
    pub fn coded(&self) -> PictureSize {
        self.layout.coded
    }

    /// Crop left and top in luma samples.
    #[must_use]
    pub fn origin(&self) -> (u32, u32) {
        self.layout.origin
    }

    /// Native planes behind a GPU frame, absent on the readback path.
    #[must_use]
    pub fn planes(&self) -> Option<&VulkanPlanes> {
        self.planes.as_ref()
    }

    /// Block until the surface's decode completes.
    ///
    /// Collection waits the submit fence before staging, so every
    /// emitted surface is already complete and this returns at once;
    /// it is the external-reader sync point, never a second wait.
    pub fn wait(&self) {}

    /// DPB placement of the surface.
    #[must_use]
    pub(crate) fn layout(&self) -> SurfaceLayout {
        self.layout
    }

    /// NV12 frame bytes in row-major order.
    #[must_use]
    pub fn nv12(&self) -> &[u8] {
        &self.data
    }

    /// Shared NV12 frame bytes.
    #[must_use]
    pub(crate) fn bytes(&self) -> &Bytes {
        &self.data
    }

    /// Planar `yuv420p` bytes: Y then U then V.
    ///
    /// The golden oracle hashes planar bytes, so tests compare through this.
    #[must_use]
    pub fn yuv420p(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.yuv420p_into(&mut out);
        out
    }

    /// Planar bytes into `out`, reusing its allocation.
    pub fn yuv420p_into(&self, out: &mut Vec<u8>) {
        let size = self.identity.size();
        let luma = u64::from(size.width()) * u64::from(size.height());
        let Ok(y_len) = usize::try_from(luma) else {
            out.clear();
            return;
        };
        out.clear();
        out.resize(y_len + y_len / 2, 0);
        let y_take = y_len.min(self.data.len());
        out[..y_take].copy_from_slice(&self.data[..y_take]);
        let uv = &self.data[y_take.min(self.data.len())..];
        let (u, v) = out[y_len..].split_at_mut(y_len / 4);
        for (index, pair) in uv.chunks_exact(2).enumerate() {
            if index >= u.len() {
                break;
            }
            u[index] = pair[0];
            v[index] = pair[1];
        }
    }

    /// Budget bytes the frame holds.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// Platform export handle for the frame.
    ///
    /// Platform planes while the device export lives; an opaque backend
    /// descriptor without one. Infallible by shape, never `BackendAbsent`.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        match self.device.as_ref() {
            Some(device) => device.export(),
            None => GpuExport::Opaque { name: "vulkan" },
        }
    }

    /// Device export behind the frame, absent without one.
    #[must_use]
    pub(crate) fn device_export(&self) -> Option<&DeviceExport> {
        self.device.as_ref()
    }

    /// CPU copy split into NV12 planes.
    #[must_use]
    pub fn to_cpu(&self) -> Option<CpuFrame> {
        CpuFrame::from_nv12(self.identity.size(), &self.data)
    }
}

impl Surface for VulkanFrame {
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

/// Exportable planes behind one leased submit.
pub(crate) struct ExportLease {
    /// Shared planes; the pool slot stays out of reuse while live.
    pair: Arc<ExportPair>,
    /// First submit into the planes since creation.
    first: bool,
}

impl ExportLease {
    /// Shared planes.
    pub(crate) fn pair(&self) -> &Arc<ExportPair> {
        &self.pair
    }

    /// First submit into the planes since creation.
    pub(crate) fn first(&self) -> bool {
        self.first
    }
}

/// Pooled exportable planes of one display size.
struct ExportEntry {
    /// Shared planes.
    pair: Arc<ExportPair>,
    /// Frame extent in luma samples.
    extent: (u32, u32),
    /// At least one submit ran into the planes.
    primed: bool,
}

/// Display sink: reorder buffer, VRAM budget, and frame generation.
pub(crate) struct FrameSink {
    /// Display reordering.
    reorder: ReorderBuffer<VulkanFrame>,
    /// VRAM budget shared with frames.
    shared: SharedDevice,
    /// Phase timing per collected picture, oldest first.
    timings: Vec<PhaseTiming>,
    /// Collection order behind the next logged timing.
    timing_seq: u64,
    /// Frame generation counter.
    generation: u64,
    /// Pooled display buffers for the collected size.
    pool: Option<(PictureSize, FramePool)>,
    /// Pooled exportable planes by display size.
    exports: Vec<ExportEntry>,
    /// Owned instance plus device, shared with pooled planes.
    keep: Arc<OwnedDevice>,
    /// Copy worker behind launches.
    pub(crate) readback: Readback,
    /// Launched but unharvested pictures, oldest first.
    pub(crate) launched: VecDeque<LaunchedPicture>,
}

impl FrameSink {
    /// Empty sink over `shared`, keeping `keep` alive in pooled planes.
    pub(crate) fn new(shared: SharedDevice, keep: Arc<OwnedDevice>) -> Self {
        Self {
            reorder: ReorderBuffer::with_capacity(0),
            shared,
            timings: Vec::new(),
            timing_seq: 0,
            generation: 0,
            pool: None,
            exports: Vec::new(),
            keep,
            readback: Readback::spawn(),
            launched: VecDeque::new(),
        }
    }

    /// Display reordering.
    pub(crate) fn reorder(&mut self) -> &mut ReorderBuffer<VulkanFrame> {
        &mut self.reorder
    }

    /// Launched but unlanded readback copies.
    pub(crate) fn launched_len(&self) -> usize {
        self.launched.len()
    }

    /// Collection order behind the next logged timing.
    pub(crate) fn next_timing_seq(&mut self) -> u64 {
        let seq = self.timing_seq;
        self.timing_seq += 1;
        seq
    }

    /// Log the phase timing of one collected picture.
    pub(crate) fn push_timing(&mut self, timing: PhaseTiming) {
        self.timings.push(timing);
    }

    /// Logged phase timings, oldest first, clearing the log.
    pub(crate) fn take_timings(&mut self) -> Vec<PhaseTiming> {
        std::mem::take(&mut self.timings)
    }

    /// Pooled readback buffer for `size`, sized for the `coded` extent.
    pub(crate) fn pooled(&mut self, size: PictureSize, coded: PictureSize) -> BytesMut {
        if self.pool.as_ref().is_none_or(|(held, _)| *held != size) {
            self.pool = Some((size, FramePool::new(coded, PixelFormat::Nv12)));
        }
        match self.pool.as_mut() {
            Some((_, pool)) => pool.acquire(),
            None => BytesMut::new(),
        }
    }

    /// Exportable planes at `extent` copied from `format`.
    ///
    /// A live frame pins its planes: the lease reuses a size-matching
    /// slot no frame holds, else it drops idle sizes and creates one.
    ///
    /// # Errors
    ///
    /// Returns `Err` without allocation handles, on a non-NV12 source,
    /// or when plane creation fails.
    pub(crate) fn lease_export(
        &mut self,
        alloc: Option<(
            &ash::Device,
            &ExtDevice,
            &vk::PhysicalDeviceMemoryProperties,
        )>,
        format: vk::Format,
        extent: (u32, u32),
    ) -> Result<ExportLease, VulkanError> {
        if !crate::raw::two_plane_nv12(format) {
            return Err(VulkanError::Unsupported {
                what: "export needs two-plane nv12".to_owned(),
            });
        }
        if let Some(entry) = self
            .exports
            .iter()
            .find(|entry| entry.extent == extent && Arc::strong_count(&entry.pair) == 1)
        {
            return Ok(ExportLease {
                pair: entry.pair.clone(),
                first: !entry.primed,
            });
        }
        let Some((device, ext, props)) = alloc else {
            return Err(VulkanError::Unsupported {
                what: EXPORT_MECHANISM.to_owned(),
            });
        };
        self.exports
            .retain(|entry| Arc::strong_count(&entry.pair) > 1);
        let pair = Arc::new(ExportPair::create(
            device,
            ext,
            props,
            extent,
            format,
            self.keep.clone(),
        )?);
        self.exports.push(ExportEntry {
            pair: pair.clone(),
            extent,
            primed: false,
        });
        Ok(ExportLease { pair, first: true })
    }

    /// Mark `lease` primed after its submit ran.
    pub(crate) fn mark_export_used(&mut self, lease: &ExportLease) {
        if let Some(entry) = self
            .exports
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.pair, lease.pair()))
        {
            entry.primed = true;
        }
    }

    /// Display frame from device `lease` through export, lease, and reorder.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit(
        &mut self,
        session: &Session,
        poc: i32,
        resets: bool,
        lease: ExportLease,
        layout: SurfaceLayout,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage_export(session, lease, layout)?;
        self.generation += 1;
        Ok(self.reorder.push_flushing_on_reset(resets, poc, frame))
    }

    /// Display frame from device `lease`, queued without a reset flush.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit_queued(
        &mut self,
        session: &Session,
        poc: i32,
        lease: ExportLease,
        layout: SurfaceLayout,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage_export(session, lease, layout)?;
        self.generation += 1;
        Ok(self.reorder.push(poc, frame))
    }

    /// Display frame from collected `buffer` through lease and reorder.
    ///
    /// # Errors
    ///
    /// Returns `Err` on an exhausted VRAM budget.
    pub(crate) fn emit_readback(
        &mut self,
        size: PictureSize,
        buffer: BytesMut,
        poc: i32,
        resets: bool,
        layout: SurfaceLayout,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage_readback(size, buffer, layout)?;
        self.generation += 1;
        Ok(self.reorder.push_flushing_on_reset(resets, poc, frame))
    }

    /// Display frame from collected `buffer`, queued without a reset flush.
    ///
    /// # Errors
    ///
    /// Returns `Err` on an exhausted VRAM budget.
    pub(crate) fn emit_readback_queued(
        &mut self,
        size: PictureSize,
        buffer: BytesMut,
        poc: i32,
        layout: SurfaceLayout,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage_readback(size, buffer, layout)?;
        self.generation += 1;
        Ok(self.reorder.push(poc, frame))
    }

    /// Display frame from retained bytes, queued in display order.
    ///
    /// # Errors
    ///
    /// Returns `Err` on an exhausted VRAM budget.
    pub(crate) fn emit_stored(
        &mut self,
        size: PictureSize,
        cpu: Option<&Bytes>,
        gpu: Option<&Arc<ExportPair>>,
        poc: i32,
        layout: SurfaceLayout,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let identity = GpuFrame::new(size, PixelFormat::Nv12, self.generation);
        let data = cpu.cloned().unwrap_or_default();
        let lease = self.shared.try_acquire(data.len() as u64)?;
        let device = gpu.map(|pair| DeviceExport::new((*pair).clone()));
        let planes = gpu.map(|pair| VulkanPlanes {
            y_image: pair.y_image(),
            uv_image: pair.uv_image(),
            y_memory: pair.y_memory(),
            uv_memory: pair.uv_memory(),
        });
        self.generation += 1;
        Ok(self.reorder.push(
            poc,
            VulkanFrame {
                identity,
                data,
                lease,
                device,
                layout,
                planes,
            },
        ))
    }

    /// Leased frame carrying the device `lease` with empty bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    fn stage_export(
        &mut self,
        session: &Session,
        lease: ExportLease,
        layout: SurfaceLayout,
    ) -> Result<VulkanFrame, VulkanError> {
        let (display_width, display_height) = session.geometry().display();
        let size =
            PictureSize::new(display_width, display_height).ok_or(VulkanError::BadExtent {
                width: display_width,
                height: display_height,
            })?;
        let identity = GpuFrame::new(size, PixelFormat::Nv12, self.generation);
        self.mark_export_used(&lease);
        let held = self.shared.try_acquire(0)?;
        let planes = Some(VulkanPlanes {
            y_image: lease.pair().y_image(),
            uv_image: lease.pair().uv_image(),
            y_memory: lease.pair().y_memory(),
            uv_memory: lease.pair().uv_memory(),
        });
        Ok(VulkanFrame {
            identity,
            data: Bytes::new(),
            lease: held,
            device: Some(DeviceExport::new(lease.pair)),
            layout,
            planes,
        })
    }

    /// Leased frame over the collected `buffer` bytes.
    ///
    /// # Errors
    ///
    /// Returns `Err` on an exhausted VRAM budget.
    fn stage_readback(
        &mut self,
        size: PictureSize,
        buffer: BytesMut,
        layout: SurfaceLayout,
    ) -> Result<VulkanFrame, VulkanError> {
        let bytes = buffer.freeze();
        let lease = self.shared.try_acquire(bytes.len() as u64)?;
        let identity = GpuFrame::new(size, PixelFormat::Nv12, self.generation);
        Ok(VulkanFrame {
            identity,
            data: bytes,
            lease,
            device: None,
            layout,
            planes: None,
        })
    }
}

/// Run core behind the shared run path: session, sink, queue, gauges.
pub(crate) struct RunCore<'a> {
    /// Live session, built from the first sequence set.
    pub(crate) session: &'a mut Option<Session>,
    /// Display sink.
    pub(crate) sink: &'a mut FrameSink,
    /// Submitted but uncollected pictures.
    pub(crate) pending: &'a mut Vec<PendingPicture>,
    /// Accumulated submit-core gauges.
    pub(crate) stats: &'a mut SubmitStats,
}

/// NAL/AU front end behind the shared H.26x run path.
///
/// AV1 stays separate: OBU framing has no NAL units or access-unit cuts.
pub(crate) trait NalFrontend {
    /// Streaming splitter output: one owned NAL unit.
    type Raw;
    /// Parsed NAL unit; access-unit cutting consumes it.
    type Nalu: AuBoundary;
    /// Decoded picture.
    type Picture;

    /// Complete raw units out of `bytes`.
    fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Self::Raw>;
    /// Trailing raw units.
    fn flush_splitter(&mut self) -> Vec<Self::Raw>;
    /// Parse one raw unit.
    fn parse(&mut self, raw: &Self::Raw) -> Result<Self::Nalu, VulkanError>;
    /// Feed one parsed unit; returns the closed access unit, if any.
    fn put_au(&mut self, nalu: Self::Nalu) -> Option<AccessUnit<Self::Nalu>>;
    /// Trailing access unit, if any.
    fn flush_au(&mut self) -> Option<AccessUnit<Self::Nalu>>;
    /// Run core: session, sink, queue, gauges.
    fn core(&mut self) -> RunCore<'_>;
    /// Submit one picture without waiting.
    fn submit_picture(&mut self, picture: &Self::Picture, gpu: bool) -> Result<(), VulkanError>;
    /// Run one access unit through the context, submitting its picture.
    fn run_unit(
        &mut self,
        unit: AccessUnit<Self::Nalu>,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Submit one picture, then collect per `mode`.
    fn run_picture(
        &mut self,
        picture: &Self::Picture,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        if mode == DrainMode::Overlap {
            let core = self.core();
            out.extend(backpressure_to_depth(
                core.session,
                core.sink,
                core.pending,
                core.stats,
            )?);
        }
        self.submit_picture(picture, gpu)?;
        match mode {
            DrainMode::Sync => {
                if self.core().pending.len() >= crate::session::slots::SUBMIT_SLOTS {
                    let core = self.core();
                    out.extend(drain_pending(
                        core.session,
                        core.sink,
                        core.pending,
                        core.stats,
                    )?);
                }
            }
            DrainMode::Overlap => {
                let core = self.core();
                out.extend(collect_ready(
                    core.session,
                    core.sink,
                    core.pending,
                    core.stats,
                )?);
            }
        }
        Ok(out)
    }

    /// One NAL unit through parse, access-unit cut, and submit.
    fn run_nal(
        &mut self,
        raw: &Self::Raw,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        let parsed = self.parse(raw)?;
        if let Some(unit) = self.put_au(parsed) {
            out.extend(self.run_unit(unit, gpu, mode)?);
        }
        Ok(out)
    }

    /// Decode raw Annex-B bytes, returning display-ready frames.
    ///
    /// Streams with zero reorder depth emit each picture on the call
    /// carrying its last byte; deeper streams buffer the trailing unit
    /// across calls exactly as before.
    fn run_bytes(
        &mut self,
        bytes: &[u8],
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.push_bytes(bytes) {
            out.extend(self.run_nal(&raw, gpu, mode)?);
        }
        if self.core().sink.reorder().capacity() == 0 {
            for raw in self.flush_splitter() {
                out.extend(self.run_nal(&raw, gpu, mode)?);
            }
            if let Some(unit) = self.flush_au() {
                out.extend(self.run_unit(unit, gpu, mode)?);
            }
        }
        match mode {
            DrainMode::Sync => {
                // Pipeline one deep: the kept submit overlaps the next
                // call instead of draining fully here.
                while self.core().pending.len() > 1 {
                    let core = self.core();
                    out.extend(collect_oldest(
                        core.session,
                        core.sink,
                        core.pending,
                        core.stats,
                    )?);
                }
                let core = self.core();
                out.extend(collect_ready(
                    core.session,
                    core.sink,
                    core.pending,
                    core.stats,
                )?);
            }
            DrainMode::Overlap => {
                let core = self.core();
                out.extend(collect_ready(
                    core.session,
                    core.sink,
                    core.pending,
                    core.stats,
                )?);
            }
        }
        Ok(out)
    }

    /// Held frames, draining the splitter and the access unit first.
    fn flush_inner(&mut self, gpu: bool) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.flush_splitter() {
            let parsed = self.parse(&raw)?;
            if let Some(unit) = self.put_au(parsed) {
                out.extend(self.run_unit(unit, gpu, DrainMode::Sync)?);
            }
        }
        if let Some(unit) = self.flush_au() {
            out.extend(self.run_unit(unit, gpu, DrainMode::Sync)?);
        }
        let core = self.core();
        let drained = drain_pending(core.session, core.sink, core.pending, core.stats)?;
        out.extend(drained);
        out.extend(core.sink.reorder().flush());
        Ok(out)
    }
}

/// Retired slots and DPB resets of one picture before its decode setup.
pub(crate) fn retire_slots(session: &mut Session, sink: &mut FrameSink, picture: &Picture) {
    for id in picture.releases() {
        if let Some(slot) = session.slots().slot_of(*id) {
            session.set_meta(slot, None);
        }
        session.slots_mut().release(*id);
    }
    if picture.drop_held() {
        sink.reorder().drop_held();
    }
    if picture.resets_dpb() {
        session.slots_mut().release_all();
        session.clear_meta();
    }
}

/// Decode target resource and slot index of the claimed setup slot.
pub(crate) fn setup_target(
    session: &Session,
    extent: vk::Extent2D,
    setup_slot: usize,
) -> Result<(vk::VideoPictureResourceInfoKHR<'static>, i32), VulkanError> {
    let setup_layer = u32::try_from(setup_slot).map_err(|_| ParseError::OutOfRange {
        syntax: "setup slot",
    })?;
    let setup_resource = picture_resource(
        session
            .dpb_view(setup_slot)
            .ok_or(VulkanError::OverCaps { what: "dpb slot" })?,
        extent,
        setup_layer,
    );
    let setup_index = i32::try_from(setup_slot).map_err(|_| ParseError::OutOfRange {
        syntax: "setup slot",
    })?;
    Ok((setup_resource, setup_index))
}

/// Reused slot-chain buffers of one submit: resources, std infos, dpb infos.
///
/// DPB infos point into `stds`; the pointers hold until the next `fill`.
pub(crate) struct ChainBufs<S, D> {
    /// Picture resources in fill order.
    resources: Vec<vk::VideoPictureResourceInfoKHR<'static>>,
    /// Standard reference infos in fill order.
    stds: Vec<S>,
    /// DPB slot infos in fill order.
    dpbs: Vec<D>,
}

impl<S: Copy, D> ChainBufs<S, D> {
    /// Empty buffers.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            resources: Vec::new(),
            stds: Vec::new(),
            dpbs: Vec::new(),
        }
    }

    /// Fill from `entries`, reusing allocations.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a slot id exceeds its field width.
    pub(crate) fn fill(
        &mut self,
        entries: &[(usize, S, vk::ImageView)],
        extent: vk::Extent2D,
        syntax: &'static str,
        dpb_of: impl Fn(*const S) -> D,
    ) -> Result<(), VulkanError> {
        self.resources.clear();
        self.stds.clear();
        self.dpbs.clear();
        self.resources.reserve(entries.len());
        self.stds.reserve(entries.len());
        self.dpbs.reserve(entries.len());
        for (slot, std, view) in entries {
            let layer = u32::try_from(*slot).map_err(|_| ParseError::OutOfRange { syntax })?;
            self.resources.push(picture_resource(*view, extent, layer));
            self.stds.push(*std);
        }
        for std in &self.stds {
            self.dpbs.push(dpb_of(std::ptr::from_ref(std)));
        }
        Ok(())
    }

    /// Slot infos over `entries` and the filled buffers.
    ///
    /// # Errors
    ///
    /// Returns `Err` when a slot id exceeds its field width.
    pub(crate) fn slots(
        &self,
        entries: &[(usize, S, vk::ImageView)],
        syntax: &'static str,
    ) -> Result<Vec<vk::VideoReferenceSlotInfoKHR<'_>>, VulkanError> {
        let mut slots = Vec::with_capacity(entries.len());
        for (index, (slot, _, _)) in entries.iter().enumerate() {
            let slot_index = i32::try_from(*slot).map_err(|_| ParseError::OutOfRange { syntax })?;
            let mut info = reference_slot(slot_index, &self.resources[index]);
            info.p_next = std::ptr::from_ref(&self.dpbs[index]).cast();
            slots.push(info);
        }
        Ok(slots)
    }
}

impl<S, D> Default for ChainBufs<S, D> {
    fn default() -> Self {
        Self {
            resources: Vec::new(),
            stds: Vec::new(),
            dpbs: Vec::new(),
        }
    }
}

/// Owned decode target and borrowed slot views of one picture submit.
pub(crate) struct SubmitPlan<'a, 'b, 'c> {
    /// Scratch identity for non-reference pictures.
    pub(crate) setup_id: ReferenceId,
    /// Claimed slot the picture decodes into.
    pub(crate) setup_slot: usize,
    /// Active DPB slots at begin time, with codec metadata.
    pub(crate) begin: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Active reference slots for the decode, with codec metadata.
    pub(crate) refs: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Setup slot with codec metadata.
    pub(crate) setup: &'a vk::VideoReferenceSlotInfoKHR<'c>,
    /// Decode target resource: the setup slot's layer.
    pub(crate) dst: vk::VideoPictureResourceInfoKHR<'static>,
    /// Codec picture-info chain for the decode's `p_next`.
    pub(crate) picture_next: *const std::ffi::c_void,
    /// Slot metadata the setup picture leaves behind.
    pub(crate) meta: DpbMeta,
    /// Flush the reorder buffer on a DPB reset.
    pub(crate) flush_on_reset: bool,
    /// Picture displays once collected.
    pub(crate) emit: bool,
}

/// Drain contract behind a decode call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrainMode {
    /// Collect down to one held picture per call; `flush` releases all.
    Sync,
    /// Submit without waiting; collect signaled completions only.
    Overlap,
}

/// Copy launched on the worker, awaiting harvest.
pub(crate) struct LaunchedPicture {
    /// Waited picture behind the copy.
    pub(crate) picture: PendingPicture,
    /// Engine-wait phase behind the picture.
    pub(crate) engine_wait: Duration,
    /// Display size in luma samples.
    pub(crate) size: PictureSize,
    /// Coded size behind pool buffers.
    pub(crate) coded: PictureSize,
    /// Surface layout behind the frame.
    pub(crate) layout: SurfaceLayout,
    /// Live mapping behind a queued copy, for rescue unmaps.
    pub(crate) release: Option<crate::readback::Release>,
}

/// Submitted but uncollected picture.
pub(crate) struct PendingPicture {
    /// Submitted decode behind the picture.
    pub(crate) pending: PendingDecode,
    /// Queue-submit phase behind the picture.
    pub(crate) submit: Duration,
    /// Submit instant of the picture.
    pub(crate) submitted_at: Instant,
    /// DPB slot the picture decoded into.
    pub(crate) setup_slot: usize,
    /// Picture order count.
    pub(crate) poc: i32,
    /// Picture starts a DPB reset.
    pub(crate) resets: bool,
    /// Flush the reorder buffer on a DPB reset.
    pub(crate) flush_on_reset: bool,
    /// Picture displays once collected.
    pub(crate) emit: bool,
    /// Stable identity when a reference.
    pub(crate) show_id: Option<ReferenceId>,
    /// Export lease behind a GPU submit, absent on the CPU path.
    pub(crate) lease: Option<ExportLease>,
    /// Submit order behind AV1 show-existing holdback.
    pub(crate) seq: u64,
}

/// Queue-submit one picture without waiting.
pub(crate) fn plan_submit(
    session: &mut Session,
    sink: &mut FrameSink,
    body: &Picture,
    plan: &SubmitPlan<'_, '_, '_>,
    gpu: bool,
) -> Result<PendingPicture, VulkanError> {
    let sub = Submission {
        bytes: body.bytes(),
        setup_slot: plan.setup_slot,
        reset: body.resets_dpb(),
        begin: plan.begin,
        refs: plan.refs,
        setup: plan.setup,
        dst: plan.dst,
        picture_next: plan.picture_next,
    };
    let lease = if gpu {
        let format = session.geometry().format();
        let display = session.geometry().display();
        Some(sink.lease_export(session.export_alloc(), format, display)?)
    } else {
        None
    };
    let export = lease
        .as_ref()
        .map(|held| ExportTarget::new(held.pair(), held.first()));
    let submitted_at = Instant::now();
    let pending = match session.submit_async(&sub, export.as_ref()) {
        Ok(pending) => pending,
        Err(error) => {
            session.slots_mut().release(plan.setup_id);
            session.set_meta(plan.setup_slot, None);
            return Err(error);
        }
    };
    let submit = submitted_at.elapsed();
    if body.is_reference() {
        session.set_meta(plan.setup_slot, Some(plan.meta));
    } else {
        session.slots_mut().release(plan.setup_id);
    }
    Ok(PendingPicture {
        pending,
        submit,
        submitted_at,
        setup_slot: plan.setup_slot,
        poc: body.poc(),
        resets: body.resets_dpb(),
        flush_on_reset: plan.flush_on_reset,
        emit: plan.emit,
        show_id: body.is_reference().then_some(plan.setup_id),
        lease,
        seq: 0,
    })
}

/// Launch the copy of waited `picture`, or emit it inline without one.
///
/// Lease pictures emit without a copy; readback pictures map and queue,
/// landing on a later harvest. Returns inline frames, if any.
pub(crate) fn launch_waited(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
    picture: PendingPicture,
    engine_wait: Duration,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let slice = u32::try_from(picture.setup_slot).map_err(|_| ParseError::OutOfRange {
        syntax: "setup slot",
    })?;
    let (coded_width, coded_height) = session.geometry().coded();
    let coded = PictureSize::new(coded_width, coded_height).ok_or(VulkanError::BadExtent {
        width: coded_width,
        height: coded_height,
    })?;
    let layout = SurfaceLayout::new(slice, coded, session.geometry().origin());
    if let Some(lease) = picture.lease {
        let frames = if picture.flush_on_reset {
            sink.emit(session, picture.poc, picture.resets, lease, layout)?
        } else {
            sink.emit_queued(session, picture.poc, lease, layout)?
        };
        let timing = PhaseTiming::new(
            sink.next_timing_seq(),
            picture.submit,
            engine_wait,
            Duration::ZERO,
            picture.submitted_at,
            Instant::now(),
        );
        fold_collection(session, sink, stats, timing);
        return Ok(frames);
    }
    let (display_width, display_height) = session.geometry().display();
    let size = PictureSize::new(display_width, display_height).ok_or(VulkanError::BadExtent {
        width: display_width,
        height: display_height,
    })?;
    let buffer = sink.pooled(size, coded);
    let (data, len, release) = session.map_pending(picture.pending)?;
    let mapped = crate::readback::Mapped::new(buffer, data, len, release.clone());
    // References land deferred: show-existing reads them after release.
    let inline = picture.show_id.is_none();
    if let Some(done) = sink.readback.launch(mapped, inline) {
        let (bytes, release, copy) = done.into_parts();
        release.unmap();
        return land_launched(
            session,
            sink,
            stats,
            LaunchedPicture {
                picture,
                engine_wait,
                size,
                coded,
                layout,
                release: None,
            },
            bytes,
            copy,
        );
    }
    sink.launched.push_back(LaunchedPicture {
        picture,
        engine_wait,
        size,
        coded,
        layout,
        release: Some(release),
    });
    Ok(Vec::new())
}

/// Land one launched copy into display frames.
fn land_launched(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
    launched: LaunchedPicture,
    buffer: BytesMut,
    copy: Duration,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let picture = launched.picture;
    let frames = if picture.flush_on_reset {
        sink.emit_readback(
            launched.size,
            buffer,
            picture.poc,
            picture.resets,
            launched.layout,
        )?
    } else {
        sink.emit_readback_queued(launched.size, buffer, picture.poc, launched.layout)?
    };
    let timing = PhaseTiming::new(
        sink.next_timing_seq(),
        picture.submit,
        launched.engine_wait,
        copy,
        picture.submitted_at,
        Instant::now(),
    );
    fold_collection(session, sink, stats, timing);
    Ok(frames)
}

/// Landed worker copies into display frames, without waiting.
fn harvest_landed(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut out = Vec::new();
    while let Some(done) = sink.readback.harvest() {
        let Some(launched) = sink.launched.pop_front() else {
            debug_assert!(false, "landed copy without launched picture");
            break;
        };
        let (bytes, release, copy) = done.into_parts();
        release.unmap();
        out.extend(land_launched(session, sink, stats, launched, bytes, copy)?);
    }
    Ok(out)
}

/// Oldest launched copy into display frames, waiting for it.
///
/// Falls back to a synchronous read without a worker.
fn collect_launched(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let Some(launched) = sink.launched.pop_front() else {
        return Ok(Vec::new());
    };
    match sink.readback.collect() {
        Some(done) => {
            let (bytes, release, copy) = done.into_parts();
            release.unmap();
            land_launched(session, sink, stats, launched, bytes, copy)
        }
        None => rescue_launched(session, sink, stats, launched),
    }
}

/// Oldest launched copy read synchronously, rescuing a lost worker.
fn rescue_launched(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
    mut launched: LaunchedPicture,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    if let Some(release) = launched.release.take() {
        release.unmap();
    }
    let mut buffer = sink.pooled(launched.size, launched.coded);
    let copy_start = Instant::now();
    session.copy_pending_into(launched.picture.pending, &mut buffer)?;
    land_launched(session, sink, stats, launched, buffer, copy_start.elapsed())
}

/// Fold one collection into the sink log plus the submit gauges.
pub(crate) fn fold_collection(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
    timing: PhaseTiming,
) {
    sink.push_timing(timing);
    let snapshot = session.submit_stats();
    stats.fold_peak(snapshot.peak_inflight());
    stats.add_submitted(session.take_submitted());
    stats.add_resets(session.take_resets());
    stats.add_buffer_creates(session.take_creates());
}

/// Collect the oldest submitted picture, waiting it out.
///
/// Empty when nothing is submitted.
pub(crate) fn collect_oldest(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    if pending.is_empty() && sink.launched.is_empty() {
        return Ok(Vec::new());
    }
    let Some(session) = session.as_mut() else {
        return Err(VulkanError::NoSession);
    };
    let mut out = harvest_landed(session, sink, stats)?;
    if pending.is_empty() {
        if !out.is_empty() || sink.launched.is_empty() {
            return Ok(out);
        }
        out.extend(collect_launched(session, sink, stats)?);
        return Ok(out);
    }
    let picture = pending.remove(0);
    let start = Instant::now();
    session.wait_pending(picture.pending)?;
    out.extend(launch_waited(
        session,
        sink,
        stats,
        picture,
        start.elapsed(),
    )?);
    if out.is_empty() && !sink.launched.is_empty() {
        out.extend(collect_launched(session, sink, stats)?);
    }
    Ok(out)
}

/// Collect the oldest submitted picture, waiting up to `timeout` for it.
///
/// `None` reports the lapsed budget with the picture still pending.
/// Empty when nothing is submitted. Launched-only work waits for the
/// oldest copy within the budget instead of returning at once.
pub(crate) fn collect_oldest_timeout(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
    timeout: Duration,
) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
    if pending.is_empty() && sink.launched.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let Some(session) = session.as_mut() else {
        return Err(VulkanError::NoSession);
    };
    let mut out = harvest_landed(session, sink, stats)?;
    if pending.is_empty() {
        if !out.is_empty() || sink.launched.is_empty() {
            return Ok(Some(out));
        }
        if let Some(done) = sink.readback.collect_timeout(timeout) {
            let Some(launched) = sink.launched.pop_front() else {
                debug_assert!(false, "landed copy without launched picture");
                return Ok(Some(out));
            };
            let (bytes, release, copy) = done.into_parts();
            release.unmap();
            out.extend(land_launched(session, sink, stats, launched, bytes, copy)?);
            return Ok(Some(out));
        }
        return Ok(if out.is_empty() { None } else { Some(out) });
    }
    let picture = pending.remove(0);
    let budget = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
    let start = Instant::now();
    if !session.wait_pending_timeout(picture.pending, budget)? {
        pending.insert(0, picture);
        return Ok(if out.is_empty() { None } else { Some(out) });
    }
    out.extend(launch_waited(
        session,
        sink,
        stats,
        picture,
        start.elapsed(),
    )?);
    if out.is_empty() {
        let elapsed = start.elapsed();
        let remaining = timeout.checked_sub(elapsed).unwrap_or(Duration::ZERO);
        if let Some(done) = sink.readback.collect_timeout(remaining) {
            let Some(launched) = sink.launched.pop_front() else {
                debug_assert!(false, "landed copy without launched picture");
                return Ok(Some(out));
            };
            let (bytes, release, copy) = done.into_parts();
            release.unmap();
            out.extend(land_launched(session, sink, stats, launched, bytes, copy)?);
        }
    }
    Ok(Some(out))
}

/// Collect signaled pictures in order, stopping at the first pending one.
pub(crate) fn collect_ready(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut out = Vec::new();
    loop {
        let ready = match (session.as_ref(), pending.first()) {
            (Some(session), Some(picture)) => session.is_ready(picture.pending)?,
            _ => false,
        };
        if !ready {
            break;
        }
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let picture = pending.remove(0);
        let start = Instant::now();
        session.wait_pending(picture.pending)?;
        out.extend(launch_waited(
            session,
            sink,
            stats,
            picture,
            start.elapsed(),
        )?);
    }
    if !sink.launched.is_empty() {
        if let Some(session) = session.as_mut() {
            out.extend(harvest_landed(session, sink, stats)?);
        }
    }
    Ok(out)
}

/// DPB slot for `id`, or `NoFreeSlot` when every slot is pinned.
pub(crate) fn claim_slot(slots: &mut SlotBook, id: ReferenceId) -> Result<usize, VulkanError> {
    slots.claim(id).map_err(|_| VulkanError::NoFreeSlot)
}

/// Wait oldest pictures out until the submitted count fits the ring.
fn backpressure_to_depth(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut out = Vec::new();
    while pending.len() >= crate::session::slots::SUBMIT_SLOTS {
        out.extend(collect_oldest(session, sink, pending, stats)?);
    }
    Ok(out)
}

/// Collect every submitted picture into display frames.
pub(crate) fn drain_pending(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut out = Vec::new();
    while !pending.is_empty() || !sink.launched.is_empty() {
        out.extend(collect_oldest(session, sink, pending, stats)?);
    }
    Ok(out)
}

/// Fire an armed fault on the live session, if any.
pub(crate) fn apply_armed_fault(session: &mut Option<Session>, fault: &mut bool) {
    if !*fault {
        return;
    }
    let Some(session) = session.as_mut() else {
        return;
    };
    session.inject_fault();
    *fault = false;
}

/// POC of the picture in `slot`, from its stored reference info.
pub(crate) fn ref_poc(session: &Session, slot: usize) -> i32 {
    match session.meta(slot) {
        Some(DpbMeta::H265(std)) => std.PicOrderCntVal,
        Some(DpbMeta::H264(std)) => std.PicOrderCnt[0],
        Some(DpbMeta::Av1(std)) => i32::from(std.OrderHint),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    use super::{SurfaceLayout, VulkanFrame};
    use crate::error::VulkanError;

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
            device: None,
            layout: SurfaceLayout::new(0, size, (0, 0)),
            planes: None,
        }
    }

    #[test]
    fn frames_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::VulkanFrame>();
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
        let over = shared.try_acquire(11).unwrap_err();
        assert_eq!(over.to_string(), "requested 11 bytes with 10 available");
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

    #[test]
    fn surface_contract_reads_layout() {
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        let size = PictureSize::new(4, 2).expect("nonzero extent");
        assert_eq!(held.size(), size);
        assert_eq!(held.format(), PixelFormat::Nv12);
        assert_eq!(held.slice(), 0);
        assert_eq!(held.coded(), size);
        assert_eq!(held.origin(), (0, 0));
        assert!(held.planes().is_none());
        held.wait();
    }

    #[test]
    fn surface_trait_matches_inherents() {
        use rust_video_caps::frame::Surface;
        fn assert_surface<T: Surface>() {}
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        assert_surface::<VulkanFrame>();
        assert_eq!(Surface::size(&held), held.size());
        assert_eq!(Surface::format(&held), held.format());
        assert_eq!(Surface::slice(&held), held.slice());
        assert_eq!(Surface::coded(&held), held.coded());
        assert_eq!(Surface::origin(&held), held.origin());
        assert!(matches!(
            (Surface::export(&held), held.export()),
            (
                GpuExport::Opaque { name: "vulkan" },
                GpuExport::Opaque { name: "vulkan" }
            )
        ));
    }

    #[test]
    fn wait_stays_infallible() {
        let shared = SharedDevice::new("vulkan", 100);
        let held = frame(&shared, 4, 2, vec![0; 12]);
        let _: () = held.wait();
    }

    #[test]
    fn decoders_implement_split_decode() {
        use rust_video_caps::delivery::SplitDecode;
        fn assert_split<D: SplitDecode>() {}
        assert_split::<crate::h264::H264Decoder<'static>>();
        assert_split::<crate::h265::H265Decoder<'static>>();
        assert_split::<crate::av1::Av1Decoder<'static>>();
    }

    #[test]
    fn slot_exhaustion_is_no_free_slot() {
        use crate::dpb::{ReferenceId, SlotBook};
        let mut book = SlotBook::with_slots(1);
        assert_eq!(
            super::claim_slot(&mut book, ReferenceId(0)).expect("slot"),
            0
        );
        let error = super::claim_slot(&mut book, ReferenceId(1)).unwrap_err();
        assert!(matches!(error, VulkanError::NoFreeSlot));
        assert_eq!(error.to_string(), "no free surface slot");
    }
}
