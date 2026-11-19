//! Decode behind the caps `Decoder` trait.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ash::vk;
use bytes::{Bytes, BytesMut};
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, FramePool, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Lease, SharedDevice};

use crate::device::VulkanDevice;
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::parse::au::{AccessUnit, AuBoundary, AuSplitter};
use crate::parse::refs::{
    H264Context, H264Instruction, H265Instruction, Picture, RefPicture, ReferenceId, ReorderBuffer,
    RpsEcho, SlotBook,
};
use crate::raw::{DeviceExport, ExportPair, ExtDevice, OwnedDevice, EXPORT_MECHANISM};
use crate::readback::Readback;
use crate::session::decode::{
    picture_resource, reference_slot, DpbMeta, ExportTarget, PendingDecode, Session, Submission,
};
use crate::session::geometry::{h264_profile_to_std, SessionGeometry};
use crate::session::params::{
    h264_params_add, h264_params_create, H264ProfileChain, ParamsTemplate, H264_DECODE_STD_NAME,
};
use crate::session::slots::SubmitStats;
use crate::session::std_h264::{fill_picture, fill_reference, H264Tables};

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
/// [`VulkanDevice::queue_lock`] across their own submits on
/// [`VulkanDevice::queue`].
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
    /// Frame generation counter.
    generation: u64,
    /// Pooled display buffers for the collected size.
    pool: Option<(PictureSize, FramePool)>,
    /// Pooled exportable planes by display size.
    exports: Vec<ExportEntry>,
    /// Owned instance plus device, shared with pooled planes.
    keep: Arc<OwnedDevice>,
    /// Copy worker behind launches.
    readback: Readback,
    /// Launched but unharvested pictures, oldest first.
    launched: VecDeque<LaunchedPicture>,
}

impl FrameSink {
    /// Empty sink over `shared`, keeping `keep` alive in pooled planes.
    pub(crate) fn new(shared: SharedDevice, keep: Arc<OwnedDevice>) -> Self {
        Self {
            reorder: ReorderBuffer::with_capacity(0),
            shared,
            timings: Vec::new(),
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

/// Split-phase decode: submit without waiting, poll completions in order.
///
/// `submit_packet` queues pictures and returns the frames already
/// complete; `poll_ready` collects further completions without waiting;
/// `collect_next` waits the oldest picture out. Completions hand off in
/// submit order: same-queue execution completes in order, and every
/// collect matches the pending picture explicitly.
pub trait OverlappedDecode {
    /// Queue `packet` pictures without waiting on their completion.
    ///
    /// Collects completions already signaled; back-pressures past the
    /// ring depth only inside multi-picture packets.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    fn submit_packet(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Completed pictures, oldest first, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its ready frames.
    fn poll_ready(&mut self) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Oldest submitted picture, waiting it out.
    ///
    /// Empty when nothing is submitted.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its oldest picture.
    fn collect_next(&mut self) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Oldest submitted picture, waiting up to `timeout` for it.
    ///
    /// `None` reports the lapsed budget with the picture still pending.
    /// Empty when nothing is submitted.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its oldest picture.
    fn collect_next_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError>;

    /// Trailing frames, then end of stream.
    ///
    /// Waits every submitted picture out, like `flush`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    fn finish_overlapped(&mut self) -> Result<Vec<VulkanFrame>, VulkanError>;

    /// Submitted but uncollected picture count.
    fn inflight_len(&self) -> usize;

    /// Logged phase timings, oldest first, clearing the log.
    fn take_timings(&mut self) -> Vec<PhaseTiming>;
}

/// Submit, engine-wait, and copy phases of one collected picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhaseTiming {
    /// Queue-submit phase: record plus submit, no completion wait.
    submit: Duration,
    /// Engine-wait phase: fence wait for the decode to complete.
    engine_wait: Duration,
    /// Copy phase: readback map into the staging buffer.
    copy: Duration,
    /// Submit instant of the picture.
    submitted_at: Instant,
    /// Collection instant of the picture.
    collected_at: Instant,
}

impl PhaseTiming {
    /// Queue-submit phase: record plus submit, no completion wait.
    #[must_use]
    pub fn submit(&self) -> Duration {
        self.submit
    }

    /// Engine-wait phase: fence wait for the decode to complete.
    #[must_use]
    pub fn engine_wait(&self) -> Duration {
        self.engine_wait
    }

    /// Copy phase: readback map into the staging buffer.
    #[must_use]
    pub fn copy(&self) -> Duration {
        self.copy
    }

    /// Submit instant of the picture.
    #[must_use]
    pub fn submitted_at(&self) -> Instant {
        self.submitted_at
    }

    /// Collection instant of the picture.
    #[must_use]
    pub fn collected_at(&self) -> Instant {
        self.collected_at
    }

    /// Stage-time sum: submit plus engine-wait plus copy.
    #[must_use]
    pub fn stage_sum(&self) -> Duration {
        self.submit + self.engine_wait + self.copy
    }
}

/// Run core behind the shared run path: session, sink, queue, gauges.
struct RunCore<'a> {
    /// Live session, built from the first sequence set.
    session: &'a mut Option<Session>,
    /// Display sink.
    sink: &'a mut FrameSink,
    /// Submitted but uncollected pictures.
    pending: &'a mut Vec<PendingPicture>,
    /// Accumulated submit-core gauges.
    stats: &'a mut SubmitStats,
}

/// NAL/AU front end behind the shared H.26x run path.
///
/// AV1 stays separate: OBU framing has no NAL units or access-unit cuts.
trait NalFrontend {
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
                let core = self.core();
                let drained = drain_pending(core.session, core.sink, core.pending, core.stats)?;
                out.extend(drained);
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

/// H.264 decoder over a negotiated Vulkan device.
///
/// Thread-local: parser state holds `Rc` tables, so the decoder stays on
/// its creating thread while queue submission takes the device lock.
pub struct H264Decoder<'a> {
    /// Negotiated device, borrowed for the decoder's life.
    device: &'a VulkanDevice,
    /// Annex-B splitter.
    splitter: crate::parse::h264::NaluSplitter,
    /// H.264 parser.
    parser: crate::parse::h264::H264Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::parse::h264::ParsedNalu>,
    /// Reference context.
    context: H264Context,
    /// Standard-structure tables.
    tables: H264Tables,
    /// Parsed sequence sets by id.
    sps: HashMap<u8, Rc<crate::parse::h264::Sps>>,
    /// Parsed picture sets by id.
    pps: HashMap<u8, Rc<crate::parse::h264::Pps>>,
    /// Live session, built from the first sequence set.
    session: Option<Session>,
    /// Display sink.
    sink: FrameSink,
    /// Scratch reference ids for non-reference pictures.
    scratch: ReferenceId,
    /// Reused begin-chain buffers.
    begin_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeH264ReferenceInfo,
        vk::VideoDecodeH264DpbSlotInfoKHR<'static>,
    >,
    /// Reused decode-chain buffers.
    ref_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeH264ReferenceInfo,
        vk::VideoDecodeH264DpbSlotInfoKHR<'static>,
    >,
    /// Submitted but uncollected pictures.
    pending: Vec<PendingPicture>,
    /// Accumulated submit-core gauges.
    stats: SubmitStats,
    /// Armed fault for the next submit, applied once a session runs.
    fault: bool,
}

impl Drop for H264Decoder<'_> {
    fn drop(&mut self) {
        self.sink.readback.shutdown();
    }
}

