//! Decode behind the caps `Decoder` trait.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use ash::vk;
use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, FramePool, GpuExport, GpuFrame, PixelFormat, Plane};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Lease, SharedDevice};

use crate::device::VulkanDevice;
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::parse::au::AuSplitter;
use crate::parse::nal::NaluSplitter;
use crate::parse::refs::{
    H264Context, H264Instruction, Picture, RefPicture, ReferenceId, ReorderBuffer, RpsEcho,
};
use crate::raw::{DeviceExport, ExportPair, OwnedDevice};
use crate::session::decode::{
    crop_nv12_extend, picture_resource, reference_slot, DpbMeta, ExportTarget, Session, Submission,
};
use crate::session::geometry::{h264_profile_to_std, SessionGeometry};
use crate::session::params::{
    h264_params_add, h264_params_create, H264ProfileChain, ParamsTemplate, H264_DECODE_STD_NAME,
};
use crate::session::std_h264::{fill_picture, fill_reference, H264Tables};

/// Decoded frame: NV12 bytes with identity and a budget lease.
///
/// `Send` plus `Sync`: shared bytes plus an atomic budget lease.
pub struct VulkanFrame {
    /// Frame identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
    /// Device export behind the frame, absent without one.
    device: Option<DeviceExport>,
}

impl VulkanFrame {
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
    /// DMA-BUF planes while the device export lives; an opaque backend
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
    /// Frame generation counter.
    generation: u64,
    /// Reused coded-extent staging for submits.
    coded: Vec<u8>,
    /// Pooled display buffers for the staged size.
    pool: Option<(PictureSize, FramePool)>,
    /// Pooled exportable planes by display size.
    exports: Vec<ExportEntry>,
    /// Owned instance plus device, shared with pooled planes.
    keep: Arc<OwnedDevice>,
}

impl FrameSink {
    /// Empty sink over `shared`, keeping `keep` alive in pooled planes.
    pub(crate) fn new(shared: SharedDevice, keep: Arc<OwnedDevice>) -> Self {
        Self {
            reorder: ReorderBuffer::with_capacity(0),
            shared,
            generation: 0,
            coded: Vec::new(),
            pool: None,
            exports: Vec::new(),
            keep,
        }
    }

    /// Display reordering.
    pub(crate) fn reorder(&mut self) -> &mut ReorderBuffer<VulkanFrame> {
        &mut self.reorder
    }

    /// Coded-extent staging for the next submit.
    pub(crate) fn coded_mut(&mut self) -> &mut Vec<u8> {
        &mut self.coded
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
            &ash::khr::external_memory_fd::Device,
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
                what: "dma-buf export".to_owned(),
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

    /// Display frame from staged coded bytes through crop, lease, and reorder.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit(
        &mut self,
        session: &Session,
        poc: i32,
        resets: bool,
        lease: Option<ExportLease>,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage(session, lease)?;
        self.generation += 1;
        Ok(self.reorder.push_flushing_on_reset(resets, poc, frame))
    }