impl<'a> H264Decoder<'a> {
    /// Decoder over `device`, negotiated for H.264 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated a different operation.
    pub fn open(device: &'a VulkanDevice) -> Result<Self, VulkanError> {
        if device.op() != CodecOp::H264Decode {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            device,
            splitter: crate::parse::h264::NaluSplitter::new(),
            parser: crate::parse::h264::H264Parser::new(),
            aus: AuSplitter::new(),
            context: H264Context::new(),
            tables: H264Tables::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
            session: None,
            sink: FrameSink::new(device.shared().clone(), device.owned()),
            scratch: ReferenceId(1 << 30),
            begin_bufs: ChainBufs::new(),
            ref_bufs: ChainBufs::new(),
            pending: Vec::new(),
            stats: SubmitStats::new(),
            fault: false,
        })
    }

    /// Submit-core gauges of the decoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.stats
    }

    /// Fail the next submit once, exercising the reset path.
    ///
    /// Armed before any session, the fault fires on the first submit.
    pub fn inject_submit_fault(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.inject_fault();
        } else {
            self.fault = true;
        }
    }

    /// Session for `sps`, recreated on geometry change.
    ///
    /// Reports recreation plus pre-recreation frames; drains submitted
    /// pictures before replacing the session they reference.
    fn ensure_session(
        &mut self,
        sps: &crate::parse::h264::Sps,
    ) -> Result<(bool, Vec<VulkanFrame>), VulkanError> {
        let crate::ladder::OpCaps::Decode(facts) = self.device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        let profile = h264_profile_to_std(sps.profile_idc).ok_or(ParseError::Unsupported {
            what: "h264 profile",
        })?;
        let chain = H264ProfileChain::new(profile);
        let usage =
            vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR;
        let formats =
            self.device
                .backend()
                .video_formats(self.device.adapter(), &chain.base(), usage)?;
        let geometry = SessionGeometry::h264(sps, &facts, &formats)?;
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.geometry() == &geometry)
        {
            return Ok((false, Vec::new()));
        }
        let drained = drain_pending(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )?;
        let std_version = crate::raw::std_header_version(H264_DECODE_STD_NAME, facts.std_version);
        let mut template = ParamsTemplate::H264(h264_params_create());
        let mut session = Session::open(
            self.device,
            &geometry,
            &chain.base(),
            &std_version,
            &mut template,
        )?;
        let mut seq_stds = Vec::new();
        let mut pic_stds = Vec::new();
        for (id, sps) in &self.sps {
            let Some(std) = self.tables.sps(*id, sps) else {
                continue;
            };
            seq_stds.push(std);
        }
        for (id, pps) in &self.pps {
            let Some(std) = self.tables.pps(*id, pps) else {
                continue;
            };
            pic_stds.push(std);
        }
        if !seq_stds.is_empty() || !pic_stds.is_empty() {
            let mut add = h264_params_add(&seq_stds, &pic_stds)?;
            let update = vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
            session.params_mut().update(&update)?;
        }
        self.session = Some(session);
        Ok((true, drained))
    }

    /// Frames decoded from `packet` with device exports attached.
    ///
    /// The frames skip the NV12 readback: `nv12` is empty and `to_cpu`
    /// is absent. Each frame pins its export planes until dropped.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet` or export it.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), true, DrainMode::Sync)
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(true)
    }
}

impl NalFrontend for H264Decoder<'_> {
    type Raw = crate::parse::h264::Nalu;
    type Nalu = crate::parse::h264::ParsedNalu;
    type Picture = Picture;

    fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Self::Raw> {
        self.splitter.push(bytes)
    }

    fn flush_splitter(&mut self) -> Vec<Self::Raw> {
        self.splitter.flush()
    }

    fn parse(&mut self, raw: &Self::Raw) -> Result<Self::Nalu, VulkanError> {
        Ok(self.parser.parse(raw)?)
    }

    fn put_au(&mut self, nalu: Self::Nalu) -> Option<AccessUnit<Self::Nalu>> {
        self.aus.put(nalu)
    }

    fn flush_au(&mut self) -> Option<AccessUnit<Self::Nalu>> {
        self.aus.flush()
    }

    fn core(&mut self) -> RunCore<'_> {
        RunCore {
            session: &mut self.session,
            sink: &mut self.sink,
            pending: &mut self.pending,
            stats: &mut self.stats,
        }
    }

    fn submit_picture(&mut self, picture: &Picture, gpu: bool) -> Result<(), VulkanError> {
        apply_armed_fault(&mut self.session, &mut self.fault);
        let Self {
            session,
            sink,
            scratch,
            begin_bufs,
            ref_bufs,
            pending,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, sink, picture);
        let setup_id = picture.id().unwrap_or_else(|| scratch.next());
        let setup_slot = claim_slot(session.slots_mut(), setup_id)?;
        let extent = {
            let (width, height) = session.geometry().coded();
            vk::Extent2D { width, height }
        };
        let refs = collect_h264_refs(session, picture)?;
        let ref_entries = fill_h264_refs(session, &refs);
        let begin_entries = begin_h264_entries(session);
        let dpb_of = |std| vk::VideoDecodeH264DpbSlotInfoKHR {
            p_std_reference_info: std,
            ..Default::default()
        };
        ref_bufs.fill(&ref_entries, extent, "decode slot", dpb_of)?;
        begin_bufs.fill(&begin_entries, extent, "begin slot", dpb_of)?;
        let ref_slots = ref_bufs.slots(&ref_entries, "decode slot")?;
        let begin_slots = begin_bufs.slots(&begin_entries, "begin slot")?;
        let setup_std = fill_reference(picture.frame_num(), picture.poc(), false);
        let (setup_resource, setup_index) = setup_target(session, extent, setup_slot)?;
        let std_picture = fill_picture(picture);
        let codec = vk::VideoDecodeH264PictureInfoKHR {
            p_std_picture_info: std::ptr::from_ref(&std_picture),
            slice_count: u32::try_from(picture.slice_offsets().len()).map_err(|_| {
                ParseError::OutOfRange {
                    syntax: "slice count",
                }
            })?,
            p_slice_offsets: picture.slice_offsets().as_ptr(),
            ..Default::default()
        };
        let setup_dpb = vk::VideoDecodeH264DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&setup_std),
            ..Default::default()
        };
        let mut setup = reference_slot(setup_index, &setup_resource);
        setup.p_next = std::ptr::from_ref(&setup_dpb).cast();
        let setup_bind = reference_slot(-1, &setup_resource);
        let mut begin_all = begin_slots;
        if session.meta(setup_slot).is_none() {
            begin_all.push(setup_bind);
        }
        let submitted = plan_submit(
            session,
            sink,
            picture,
            &SubmitPlan {
                setup_id,
                setup_slot,
                begin: &begin_all,
                refs: &ref_slots,
                setup: &setup,
                dst: setup_resource,
                picture_next: std::ptr::from_ref(&codec).cast(),
                meta: DpbMeta::H264(setup_std),
                flush_on_reset: true,
                emit: true,
            },
            gpu,
        )?;
        pending.push(submitted);
        Ok(())
    }

    fn run_unit(
        &mut self,
        unit: AccessUnit<Self::Nalu>,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H264Instruction::Sps(sps) => {
                    let fresh = !self.sps.contains_key(&sps.seq_parameter_set_id);
                    self.tables.insert_sps(&sps)?;
                    if fresh {
                        let drained = drain_pending(
                            &mut self.session,
                            &mut self.sink,
                            &mut self.pending,
                            &mut self.stats,
                        )?;
                        out.extend(drained);
                    }
                    self.sink
                        .reorder()
                        .set_capacity(sps.num_reorder_frames().unwrap_or(0).min(16));
                    self.sps.insert(sps.seq_parameter_set_id, sps.clone());
                    let (recreated, drained) = self.ensure_session(&sps)?;
                    out.extend(drained);
                    let Some(session) = self.session.as_mut() else {
                        return Err(VulkanError::NoSession);
                    };
                    if fresh && !recreated {
                        if let Some(std) = self.tables.sps(sps.seq_parameter_set_id, &sps) {
                            let std_sps = [std];
                            let mut add = h264_params_add(&std_sps, &[])?;
                            let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                .push_next(&mut add);
                            session.params_mut().update(&update)?;
                        }
                    }
                }
                H264Instruction::Pps(pps) => {
                    let fresh = !self.pps.contains_key(&pps.pic_parameter_set_id);
                    self.tables.insert_pps(&pps);
                    if fresh {
                        let drained = drain_pending(
                            &mut self.session,
                            &mut self.sink,
                            &mut self.pending,
                            &mut self.stats,
                        )?;
                        out.extend(drained);
                    }
                    self.pps.insert(pps.pic_parameter_set_id, pps.clone());
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.pps(pps.pic_parameter_set_id, &pps) {
                                let std_pps = [std];
                                let mut add = h264_params_add(&[], &std_pps)?;
                                let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                    .push_next(&mut add);
                                session.params_mut().update(&update)?;
                            }
                        }
                    }
                }
                H264Instruction::Picture(picture) => {
                    out.extend(self.run_picture(&picture, gpu, mode)?);
                }
            }
        }
        Ok(out)
    }
}

impl Decoder for H264Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false, DrainMode::Sync)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner(false)
    }
}

impl OverlappedDecode for H264Decoder<'_> {
    fn submit_packet(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), false, DrainMode::Overlap)
    }

    fn poll_ready(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        collect_ready(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )
    }

    fn collect_next(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        collect_oldest(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )
    }

    fn collect_next_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        collect_oldest_timeout(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
            timeout,
        )
    }

    fn finish_overlapped(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(false)
    }

    fn inflight_len(&self) -> usize {
        self.pending.len()
    }

    fn take_timings(&mut self) -> Vec<PhaseTiming> {
        self.sink.take_timings()
    }
}

/// Retired slots and DPB resets of one picture before its decode setup.
fn retire_slots(session: &mut Session, sink: &mut FrameSink, picture: &Picture) {
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

/// Deduplicated reference slots of one H.264 picture in DPB order.
fn collect_h264_refs<'a>(
    session: &Session,
    picture: &'a Picture,
) -> Result<Vec<(usize, &'a RefPicture)>, VulkanError> {
    let mut refs = Vec::new();
    let lists = [picture.l0(), picture.l1()];
    for list in lists.into_iter().flatten() {
        for reference in list {
            let Some(id) = reference.id() else { continue };
            let Some(slot) = session.slots().slot_of(id) else {
                continue;
            };
            if refs.iter().any(|(seen, _)| *seen == slot) {
                continue;
            }
            refs.push((slot, reference));
        }
    }
    if refs.len() > session.geometry().active_refs() as usize {
        return Err(VulkanError::OverCaps {
            what: "active references",
        });
    }
    Ok(refs)
}

/// Standard reference entries of `refs`, tagged into the session metas.
fn fill_h264_refs(
    session: &mut Session,
    refs: &[(usize, &RefPicture)],
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeH264ReferenceInfo,
    vk::ImageView,
)> {
    let mut entries = Vec::with_capacity(refs.len());
    for (slot, reference) in refs {
        let Some(view) = session.dpb_view(*slot) else {
            continue;
        };
        let poc = reference.poc();
        let frame_num = reference.frame_num();
        let std = fill_reference(frame_num, poc, reference.long_term().is_some());
        session.set_meta(*slot, Some(DpbMeta::H264(std)));
        entries.push((*slot, std, view));
    }
    entries
}

/// Standard reference entries held at begin time.
fn begin_h264_entries(
    session: &Session,
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeH264ReferenceInfo,
    vk::ImageView,
)> {
    let mut entries = Vec::new();
    for slot in 0..session.geometry().dpb_slots() as usize {
        let Some(DpbMeta::H264(std)) = session.meta(slot) else {
            continue;
        };
        let Some(view) = session.dpb_view(slot) else {
            continue;
        };
        entries.push((slot, std, view));
    }
    entries
}

/// Decode target resource and slot index of the claimed setup slot.
fn setup_target(
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
struct ChainBufs<S, D> {
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
    fn new() -> Self {
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
    fn fill(
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
    fn slots(
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
struct SubmitPlan<'a, 'b, 'c> {
    /// Scratch identity for non-reference pictures.
    setup_id: ReferenceId,
    /// Claimed slot the picture decodes into.
    setup_slot: usize,
    /// Active DPB slots at begin time, with codec metadata.
    begin: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Active reference slots for the decode, with codec metadata.
    refs: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Setup slot with codec metadata.
    setup: &'a vk::VideoReferenceSlotInfoKHR<'c>,
    /// Decode target resource: the setup slot's layer.
    dst: vk::VideoPictureResourceInfoKHR<'static>,
    /// Codec picture-info chain for the decode's `p_next`.
    picture_next: *const std::ffi::c_void,
    /// Slot metadata the setup picture leaves behind.
    meta: DpbMeta,
    /// Flush the reorder buffer on a DPB reset.
    flush_on_reset: bool,
    /// Picture displays once collected.
    emit: bool,
}

/// Drain contract behind a decode call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrainMode {
    /// Wait every submitted picture out before returning.
    Sync,
    /// Submit without waiting; collect signaled completions only.
    Overlap,
}

/// Copy launched on the worker, awaiting harvest.
struct LaunchedPicture {
    /// Waited picture behind the copy.
    picture: PendingPicture,
    /// Engine-wait phase behind the picture.
    engine_wait: Duration,
    /// Display size in luma samples.
    size: PictureSize,
    /// Coded size behind pool buffers.
    coded: PictureSize,
    /// Surface layout behind the frame.
    layout: SurfaceLayout,
    /// Live mapping behind a queued copy, for rescue unmaps.
    release: Option<crate::readback::Release>,
}

/// Submitted but uncollected picture.
struct PendingPicture {
    /// Submitted decode behind the picture.
    pending: PendingDecode,
    /// Queue-submit phase behind the picture.
    submit: Duration,
    /// Submit instant of the picture.
    submitted_at: Instant,
    /// DPB slot the picture decoded into.
    setup_slot: usize,
    /// Picture order count.
    poc: i32,
    /// Picture starts a DPB reset.
    resets: bool,
    /// Flush the reorder buffer on a DPB reset.
    flush_on_reset: bool,
    /// Picture displays once collected.
    emit: bool,
    /// Stable identity when a reference.
    show_id: Option<ReferenceId>,
    /// Export lease behind a GPU submit, absent on the CPU path.
    lease: Option<ExportLease>,
}

/// Queue-submit one picture without waiting.
fn plan_submit(
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
    })
}

/// Launch the copy of waited `picture`, or emit it inline without one.
///
/// Lease pictures emit without a copy; readback pictures map and queue,
/// landing on a later harvest. Returns inline frames, if any.
fn launch_waited(
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
        fold_collection(
            session,
            sink,
            stats,
            PhaseTiming {
                submit: picture.submit,
                engine_wait,
                copy: Duration::ZERO,
                submitted_at: picture.submitted_at,
                collected_at: Instant::now(),
            },
        );
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
    fold_collection(
        session,
        sink,
        stats,
        PhaseTiming {
            submit: picture.submit,
            engine_wait: launched.engine_wait,
            copy,
            submitted_at: picture.submitted_at,
            collected_at: Instant::now(),
        },
    );
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
fn fold_collection(
    session: &mut Session,
    sink: &mut FrameSink,
    stats: &mut SubmitStats,
    timing: PhaseTiming,
) {
    sink.push_timing(timing);
    let snapshot = session.submit_stats();
    stats.peak_inflight = stats.peak_inflight.max(snapshot.peak_inflight);
    stats.resets += session.take_resets();
}

/// Collect the oldest submitted picture, waiting it out.
///
/// Empty when nothing is submitted.
fn collect_oldest(
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
/// Empty when nothing is submitted.
fn collect_oldest_timeout(
    session: &mut Option<Session>,
    sink: &mut FrameSink,
    pending: &mut Vec<PendingPicture>,
    stats: &mut SubmitStats,
    timeout: Duration,
) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
    if pending.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if pending.is_empty() && sink.launched.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let Some(session) = session.as_mut() else {
        return Err(VulkanError::NoSession);
    };
    let mut out = harvest_landed(session, sink, stats)?;
    if pending.is_empty() {
        return Ok(Some(out));
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
fn collect_ready(
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
fn claim_slot(slots: &mut SlotBook, id: ReferenceId) -> Result<usize, VulkanError> {
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
fn drain_pending(
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
fn apply_armed_fault(session: &mut Option<Session>, fault: &mut bool) {
    if !*fault {
        return;
    }
    let Some(session) = session.as_mut() else {
        return;
    };
    session.inject_fault();
    *fault = false;
}

/// Deduplicated reference slots of one H.265 picture in DPB order.
fn collect_h265_refs(
    session: &Session,
    echo: &RpsEcho,
) -> Result<Vec<(usize, ReferenceId)>, VulkanError> {
    let mut refs = Vec::new();
    let lists: [&[Option<ReferenceId>]; 3] = [echo.before(), echo.after(), echo.lt()];
    for list in lists {
        for id in list.iter().flatten() {
            let Some(slot) = session.slots().slot_of(*id) else {
                continue;
            };
            if refs.iter().any(|(seen, _)| *seen == slot) {
                continue;
            }
            refs.push((slot, *id));
        }
    }
    if refs.len() > session.geometry().active_refs() as usize {
        return Err(VulkanError::OverCaps {
            what: "active references",
        });
    }
    Ok(refs)
}

/// Standard reference entries of `refs`, tagged into the session metas.
fn fill_h265_refs(
    session: &mut Session,
    refs: &[(usize, ReferenceId)],
    echo: &RpsEcho,
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeH265ReferenceInfo,
    vk::ImageView,
)> {
    use crate::session::std_h265::fill_reference;
    let mut entries = Vec::with_capacity(refs.len());
    for (slot, id) in refs {
        let Some(view) = session.dpb_view(*slot) else {
            continue;
        };
        let long_term = echo.lt().contains(&Some(*id));
        let poc = ref_poc(session, *slot);
        let std = fill_reference(poc, long_term);
        session.set_meta(*slot, Some(DpbMeta::H265(std)));
        entries.push((*slot, std, view));
    }
    entries
}

/// Standard reference entries held at begin time.
fn begin_h265_entries(
    session: &Session,
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeH265ReferenceInfo,
    vk::ImageView,
)> {
    let mut entries = Vec::new();
    for slot in 0..session.geometry().dpb_slots() as usize {
        let Some(DpbMeta::H265(std)) = session.meta(slot) else {
            continue;
        };
        let Some(view) = session.dpb_view(slot) else {
            continue;
        };
        entries.push((slot, std, view));
    }
    entries
}

/// H.265 decoder over a negotiated Vulkan device.
///
/// Thread-local: parser state holds `Rc` tables, so the decoder stays on
/// its creating thread while queue submission takes the device lock.
pub struct H265Decoder<'a> {
    /// Negotiated device, borrowed for the decoder's life.
    device: &'a VulkanDevice,
    /// Annex-B splitter.
    splitter: crate::parse::h265::NaluSplitter,
    /// H.265 parser.
    parser: crate::parse::h265::H265Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::parse::h265::ParsedNalu>,
    /// Reference context.
    context: crate::parse::refs::H265Context,
    /// Standard-structure tables.
    tables: crate::session::std_h265::H265Tables,
    /// Parsed video sets by id.
    vps: HashMap<u8, Rc<crate::parse::h265::Vps>>,
    /// Parsed sequence sets by id.
    sps: HashMap<u8, Rc<crate::parse::h265::Sps>>,
    /// Parsed picture sets by id.
    pps: HashMap<u8, Rc<crate::parse::h265::Pps>>,
    /// Live session, built from the first sequence set.
    session: Option<Session>,
    /// Display sink.
    sink: FrameSink,
    /// Scratch reference ids for non-reference pictures.
    scratch: ReferenceId,
    /// Reused begin-chain buffers.
    begin_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeH265ReferenceInfo,
        vk::VideoDecodeH265DpbSlotInfoKHR<'static>,
    >,
    /// Reused decode-chain buffers.
    ref_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeH265ReferenceInfo,
        vk::VideoDecodeH265DpbSlotInfoKHR<'static>,
    >,
    /// Submitted but uncollected pictures.
    pending: Vec<PendingPicture>,
    /// Accumulated submit-core gauges.
    stats: SubmitStats,
    /// Armed fault for the next submit, applied once a session runs.
    fault: bool,
}

impl Drop for H265Decoder<'_> {
    fn drop(&mut self) {
        self.sink.readback.shutdown();
    }
}

impl<'a> H265Decoder<'a> {
    /// Decoder over `device`, negotiated for H.265 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated a different operation.
    pub fn open(device: &'a VulkanDevice) -> Result<Self, VulkanError> {
        if device.op() != CodecOp::H265Decode {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            device,
            splitter: crate::parse::h265::NaluSplitter::new(),
            parser: crate::parse::h265::H265Parser::new(),
            aus: AuSplitter::new(),
            context: crate::parse::refs::H265Context::new(),
            tables: crate::session::std_h265::H265Tables::new(),
            vps: HashMap::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
            session: None,
            sink: FrameSink::new(device.shared().clone(), device.owned()),
            scratch: ReferenceId(1 << 30),
            begin_bufs: ChainBufs::new(),
            ref_bufs: ChainBufs::new(),
            pending: Vec::new(),
            stats: SubmitStats::new(),
            fault: false,
        })
    }

    /// Submit-core gauges of the decoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.stats
    }

    /// Fail the next submit once, exercising the reset path.
    ///
    /// Armed before any session, the fault fires on the first submit.
    pub fn inject_submit_fault(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.inject_fault();
        } else {
            self.fault = true;
        }
    }

    /// Session for `sps`, recreated on geometry change.
    ///
    /// Reports recreation plus pre-recreation frames; drains submitted
    /// pictures before replacing the session they reference.
    fn ensure_session(
        &mut self,
        sps: &crate::parse::h265::Sps,
    ) -> Result<(bool, Vec<VulkanFrame>), VulkanError> {
        let crate::ladder::OpCaps::Decode(facts) = self.device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        let profile = crate::session::geometry::h265_profile_to_std(sps.ptl.profile_idc).ok_or(
            ParseError::Unsupported {
                what: "h265 profile",
            },
        )?;
        let chain = crate::session::params::H265ProfileChain::new(profile);
        let usage =
            vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR;
        let formats =
            self.device
                .backend()
                .video_formats(self.device.adapter(), &chain.base(), usage)?;
        let geometry = SessionGeometry::h265(sps, &facts, &formats)?;
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.geometry() == &geometry)
        {
            return Ok((false, Vec::new()));
        }
        let drained = drain_pending(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )?;
        let std_version = crate::raw::std_header_version(
            crate::session::params::H265_DECODE_STD_NAME,
            facts.std_version,
        );
        let mut template = ParamsTemplate::H265(crate::session::params::h265_params_create());
        let mut session = Session::open(
            self.device,
            &geometry,
            &chain.base(),
            &std_version,
            &mut template,
        )?;
        let mut vps_stds = Vec::new();
        let mut seq_stds = Vec::new();
        let mut pic_stds = Vec::new();
        for (id, vps) in &self.vps {
            if let Some(std) = self.tables.vps(*id, vps) {
                vps_stds.push(std);
            }
        }
        for (id, sps) in &self.sps {
            if let Some(std) = self.tables.sps(*id, sps).transpose()? {
                seq_stds.push(std);
            }
        }
        for (id, pps) in &self.pps {
            if let Some(std) = self.tables.pps(*id, pps) {
                pic_stds.push(std);
            }
        }
        if !vps_stds.is_empty() || !seq_stds.is_empty() || !pic_stds.is_empty() {
            let mut add = crate::session::params::h265_params_add(&vps_stds, &seq_stds, &pic_stds)?;
            let update = vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
            session.params_mut().update(&update)?;
        }
        self.session = Some(session);
        Ok((true, drained))
    }

    /// Frames decoded from `packet` with device exports attached.
    ///
    /// The frames skip the NV12 readback: `nv12` is empty and `to_cpu`
    /// is absent. Each frame pins its export planes until dropped.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet` or export it.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), true, DrainMode::Sync)
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(true)
    }
}