    /// Display frame from staged coded bytes, queued without a reset flush.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit_queued(
        &mut self,
        session: &Session,
        poc: i32,
        lease: Option<ExportLease>,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage(session, lease)?;
        self.generation += 1;
        Ok(self.reorder.push(poc, frame))
    }

    /// Cropped leased frame from staged coded bytes.
    ///
    /// A lease skips the crop: the frame carries the device export with
    /// empty bytes instead.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    fn stage(
        &mut self,
        session: &Session,
        lease: Option<ExportLease>,
    ) -> Result<VulkanFrame, VulkanError> {
        let (display_width, display_height) = session.geometry().display();
        let size =
            PictureSize::new(display_width, display_height).ok_or(VulkanError::BadExtent {
                width: display_width,
                height: display_height,
            })?;
        let identity = GpuFrame::new(size, PixelFormat::Nv12, self.generation);
        if let Some(lease) = lease {
            self.mark_export_used(&lease);
            let held = self.shared.try_acquire(0)?;
            return Ok(VulkanFrame {
                identity,
                data: Bytes::new(),
                lease: held,
                device: Some(DeviceExport::new(lease.pair)),
            });
        }
        let display = vk::Extent2D {
            width: display_width,
            height: display_height,
        };
        let (coded_width, coded_height) = session.geometry().coded();
        let coded = vk::Extent2D {
            width: coded_width,
            height: coded_height,
        };
        if self.pool.as_ref().is_none_or(|(held, _)| *held != size) {
            self.pool = Some((size, FramePool::new(size, PixelFormat::Nv12)));
        }
        let mut buffer = match self.pool.as_mut() {
            Some((_, pool)) => pool.acquire(),
            None => bytes::BytesMut::new(),
        };
        crop_nv12_extend(coded, display, &self.coded, &mut buffer)?;
        let bytes = buffer.freeze();
        let lease = self.shared.try_acquire(bytes.len() as u64)?;
        Ok(VulkanFrame {
            identity,
            data: bytes,
            lease,
            device: None,
        })
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
    splitter: NaluSplitter,
    /// H.264 parser.
    parser: crate::parse::h264::H264Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::parse::h264::Nalu>,
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
            splitter: NaluSplitter::new(),
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
        })
    }

    /// Session for `sps`, recreated on geometry change; reports recreation.
    fn ensure_session(&mut self, sps: &crate::parse::h264::Sps) -> Result<bool, VulkanError> {
        let crate::ladder::OpCaps::Decode(facts) = self.device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        let profile = h264_profile_to_std(sps.profile_idc()).ok_or(ParseError::Unsupported {
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
            return Ok(false);
        }
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
        Ok(true)
    }

    /// Submit one decoded picture, returning display-ready frames.
    fn submit_picture(
        &mut self,
        picture: &Picture,
        gpu: bool,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let Self {
            session,
            sink,
            scratch,
            begin_bufs,
            ref_bufs,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, sink, picture);
        let setup_id = picture.id().unwrap_or_else(|| scratch.next());
        let setup_slot = session.slots_mut().claim(setup_id)?;
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
        finish_submit(
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
            },
            gpu,
        )
    }

    /// Run one access unit through the context, submitting its picture.
    fn run_unit(
        &mut self,
        unit: crate::parse::au::AccessUnit<crate::parse::h264::Nalu>,
        gpu: bool,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H264Instruction::Sps(sps) => {
                    self.tables.insert_sps(&sps)?;
                    self.sink
                        .reorder()
                        .set_capacity(sps.max_num_reorder_frames());
                    let fresh = self.sps.insert(sps.id(), sps.clone()).is_none();
                    let recreated = self.ensure_session(&sps)?;
                    let Some(session) = self.session.as_mut() else {
                        return Err(VulkanError::NoSession);
                    };
                    if fresh && !recreated {
                        if let Some(std) = self.tables.sps(sps.id(), &sps) {
                            let std_sps = [std];
                            let mut add = h264_params_add(&std_sps, &[])?;
                            let update = vk::VideoSessionParametersUpdateInfoKHR::default()
                                .push_next(&mut add);
                            session.params_mut().update(&update)?;
                        }
                    }
                }
                H264Instruction::Pps(pps) => {
                    self.tables.insert_pps(&pps)?;
                    let fresh = self.pps.insert(pps.id(), pps.clone()).is_none();
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.pps(pps.id(), &pps) {
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
                    out.extend(self.submit_picture(&picture, gpu)?);
                }
            }
        }
        Ok(out)
    }

    /// Decode raw Annex-B bytes, returning display-ready frames.
    fn run_bytes(&mut self, bytes: &[u8], gpu: bool) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.push(bytes) {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h264::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit, gpu)?);
            }
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
        self.run_bytes(packet.data(), true)
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(true)
    }

    /// Held frames, with device exports attached when `gpu` holds.
    fn flush_inner(&mut self, gpu: bool) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.flush() {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h264::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit, gpu)?);
            }
        }
        if let Some(unit) = self.aus.flush() {
            out.extend(self.run_unit(unit, gpu)?);
        }
        out.extend(self.sink.reorder().flush());
        Ok(out)
    }
}