impl NalFrontend for H265Decoder<'_> {
    type Raw = crate::parse::h265::Nalu;
    type Nalu = crate::parse::h265::ParsedNalu;
    type Picture = crate::parse::refs::H265Picture;

    fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Self::Raw> {
        self.splitter.push(bytes)
    }

    fn flush_splitter(&mut self) -> Vec<Self::Raw> {
        self.splitter.flush()
    }

    fn parse(&mut self, raw: &Self::Raw) -> Result<Self::Nalu, VulkanError> {
        Ok(self.parser.parse(raw)?)
    }

    fn put_au(&mut self, nalu: Self::Nalu) -> Option<AccessUnit<Self::Nalu>> {
        self.aus.put(nalu)
    }

    fn flush_au(&mut self) -> Option<AccessUnit<Self::Nalu>> {
        self.aus.flush()
    }

    fn core(&mut self) -> RunCore<'_> {
        RunCore {
            session: &mut self.session,
            sink: &mut self.sink,
            pending: &mut self.pending,
            stats: &mut self.stats,
        }
    }

    fn submit_picture(
        &mut self,
        picture: &crate::parse::refs::H265Picture,
        gpu: bool,
    ) -> Result<(), VulkanError> {
        use crate::session::std_h265::{fill_picture, fill_reference};
        let body = picture.picture();
        let echo = picture.echo();
        apply_armed_fault(&mut self.session, &mut self.fault);
        let Self {
            session,
            sink,
            scratch,
            begin_bufs,
            ref_bufs,
            pending,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, sink, body);
        let setup_id = body.id().unwrap_or_else(|| scratch.next());
        let setup_slot = claim_slot(session.slots_mut(), setup_id)?;
        let extent = {
            let (width, height) = session.geometry().coded();
            vk::Extent2D { width, height }
        };
        let refs = collect_h265_refs(session, echo)?;
        let ref_entries = fill_h265_refs(session, &refs, echo);
        let begin_entries = begin_h265_entries(session);
        let dpb_of = |std| vk::VideoDecodeH265DpbSlotInfoKHR {
            p_std_reference_info: std,
            ..Default::default()
        };
        ref_bufs.fill(&ref_entries, extent, "decode slot", dpb_of)?;
        begin_bufs.fill(&begin_entries, extent, "begin slot", dpb_of)?;
        let ref_slots = ref_bufs.slots(&ref_entries, "decode slot")?;
        let begin_slots = begin_bufs.slots(&begin_entries, "begin slot")?;
        let setup_std = fill_reference(body.poc(), false);
        let (setup_resource, setup_index) = setup_target(session, extent, setup_slot)?;
        let std_picture = fill_picture(picture, session.slots())?;
        let codec = vk::VideoDecodeH265PictureInfoKHR {
            p_std_picture_info: std::ptr::from_ref(&std_picture),
            slice_segment_count: u32::try_from(body.slice_offsets().len()).map_err(|_| {
                ParseError::OutOfRange {
                    syntax: "slice count",
                }
            })?,
            p_slice_segment_offsets: body.slice_offsets().as_ptr(),
            ..Default::default()
        };
        let setup_dpb = vk::VideoDecodeH265DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&setup_std),
            ..Default::default()
        };
        let mut setup = reference_slot(setup_index, &setup_resource);
        setup.p_next = std::ptr::from_ref(&setup_dpb).cast();
        let setup_bind = reference_slot(-1, &setup_resource);
        let mut begin_all = begin_slots;
        if session.meta(setup_slot).is_none() {
            begin_all.push(setup_bind);
        }
        let submitted = plan_submit(
            session,
            sink,
            body,
            &SubmitPlan {
                setup_id,
                setup_slot,
                begin: &begin_all,
                refs: &ref_slots,
                setup: &setup,
                dst: setup_resource,
                picture_next: std::ptr::from_ref(&codec).cast(),
                meta: DpbMeta::H265(setup_std),
                flush_on_reset: false,
                emit: true,
            },
            gpu,
        )?;
        pending.push(submitted);
        Ok(())
    }

    fn run_unit(
        &mut self,
        unit: AccessUnit<Self::Nalu>,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H265Instruction::Vps(vps) => {
                    let fresh = !self.vps.contains_key(&vps.id);
                    self.tables.insert_vps(&vps)?;
                    if fresh {
                        let drained = drain_pending(
                            &mut self.session,
                            &mut self.sink,
                            &mut self.pending,
                            &mut self.stats,
                        )?;
                        out.extend(drained);
                    }
                    self.vps.insert(vps.id, vps.clone());
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.vps(vps.id, &vps) {
                                let std_vps = [std];
                                let mut add =
                                    crate::session::params::h265_params_add(&std_vps, &[], &[])?;
                                let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                    .push_next(&mut add);
                                session.params_mut().update(&update)?;
                            }
                        }
                    }
                }
                H265Instruction::Sps(sps) => {
                    let fresh = !self.sps.contains_key(&sps.id);
                    self.tables.insert_sps(&sps)?;
                    if fresh {
                        let drained = drain_pending(
                            &mut self.session,
                            &mut self.sink,
                            &mut self.pending,
                            &mut self.stats,
                        )?;
                        out.extend(drained);
                    }
                    self.sink.reorder().set_capacity(u32::from(
                        sps.ordering_reorder.last().copied().unwrap_or(0).min(16),
                    ));
                    self.sps.insert(sps.id, sps.clone());
                    let (recreated, drained) = self.ensure_session(&sps)?;
                    out.extend(drained);
                    let Some(session) = self.session.as_mut() else {
                        return Err(VulkanError::NoSession);
                    };
                    if fresh && !recreated {
                        if let Some(std) = self.tables.sps(sps.id, &sps).transpose()? {
                            let std_sps = [std];
                            let mut add =
                                crate::session::params::h265_params_add(&[], &std_sps, &[])?;
                            let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                .push_next(&mut add);
                            session.params_mut().update(&update)?;
                        }
                    }
                }
                H265Instruction::Pps(pps) => {
                    let fresh = !self.pps.contains_key(&pps.id);
                    if fresh {
                        let drained = drain_pending(
                            &mut self.session,
                            &mut self.sink,
                            &mut self.pending,
                            &mut self.stats,
                        )?;
                        out.extend(drained);
                    }
                    let sps_id = pps.sps_id;
                    let Some(sps) = self.sps.get(&sps_id).cloned() else {
                        return Err(ParseError::MissingSet {
                            id: u32::from(sps_id),
                        }
                        .into());
                    };
                    self.tables.insert_pps(&pps, &sps)?;
                    self.pps.insert(pps.id, pps.clone());
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.pps(pps.id, &pps) {
                                let std_pps = [std];
                                let mut add =
                                    crate::session::params::h265_params_add(&[], &[], &std_pps)?;
                                let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                    .push_next(&mut add);
                                session.params_mut().update(&update)?;
                            }
                        }
                    }
                }
                H265Instruction::Picture(picture) => {
                    out.extend(self.run_picture(&picture, gpu, mode)?);
                }
            }
        }
        Ok(out)
    }
}

impl Decoder for H265Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false, DrainMode::Sync)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner(false)
    }
}

impl OverlappedDecode for H265Decoder<'_> {
    fn submit_packet(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), false, DrainMode::Overlap)
    }

    fn poll_ready(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        collect_ready(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )
    }

    fn collect_next(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        collect_oldest(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )
    }

    fn collect_next_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        collect_oldest_timeout(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
            timeout,
        )
    }

    fn finish_overlapped(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(false)
    }

    fn inflight_len(&self) -> usize {
        self.pending.len()
    }

    fn take_timings(&mut self) -> Vec<PhaseTiming> {
        self.sink.take_timings()
    }
}

/// Retained display frame behind one reference identity.
struct StoredFrame {
    /// Display size in luma samples.
    size: PictureSize,
    /// NV12 bytes on the CPU path, absent on the GPU path.
    cpu: Option<Bytes>,
    /// Exportable planes on the GPU path, absent on the CPU path.
    gpu: Option<Arc<ExportPair>>,
    /// DPB placement of the surface.
    layout: SurfaceLayout,
}

/// Display frames retained for show-existing displays.
struct ShowStore {
    /// Retained frames by reference identity.
    entries: Vec<(ReferenceId, StoredFrame)>,
}

impl ShowStore {
    /// Empty store.
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Retain `frame` under `id`, replacing any prior entry.
    fn insert(
        &mut self,
        id: ReferenceId,
        size: PictureSize,
        cpu: Option<Bytes>,
        gpu: Option<Arc<ExportPair>>,
        layout: SurfaceLayout,
    ) {
        self.entries.retain(|entry| entry.0 != id);
        self.entries.push((
            id,
            StoredFrame {
                size,
                cpu,
                gpu,
                layout,
            },
        ));
    }

    /// Retained frame of `id`, if any.
    fn get(&self, id: ReferenceId) -> Option<&StoredFrame> {
        self.entries
            .iter()
            .find(|entry| entry.0 == id)
            .map(|entry| &entry.1)
    }

    /// Drop every entry named in `releases`.
    fn evict(&mut self, releases: &[ReferenceId]) {
        self.entries.retain(|entry| !releases.contains(&entry.0));
    }

    /// Drop every entry.
    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// AV1 decoder over a negotiated Vulkan device.
///
/// Thread-local: parser state holds `Rc` tables, so the decoder stays on
/// its creating thread while queue submission takes the device lock.
pub struct Av1Decoder<'a> {
    /// Negotiated device, borrowed for the decoder's life.
    device: &'a VulkanDevice,
    /// Reference context.
    context: crate::parse::refs::Av1Context,
    /// Standard-structure tables.
    tables: crate::session::std_av1::Av1Tables,
    /// Active sequence header.
    seq: Option<Rc<crate::parse::av1::SequenceHeader>>,
    /// Live session, built from the first sequence header.
    session: Option<Session>,
    /// Display sink.
    sink: FrameSink,
    /// Scratch reference ids for non-reference pictures.
    scratch: ReferenceId,
    /// Reused begin-chain buffers.
    begin_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
        vk::VideoDecodeAV1DpbSlotInfoKHR<'static>,
    >,
    /// Reused decode-chain buffers.
    ref_bufs: ChainBufs<
        ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
        vk::VideoDecodeAV1DpbSlotInfoKHR<'static>,
    >,
    /// Submitted but uncollected pictures.
    pending: Vec<PendingPicture>,
    /// Retained display frames by reference identity.
    shown: ShowStore,
    /// Display order counter for show-existing frames.
    show_seq: i32,
    /// Accumulated submit-core gauges.
    stats: SubmitStats,
    /// Armed fault for the next submit, applied once a session runs.
    fault: bool,
}

impl Drop for Av1Decoder<'_> {
    fn drop(&mut self) {
        self.sink.readback.shutdown();
    }
}

/// Resolved reference state of one AV1 picture submit.
struct Av1RefState {
    /// Stored order hint per reference name.
    order_hints: [u8; 8],
    /// Sign bias mask over reference names.
    bias: u8,
    /// Expected frame id per reference name.
    expected: [u32; 8],
}