impl Decoder for H264Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner(false)
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
    let mut seen = HashSet::new();
    let lists = [picture.l0(), picture.l1()];
    for list in lists.into_iter().flatten() {
        for reference in list {
            let Some(id) = reference.id() else { continue };
            let Some(slot) = session.slots().slot_of(id) else {
                continue;
            };
            if !seen.insert(slot) {
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
}

/// Submission, slot bookkeeping, and display frames of one picture.
fn finish_submit(
    session: &mut Session,
    sink: &mut FrameSink,
    body: &Picture,
    plan: &SubmitPlan<'_, '_, '_>,
    gpu: bool,
) -> Result<Vec<VulkanFrame>, VulkanError> {
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
    let submitted = if let Some(lease) = &lease {
        let target = ExportTarget::new(lease.pair(), lease.first());
        session.submit_gpu(&sub, &target)
    } else {
        session.submit_into(&sub, sink.coded_mut())
    };
    if let Err(error) = submitted {
        session.slots_mut().release(plan.setup_id);
        session.set_meta(plan.setup_slot, None);
        return Err(error);
    }
    if body.is_reference() {
        session.set_meta(plan.setup_slot, Some(plan.meta));
    } else {
        session.slots_mut().release(plan.setup_id);
    }
    if plan.flush_on_reset {
        sink.emit(session, body.poc(), body.resets_dpb(), lease)
    } else {
        sink.emit_queued(session, body.poc(), lease)
    }
}

/// Deduplicated reference slots of one H.265 picture in DPB order.
fn collect_h265_refs(
    session: &Session,
    echo: &RpsEcho,
) -> Result<Vec<(usize, ReferenceId)>, VulkanError> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let lists: [&[Option<ReferenceId>]; 3] = [echo.before(), echo.after(), echo.lt()];
    for list in lists {
        for id in list.iter().flatten() {
            let Some(slot) = session.slots().slot_of(*id) else {
                continue;
            };
            if !seen.insert(slot) {
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
    splitter: NaluSplitter,
    /// H.265 parser.
    parser: crate::parse::h265::H265Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::parse::h265::Nalu>,
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
            splitter: NaluSplitter::new(),
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
        })
    }

    /// Session for `sps`, recreated on geometry change; reports recreation.
    fn ensure_session(&mut self, sps: &crate::parse::h265::Sps) -> Result<bool, VulkanError> {
        let crate::ladder::OpCaps::Decode(facts) = self.device.caps() else {
            return Err(VulkanError::OverCaps { what: "codec op" });
        };
        let profile = crate::session::geometry::h265_profile_to_std(sps.ptl().profile_idc())
            .ok_or(ParseError::Unsupported {
                what: "h265 profile",
            })?;
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
            return Ok(false);
        }
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
            if let Some(std) = self.tables.pps(*id, pps).transpose()? {
                pic_stds.push(std);
            }
        }
        if !vps_stds.is_empty() || !seq_stds.is_empty() || !pic_stds.is_empty() {
            let mut add = crate::session::params::h265_params_add(&vps_stds, &seq_stds, &pic_stds)?;
            let update = vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
            session.params_mut().update(&update)?;
        }
        self.session = Some(session);
        Ok(true)
    }

    /// Submit one decoded picture, returning display-ready frames.
    fn submit_picture(
        &mut self,
        picture: &crate::parse::refs::H265Picture,
        gpu: bool,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        use crate::session::std_h265::{fill_picture, fill_reference};
        let body = picture.picture();
        let echo = picture.echo();
        let Self {
            session,
            sink,
            scratch,
            begin_bufs,
            ref_bufs,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, sink, body);
        let setup_id = body.id().unwrap_or_else(|| scratch.next());
        let setup_slot = session.slots_mut().claim(setup_id)?;
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
        finish_submit(
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
            },
            gpu,
        )
    }

    /// Run one access unit through the context, submitting its picture.
    fn run_unit(
        &mut self,
        unit: crate::parse::au::AccessUnit<crate::parse::h265::Nalu>,
        gpu: bool,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        use crate::parse::refs::H265Instruction;
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H265Instruction::Vps(vps) => {
                    self.tables.insert_vps(&vps)?;
                    let fresh = self.vps.insert(vps.id(), vps.clone()).is_none();
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.vps(vps.id(), &vps) {
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
                    self.tables.insert_sps(&sps)?;
                    self.sink.reorder().set_capacity(sps.max_num_reorder_pics());
                    let fresh = self.sps.insert(sps.id(), sps.clone()).is_none();
                    let recreated = self.ensure_session(&sps)?;
                    let Some(session) = self.session.as_mut() else {
                        return Err(VulkanError::NoSession);
                    };
                    if fresh && !recreated {
                        if let Some(std) = self.tables.sps(sps.id(), &sps).transpose()? {
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
                    let sps_id = pps.sps_id();
                    let Some(sps) = self.sps.get(&sps_id).cloned() else {
                        return Err(ParseError::MissingSet {
                            id: u32::from(sps_id),
                        }
                        .into());
                    };
                    self.tables.insert_pps(&pps, &sps)?;
                    let fresh = self.pps.insert(pps.id(), pps.clone()).is_none();
                    if fresh {
                        if let Some(session) = self.session.as_mut() {
                            if let Some(std) = self.tables.pps(pps.id(), &pps).transpose()? {
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
                    out.extend(self.submit_picture(&picture, gpu)?);
                }
            }
        }
        Ok(out)
    }

    /// Decode raw Annex-B bytes, returning display-ready frames.
    fn run_bytes(&mut self, bytes: &[u8], gpu: bool) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.push(bytes) {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h265::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit, gpu)?);
            }
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
        self.run_bytes(packet.data(), true)
    }

    /// Surface frames held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    pub fn flush_surface(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        self.flush_inner(true)
    }

    /// Held frames, with device exports attached when `gpu` holds.
    fn flush_inner(&mut self, gpu: bool) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.flush() {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h265::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit, gpu)?);
            }
        }
        if let Some(unit) = self.aus.flush() {
            out.extend(self.run_unit(unit, gpu)?);
        }
        out.extend(self.sink.reorder().flush());
        Ok(out)
    }
}

impl Decoder for H265Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner(false)
    }
}

/// POC of the picture in `slot`, from its stored reference info.
fn ref_poc(session: &Session, slot: usize) -> i32 {
    match session.meta(slot) {
        Some(DpbMeta::H265(std)) => std.PicOrderCntVal,
        Some(DpbMeta::H264(std)) => std.PicOrderCnt[0],
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{GpuExport, GpuFrame, PixelFormat};
    use rust_video_caps::traits::SharedDevice;

    use super::VulkanFrame;

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
        let size = PictureSize::new(4, 2).expect("nonzero extent");
        let over = shared.try_acquire(11);
        assert!(over.is_err());
        assert_eq!(size.width(), 4);
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
}