impl<'a> Av1Decoder<'a> {
    /// Decoder over `device`, negotiated for AV1 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated a different operation.
    pub fn open(device: &'a VulkanDevice) -> Result<Self, VulkanError> {
        if device.op() != CodecOp::Av1Decode {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            device,
            context: crate::parse::refs::Av1Context::new(),
            tables: crate::session::std_av1::Av1Tables::new(),
            seq: None,
            session: None,
            sink: FrameSink::new(device.shared().clone(), device.owned()),
            scratch: ReferenceId(1 << 30),
            begin_bufs: ChainBufs::new(),
            ref_bufs: ChainBufs::new(),
            pending: Vec::new(),
            shown: ShowStore::new(),
            show_seq: 0,
            stats: SubmitStats::new(),
            fault: false,
        })
    }

    /// Submit-core gauges of the decoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.stats
    }

    /// Fail the next submit once, exercising the reset path.
    ///
    /// Armed before any session, the fault fires on the first submit.
    pub fn inject_submit_fault(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.inject_fault();
        } else {
            self.fault = true;
        }
    }

    /// Session for `seq` at `display`, recreated on geometry change.
    fn ensure_session(
        &mut self,
        seq: &Rc<crate::parse::av1::SequenceHeader>,
        display: (u32, u32),
    ) -> Result<(), VulkanError> {
        let crate::ladder::OpCaps::Decode(facts) = self.device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        let Some(profile) = crate::session::geometry::av1_profile_to_std(seq.profile) else {
            return Err(ParseError::Unsupported {
                what: "av1 profile",
            }
            .into());
        };
        let chain = crate::session::params::Av1ProfileChain::new(profile);
        let usage =
            vk::ImageUsageFlags::VIDEO_DECODE_DST_KHR | vk::ImageUsageFlags::VIDEO_DECODE_DPB_KHR;
        let formats =
            self.device
                .backend()
                .video_formats(self.device.adapter(), &chain.base(), usage)?;
        let geometry = SessionGeometry::av1(seq, display, &facts, &formats)?;
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.geometry() == &geometry)
        {
            return Ok(());
        }
        let std_version = crate::raw::std_header_version(
            crate::session::params::AV1_DECODE_STD_NAME,
            facts.std_version,
        );
        let Some(std_seq) = self.tables.sequence() else {
            return Err(VulkanError::NoSession);
        };
        let mut template = ParamsTemplate::Av1(crate::session::params::av1_params_create(std_seq));
        let session = Session::open(
            self.device,
            &geometry,
            &chain.base(),
            &std_version,
            &mut template,
        )?;
        self.session = Some(session);
        Ok(())
    }

    /// Order hints, sign bias, and expected ids of `picture` references.
    fn resolve_ref_state(
        &self,
        picture: &crate::parse::refs::Av1Picture,
    ) -> Result<Av1RefState, VulkanError> {
        let (Some(session), Some(seq)) = (self.session.as_ref(), self.seq.as_ref()) else {
            return Err(VulkanError::NoSession);
        };
        let header = picture.header();
        let mut order_hints = [0_u8; 8];
        if !header.frame_type.is_intra() {
            for (name, id) in picture.name_slots().iter().enumerate() {
                let Some(slot) = id.and_then(|id| session.slots().slot_of(id)) else {
                    continue;
                };
                if let Some(DpbMeta::Av1(std)) = session.meta(slot) {
                    order_hints[name + 1] = std.OrderHint;
                }
            }
        }
        let mut bias = 0;
        if seq.enable_order_hint() && !header.frame_type.is_intra() {
            for name in 0..crate::parse::av1::REFS_PER_FRAME {
                if crate::parse::av1::relative_dist(
                    order_hints[name + 1],
                    header.order_hint,
                    seq.order_hint_bits_minus1,
                ) > 0
                {
                    bias |= 1_u8 << (name + 1);
                }
            }
        }
        let mut expected = [0_u32; 8];
        if !header.frame_type.is_intra() {
            for name in 0..crate::parse::av1::REFS_PER_FRAME {
                expected[name + 1] = crate::session::std_av1::expected_frame_id(seq, header, name);
            }
        }
        Ok(Av1RefState {
            order_hints,
            bias,
            expected,
        })
    }

    /// Queue-submit one decoded picture without waiting.
    fn submit_picture(
        &mut self,
        picture: &crate::parse::refs::Av1Picture,
        gpu: bool,
    ) -> Result<(), VulkanError> {
        use crate::session::std_av1::{fill_reference, Av1PictureStd};
        if picture.header().film_grain.apply_grain() {
            return Err(VulkanError::Unsupported {
                what: "av1 film grain output".to_owned(),
            });
        }
        apply_armed_fault(&mut self.session, &mut self.fault);
        let state = self.resolve_ref_state(picture)?;
        let super128 = self
            .seq
            .as_ref()
            .is_some_and(|seq| seq.use_128x128_superblock());
        let Self {
            session,
            sink,
            scratch,
            begin_bufs,
            ref_bufs,
            pending,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let body = picture.picture();
        let header = picture.header();
        release_ids(session, picture.pre());
        let setup_id = body.id().unwrap_or_else(|| scratch.next());
        let setup_slot = claim_slot(session.slots_mut(), setup_id)?;
        let extent = {
            let (width, height) = session.geometry().coded();
            vk::Extent2D { width, height }
        };
        let refs = collect_av1_refs(session, picture)?;
        let ref_entries = fill_av1_refs(session, &refs);
        let begin_entries = begin_av1_entries(session);
        let dpb_of = |std| vk::VideoDecodeAV1DpbSlotInfoKHR {
            p_std_reference_info: std,
            ..Default::default()
        };
        ref_bufs.fill(&ref_entries, extent, "decode slot", dpb_of)?;
        begin_bufs.fill(&begin_entries, extent, "begin slot", dpb_of)?;
        let ref_slots = ref_bufs.slots(&ref_entries, "decode slot")?;
        let begin_slots = begin_bufs.slots(&begin_entries, "begin slot")?;
        let setup_std = fill_reference(
            header.order_hint,
            header.frame_type.id(),
            state.bias,
            state.order_hints,
            header.disable_frame_end_update_cdf(),
            header.segmentation_enabled(),
        );
        let (setup_resource, setup_index) = setup_target(session, extent, setup_slot)?;
        let std_sets = Av1PictureStd::fill(
            header,
            super128,
            state.order_hints,
            state.expected,
            picture.skip_frames(),
        )?;
        let name_indices = av1_name_indices(session, picture)?;
        let codec = av1_codec_info(picture, &std_sets, name_indices)?;
        let setup_dpb = vk::VideoDecodeAV1DpbSlotInfoKHR {
            p_std_reference_info: std::ptr::from_ref(&setup_std),
            ..Default::default()
        };
        let mut setup = reference_slot(setup_index, &setup_resource);
        setup.p_next = std::ptr::from_ref(&setup_dpb).cast();
        let setup_bind = reference_slot(-1, &setup_resource);
        let mut begin_all = begin_slots;
        if session.meta(setup_slot).is_none() {
            begin_all.push(setup_bind);
        }
        let submitted = match plan_submit(
            session,
            sink,
            body,
            &SubmitPlan {
                setup_id,
                setup_slot,
                begin: &begin_all,
                refs: &ref_slots,
                setup: &setup,
                dst: setup_resource,
                picture_next: std::ptr::from_ref(&codec).cast(),
                meta: DpbMeta::Av1(setup_std),
                flush_on_reset: true,
                emit: header.show_frame(),
            },
            gpu,
        ) {
            Ok(submitted) => submitted,
            Err(error) => {
                release_ids(session, picture.post());
                return Err(error);
            }
        };
        release_ids(session, picture.post());
        pending.push(submitted);
        Ok(())
    }

    /// Submit one picture, then collect per `mode`.
    fn run_picture(
        &mut self,
        picture: &crate::parse::refs::Av1Picture,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        if mode == DrainMode::Overlap {
            while self.pending.len() >= crate::session::slots::SUBMIT_SLOTS {
                out.extend(self.collect_one()?);
            }
        }
        self.submit_picture(picture, gpu)?;
        match mode {
            DrainMode::Sync => {
                if self.pending.len() >= crate::session::slots::SUBMIT_SLOTS {
                    out.extend(self.drain()?);
                }
            }
            DrainMode::Overlap => {
                out.extend(self.collect_ready_pictures()?);
            }
        }
        Ok(out)
    }

    /// Run one temporal unit through the context, submitting its pictures.
    fn run_unit(
        &mut self,
        unit: &crate::parse::av1::TemporalUnit<'_>,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        use crate::parse::refs::Av1Instruction;
        let mut out = Vec::new();
        for instruction in self.context.compile(unit)? {
            match instruction {
                Av1Instruction::Sequence(seq) => {
                    let changed = self.seq.as_ref().is_none_or(|cur| cur != &seq);
                    if changed {
                        let drained = self.drain()?;
                        out.extend(drained);
                    }
                    self.tables.insert(&seq)?;
                    if changed {
                        self.session = None;
                        self.shown.clear();
                    }
                    self.seq = Some(seq);
                }
                Av1Instruction::Picture(picture) => {
                    let Some(seq) = self.seq.clone() else {
                        return Err(VulkanError::NoSession);
                    };
                    let display = (
                        picture.header().upscaled_width,
                        picture.header().frame_height,
                    );
                    self.ensure_session(&seq, display)?;
                    out.extend(self.run_picture(picture.as_ref(), gpu, mode)?);
                }
                Av1Instruction::ShowExisting { id, releases } => {
                    let drained = self.drain()?;
                    out.extend(drained);
                    if let Some(session) = self.session.as_mut() {
                        for release in &releases {
                            if let Some(slot) = session.slots().slot_of(*release) {
                                session.set_meta(slot, None);
                            }
                            session.slots_mut().release(*release);
                        }
                    }
                    self.shown.evict(&releases);
                    let Some(stored) = self.shown.get(id) else {
                        return Err(ParseError::MissingSet {
                            id: u32::try_from(id.0).unwrap_or(0),
                        }
                        .into());
                    };
                    let poc = self.show_seq;
                    self.show_seq = self.show_seq.wrapping_add(1);
                    out.extend(self.sink.emit_stored(
                        stored.size,
                        stored.cpu.as_ref(),
                        stored.gpu.as_ref(),
                        poc,
                        stored.layout,
                    )?);
                }
            }
        }
        Ok(out)
    }

    /// Retain `frames` for show-existing displays, dropping unshown ones.
    fn finish_retained(
        &mut self,
        emit: bool,
        show_id: Option<ReferenceId>,
        pair: Option<&Arc<ExportPair>>,
        frames: Vec<VulkanFrame>,
    ) -> Vec<VulkanFrame> {
        if let Some(id) = show_id {
            for frame in &frames {
                let cpu = frame
                    .device_export()
                    .is_none()
                    .then(|| frame.bytes().clone());
                self.shown.insert(
                    id,
                    frame.identity().size(),
                    cpu,
                    pair.cloned(),
                    frame.layout(),
                );
            }
        }
        if emit {
            frames
        } else {
            Vec::new()
        }
    }

    /// Collect the oldest submitted picture, retaining reference frames.
    ///
    /// Empty when nothing is submitted.
    fn collect_one(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        if self.pending.is_empty() && self.sink.launched.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(frames) = self.harvest_one()? {
            return Ok(frames);
        }
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        if self.pending.is_empty() {
            return self.collect_launched_one();
        }
        let picture = self.pending.remove(0);
        let start = Instant::now();
        session.wait_pending(picture.pending)?;
        let engine_wait = start.elapsed();
        let frames = launch_waited(
            session,
            &mut self.sink,
            &mut self.stats,
            picture,
            engine_wait,
        )?;
        if !frames.is_empty() || self.sink.launched.is_empty() {
            return Ok(frames);
        }
        let mut out = frames;
        out.extend(self.collect_launched_one()?);
        Ok(out)
    }

    /// Oldest landed copy into display frames, if any arrived.
    fn harvest_one(&mut self) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        if self.session.is_none() {
            return Err(VulkanError::NoSession);
        }
        let Some(done) = self.sink.readback.harvest() else {
            return Ok(None);
        };
        let Some(launched) = self.sink.launched.pop_front() else {
            debug_assert!(false, "landed copy without launched picture");
            return Ok(None);
        };
        let (bytes, release, copy) = done.into_parts();
        release.unmap();
        self.land_retained(launched, bytes, copy).map(Some)
    }

    /// Oldest launched copy into display frames, waiting for it.
    fn collect_launched_one(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let Some(launched) = self.sink.launched.pop_front() else {
            return Ok(Vec::new());
        };
        if let Some(done) = self.sink.readback.collect() {
            let (bytes, release, copy) = done.into_parts();
            release.unmap();
            return self.land_retained(launched, bytes, copy);
        }
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let mut launched = launched;
        if let Some(release) = launched.release.take() {
            release.unmap();
        }
        let mut buffer = self.sink.pooled(launched.size, launched.coded);
        let copy_start = Instant::now();
        session.copy_pending_into(launched.picture.pending, &mut buffer)?;
        self.land_retained(launched, buffer, copy_start.elapsed())
    }

    /// Land one launched copy, retaining reference frames.
    fn land_retained(
        &mut self,
        launched: LaunchedPicture,
        buffer: BytesMut,
        copy: Duration,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let LaunchedPicture {
            picture,
            engine_wait,
            size,
            coded: _,
            layout,
            release: _,
        } = launched;
        let emit = picture.emit;
        let show_id = picture.show_id;
        let pair = picture.lease.as_ref().map(|lease| lease.pair().clone());
        let frames = if picture.flush_on_reset {
            self.sink
                .emit_readback(size, buffer, picture.poc, picture.resets, layout)?
        } else {
            self.sink
                .emit_readback_queued(size, buffer, picture.poc, layout)?
        };
        let timing = PhaseTiming {
            submit: picture.submit,
            engine_wait,
            copy,
            submitted_at: picture.submitted_at,
            collected_at: Instant::now(),
        };
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        fold_collection(session, &mut self.sink, &mut self.stats, timing);
        Ok(self.finish_retained(emit, show_id, pair.as_ref(), frames))
    }

    /// Collect the oldest submitted picture, waiting up to `timeout` for it.
    ///
    /// `None` reports the lapsed budget with the picture still pending.
    /// Empty when nothing is submitted.
    fn collect_one_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        if self.pending.is_empty() && self.sink.launched.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if let Some(frames) = self.harvest_one()? {
            return Ok(Some(frames));
        }
        if self.pending.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let picture = self.pending.remove(0);
        let budget = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        let start = Instant::now();
        if !session.wait_pending_timeout(picture.pending, budget)? {
            self.pending.insert(0, picture);
            return Ok(None);
        }
        let engine_wait = start.elapsed();
        let frames = launch_waited(
            session,
            &mut self.sink,
            &mut self.stats,
            picture,
            engine_wait,
        )?;
        if !frames.is_empty() {
            return Ok(Some(frames));
        }
        let elapsed = start.elapsed();
        let remaining = timeout.checked_sub(elapsed).unwrap_or(Duration::ZERO);
        let Some(done) = self.sink.readback.collect_timeout(remaining) else {
            return Ok(Some(Vec::new()));
        };
        let Some(launched) = self.sink.launched.pop_front() else {
            debug_assert!(false, "landed copy without launched picture");
            return Ok(Some(Vec::new()));
        };
        let (bytes, release, copy) = done.into_parts();
        release.unmap();
        Ok(Some(self.land_retained(launched, bytes, copy)?))
    }

    /// Collect signaled pictures in order, stopping at the first pending one.
    fn collect_ready_pictures(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        loop {
            let ready = match (self.session.as_ref(), self.pending.first()) {
                (Some(session), Some(picture)) => session.is_ready(picture.pending)?,
                _ => false,
            };
            if !ready {
                break;
            }
            out.extend(self.collect_one()?);
        }
        Ok(out)
    }

    /// Collect every submitted picture, retaining reference frames.
    fn drain(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        while !self.pending.is_empty() || !self.sink.launched.is_empty() {
            out.extend(self.collect_one()?);
        }
        Ok(out)
    }

    /// Decode raw OBU bytes, returning display-ready frames.
    fn run_bytes(
        &mut self,
        bytes: &[u8],
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        let obus = crate::parse::av1::split_obus(bytes)?;
        for unit in crate::parse::av1::temporal_units(obus) {
            out.extend(self.run_unit(&unit, gpu, mode)?);
        }
        match mode {
            DrainMode::Sync => out.extend(self.drain()?),
            DrainMode::Overlap => out.extend(self.collect_ready_pictures()?),
        }
        Ok(out)
    }

    /// Frames decoded from `packet` with device exports attached.
    ///
    /// The frames skip the NV12 readback: `nv12` is empty and `to_cpu`
    /// is absent. Each frame pins its export planes until dropped.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet` or export it.
    pub fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), true, DrainMode::Sync)
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner()
    }

    /// Held frames.
    fn flush_inner(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = self.drain()?;
        out.extend(self.sink.reorder().flush());
        Ok(out)
    }
}

impl Decoder for Av1Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false, DrainMode::Sync)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner()
    }
}

impl OverlappedDecode for Av1Decoder<'_> {
    fn submit_packet(&mut self, packet: &Packet) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.run_bytes(packet.data(), false, DrainMode::Overlap)
    }

    fn poll_ready(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.collect_ready_pictures()
    }

    fn collect_next(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.collect_one()
    }

    fn collect_next_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        self.collect_one_timeout(timeout)
    }

    fn finish_overlapped(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner()
    }

    fn inflight_len(&self) -> usize {
        self.pending.len()
    }

    fn take_timings(&mut self) -> Vec<PhaseTiming> {
        self.sink.take_timings()
    }
}

/// Release `ids` from the session DPB, clearing their metadata.
fn release_ids(session: &mut Session, ids: &[ReferenceId]) {
    for id in ids {
        if let Some(slot) = session.slots().slot_of(*id) {
            session.set_meta(slot, None);
        }
        session.slots_mut().release(*id);
    }
}

/// Reference name slot indices of `picture`, negative when unclaimed.
///
/// # Errors
///
/// Returns `Err` when a slot id exceeds its field width.
fn av1_name_indices(
    session: &Session,
    picture: &crate::parse::refs::Av1Picture,
) -> Result<[i32; vk::MAX_VIDEO_AV1_REFERENCES_PER_FRAME_KHR], VulkanError> {
    let mut name_indices = [-1_i32; vk::MAX_VIDEO_AV1_REFERENCES_PER_FRAME_KHR];
    for (name, id) in picture.name_slots().iter().enumerate() {
        let Some(id) = id else { continue };
        let Some(slot) = session.slots().slot_of(*id) else {
            continue;
        };
        name_indices[name] = i32::try_from(slot).map_err(|_| ParseError::OutOfRange {
            syntax: "decode slot",
        })?;
    }
    Ok(name_indices)
}

/// AV1 picture info chain of `picture` over `std_sets`.
///
/// # Errors
///
/// Returns `Err` when the tile count exceeds its field width.
fn av1_codec_info<'a>(
    picture: &'a crate::parse::refs::Av1Picture,
    std_sets: &'a crate::session::std_av1::Av1PictureStd,
    name_indices: [i32; vk::MAX_VIDEO_AV1_REFERENCES_PER_FRAME_KHR],
) -> Result<vk::VideoDecodeAV1PictureInfoKHR<'a>, VulkanError> {
    Ok(vk::VideoDecodeAV1PictureInfoKHR {
        p_std_picture_info: std::ptr::from_ref(std_sets.info()),
        reference_name_slot_indices: name_indices,
        frame_header_offset: 0,
        tile_count: u32::try_from(picture.tile_offsets().len()).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "tile count",
            }
        })?,
        p_tile_offsets: picture.tile_offsets().as_ptr(),
        p_tile_sizes: picture.tile_sizes().as_ptr(),
        ..Default::default()
    })
}

/// Deduplicated reference slots of one AV1 picture in DPB order.
///
/// # Errors
///
/// Returns `Err` when a cited reference left the session; the caller
/// resyncs instead of submitting a dangling `-1`.
fn collect_av1_refs(
    session: &Session,
    picture: &crate::parse::refs::Av1Picture,
) -> Result<Vec<(usize, ReferenceId)>, VulkanError> {
    let mut refs = Vec::new();
    for id in picture.name_slots().iter().flatten() {
        let Some(slot) = session.slots().slot_of(*id) else {
            return Err(ParseError::MissingSet {
                id: u32::try_from(id.0).map_err(|_| ParseError::OutOfRange {
                    syntax: "reference id",
                })?,
            }
            .into());
        };
        if refs.iter().any(|(seen, _)| *seen == slot) {
            continue;
        }
        refs.push((slot, *id));
    }
    if refs.len() > session.geometry().active_refs() as usize {
        return Err(VulkanError::OverCaps {
            what: "active references",
        });
    }
    Ok(refs)
}

/// Standard reference entries of `refs` from the session metas.
fn fill_av1_refs(
    session: &Session,
    refs: &[(usize, ReferenceId)],
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
    vk::ImageView,
)> {
    let mut entries = Vec::with_capacity(refs.len());
    for (slot, _) in refs {
        let Some(view) = session.dpb_view(*slot) else {
            continue;
        };
        let Some(DpbMeta::Av1(std)) = session.meta(*slot) else {
            continue;
        };
        entries.push((*slot, std, view));
    }
    entries
}

/// Standard reference entries held at begin time.
fn begin_av1_entries(
    session: &Session,
) -> Vec<(
    usize,
    ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
    vk::ImageView,
)> {
    let mut entries = Vec::new();
    for slot in 0..session.geometry().dpb_slots() as usize {
        let Some(DpbMeta::Av1(std)) = session.meta(slot) else {
            continue;
        };
        let Some(view) = session.dpb_view(slot) else {
            continue;
        };
        entries.push((slot, std, view));
    }
    entries
}

/// POC of the picture in `slot`, from its stored reference info.
fn ref_poc(session: &Session, slot: usize) -> i32 {
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

    /// AV1 hardware decode against software decode, or a loud decline.
    ///
    /// Without AV1 caps, ffmpeg, or VRAM the test prints its reason and
    /// passes; with all three the GPU bytes must match software exactly.
    #[test]
    fn av1_matches_software_decode_or_declines() {
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;

        use crate::device::VulkanDevice;
        use crate::error::VulkanError;
        use crate::ladder::CodecOp;

        let device = match VulkanDevice::open_or_skip(CodecOp::Av1Decode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: av1 on this box: {reason}");
                return;
            }
            Err(error) => panic!("negotiation failed: {error}"),
        };
        let workdir = std::env::temp_dir().join("vulkan-av1-test");
        if std::fs::create_dir_all(&workdir).is_err() {
            println!("declined: av1 without a workdir");
            return;
        }
        let vector = workdir.join("probe.obu");
        let synth = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=30:duration=0.2",
                "-c:v",
                "libaom-av1",
                "-b:v",
                "200k",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&vector)
            .output();
        if !synth.is_ok_and(|synth| synth.status.success()) {
            println!("declined: av1 without ffmpeg libaom-av1");
            return;
        }
        let bytes = std::fs::read(&vector).expect("vector reads");
        let mut decoder = match super::Av1Decoder::open(&device) {
            Ok(decoder) => decoder,
            Err(error) if error.is_out_of_device_memory() => {
                let (free_mb, total_mb) = device.memory_budget_mb();
                println!("declined:OOM av1 open: {error} (free {free_mb}MB of {total_mb}MB)");
                return;
            }
            Err(error) => panic!("engine starts: {error}"),
        };
        let mut actual = Vec::new();
        let mut scratch = Vec::new();
        let mut frames = match decoder.decode(&Packet::new(Bytes::from(bytes))) {
            Ok(frames) => frames,
            Err(error) if error.is_out_of_device_memory() => {
                let (free_mb, total_mb) = device.memory_budget_mb();
                println!("declined:OOM av1 decode: {error} (free {free_mb}MB of {total_mb}MB)");
                return;
            }
            Err(error) => panic!("decode runs: {error}"),
        };
        frames.extend(match decoder.flush() {
            Ok(frames) => frames,
            Err(error) if error.is_out_of_device_memory() => {
                let (free_mb, total_mb) = device.memory_budget_mb();
                println!("declined:OOM av1 flush: {error} (free {free_mb}MB of {total_mb}MB)");
                return;
            }
            Err(error) => panic!("flush runs: {error}"),
        });
        assert!(!frames.is_empty(), "no frames decoded");
        for frame in &frames {
            frame.yuv420p_into(&mut scratch);
            actual.extend_from_slice(&scratch);
        }
        let expected = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&vector)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("software decode runs");
        assert!(expected.status.success(), "software decode fails");
        assert_eq!(
            actual.len(),
            expected.stdout.len(),
            "byte count differs from software decode"
        );
        let first = actual
            .iter()
            .zip(expected.stdout.iter())
            .position(|(a, b)| a != b);
        assert!(
            first.is_none(),
            "bytes differ from software decode at {first:?}"
        );
    }

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

    /// Kind tag of one instruction, asserting a zero reorder depth on sets.
    fn tag(instruction: &crate::parse::refs::H264Instruction) -> &'static str {
        use crate::parse::refs::H264Instruction;
        match instruction {
            H264Instruction::Sps(sps) => {
                assert_eq!(sps.num_reorder_frames().unwrap_or(0), 0);
                assert_eq!((sps.width(), sps.height()), (64, 64));
                assert_eq!(sps.display_size(), (64, 64));
                "sps"
            }
            H264Instruction::Pps(_) => "pps",
            H264Instruction::Picture(_) => "pic",
        }
    }

    /// Instruction tags out of one packet, trailing unit completed.
    fn feed_packet(
        splitter: &mut crate::parse::h264::NaluSplitter,
        parser: &mut crate::parse::h264::H264Parser,
        aus: &mut crate::parse::au::AuSplitter<crate::parse::h264::ParsedNalu>,
        context: &mut crate::parse::refs::H264Context,
        bytes: &[u8],
    ) -> Vec<&'static str> {
        let mut tags = Vec::new();
        let mut run_nal = |raw: crate::parse::h264::Nalu| {
            let parsed = parser.parse(&raw).expect("synth parses");
            if let Some(unit) = aus.put(parsed) {
                for instruction in context.compile(unit) {
                    tags.push(tag(&instruction));
                }
            }
        };
        for raw in splitter.push(bytes) {
            run_nal(raw);
        }
        for raw in splitter.flush() {
            run_nal(raw);
        }
        if let Some(unit) = aus.flush() {
            for instruction in context.compile(unit) {
                tags.push(tag(&instruction));
            }
        }
        tags
    }

    #[test]
    fn synth_packets_complete_one_picture_per_call_without_reorder() {
        use rust_video_harness::H264Synth;

        use crate::parse::au::AuSplitter;
        use crate::parse::h264::{H264Parser, NaluSplitter};
        use crate::parse::refs::H264Context;

        let packets = H264Synth::annex_b_packets();
        assert_eq!(packets.len(), 2);
        let mut splitter = NaluSplitter::new();
        let mut parser = H264Parser::new();
        let mut aus = AuSplitter::new();
        let mut context = H264Context::new();
        assert_eq!(
            feed_packet(
                &mut splitter,
                &mut parser,
                &mut aus,
                &mut context,
                &packets[0]
            ),
            vec!["sps", "pps", "pic"]
        );
        assert_eq!(
            feed_packet(
                &mut splitter,
                &mut parser,
                &mut aus,
                &mut context,
                &packets[1]
            ),
            vec!["pic"]
        );
        assert!(aus.flush().is_none());
    }

    #[test]
    fn slot_exhaustion_is_no_free_slot() {
        use crate::parse::refs::{ReferenceId, SlotBook};
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
