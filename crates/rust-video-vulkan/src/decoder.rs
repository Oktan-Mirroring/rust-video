//! Decode behind the facade `Decoder` trait.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ash::vk;
use bytes::Bytes;
use rust_video::{CpuFrame, Decoder, GpuExport, GpuFrame, Lease, Packet, PixelFormat, Plane};

use crate::device::VulkanDevice;
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::parse::au::AuSplitter;
use crate::parse::nal::NaluSplitter;
use crate::parse::refs::{
    H264Context, H264Instruction, Picture, RefPicture, ReferenceId, ReorderBuffer, RpsEcho,
};
use crate::session::decode::{
    crop_nv12, picture_resource, reference_slot, DpbMeta, Session, Submission,
};
use crate::session::geometry::{h264_profile_to_std, SessionGeometry};
use crate::session::params::{
    h264_params_add, h264_params_create, H264ProfileChain, ParamsTemplate, H264_DECODE_STD_NAME,
};
use crate::session::std_h264::{fill_picture, fill_reference, H264Tables};

/// Decoded frame: NV12 bytes with identity and a budget lease.
pub struct VulkanFrame {
    /// Facade identity: size, format, generation.
    identity: GpuFrame,
    /// NV12 frame bytes in row-major order.
    data: Bytes,
    /// VRAM budget held for the bytes.
    lease: Lease,
}

impl VulkanFrame {
    /// Facade identity of the frame.
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
        let size = self.identity.size();
        let luma = u64::from(size.width()) * u64::from(size.height());
        let Ok(y_len) = usize::try_from(luma) else {
            return Vec::new();
        };
        let mut out = vec![0; y_len + y_len / 2];
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
        out
    }

    /// Budget bytes the frame holds.
    #[must_use]
    pub fn lease_bytes(&self) -> u64 {
        self.lease.bytes()
    }

    /// Platform export handle for the frame.
    ///
    /// Opaque backend descriptor: the 0.4.0 engine readbacks to CPU,
    /// so no platform handle exists to hand out. Infallible by shape,
    /// never `BackendAbsent`.
    #[must_use]
    pub fn export(&self) -> GpuExport {
        GpuExport::Opaque { name: "vulkan" }
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

/// Display sink: reorder buffer, VRAM budget, and frame generation.
pub(crate) struct FrameSink {
    /// Display reordering.
    reorder: ReorderBuffer<VulkanFrame>,
    /// VRAM budget shared with frames.
    shared: rust_video::SharedDevice,
    /// Frame generation counter.
    generation: u64,
}

impl FrameSink {
    /// Empty sink over `shared`.
    pub(crate) fn new(shared: rust_video::SharedDevice) -> Self {
        Self {
            reorder: ReorderBuffer::with_capacity(0),
            shared,
            generation: 0,
        }
    }

    /// Display reordering.
    pub(crate) fn reorder(&mut self) -> &mut ReorderBuffer<VulkanFrame> {
        &mut self.reorder
    }

    /// Display frame from tight coded `nv12` through crop, lease, and reorder.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit(
        &mut self,
        session: &Session,
        nv12: &[u8],
        poc: i32,
        resets: bool,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage(session, nv12)?;
        self.generation += 1;
        Ok(self.reorder.push_flushing_on_reset(resets, poc, frame))
    }

    /// Display frame from tight coded `nv12`, queued without a reset flush.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    pub(crate) fn emit_queued(
        &mut self,
        session: &Session,
        nv12: &[u8],
        poc: i32,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let frame = self.stage(session, nv12)?;
        self.generation += 1;
        Ok(self.reorder.push(poc, frame))
    }

    /// Cropped leased frame from tight coded `nv12`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on a degenerate display extent or an exhausted VRAM budget.
    fn stage(&mut self, session: &Session, nv12: &[u8]) -> Result<VulkanFrame, VulkanError> {
        let (display_width, display_height) = session.geometry().display();
        let display = vk::Extent2D {
            width: display_width,
            height: display_height,
        };
        let (coded_width, coded_height) = session.geometry().coded();
        let coded = vk::Extent2D {
            width: coded_width,
            height: coded_height,
        };
        let cropped = crop_nv12(coded, display, nv12);
        let size = rust_video_caps::common::PictureSize::new(display_width, display_height).ok_or(
            VulkanError::BadExtent {
                width: display_width,
                height: display_height,
            },
        )?;
        let bytes = Bytes::from(cropped);
        let lease = self.shared.try_acquire(bytes.len() as u64)?;
        Ok(VulkanFrame {
            identity: GpuFrame::new(size, PixelFormat::Nv12, self.generation),
            data: bytes,
            lease,
        })
    }
}

/// H.264 decoder over a negotiated Vulkan device.
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
            sink: FrameSink::new(device.shared().clone()),
            scratch: ReferenceId(1 << 30),
        })
    }

    /// Session for `sps`, rebuilding when the geometry changed.
    fn ensure_session(&mut self, sps: &crate::parse::h264::Sps) -> Result<(), VulkanError> {
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
            return Ok(());
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
        Ok(())
    }

    /// Submit one decoded picture, returning display-ready frames.
    fn submit_picture(&mut self, picture: &Picture) -> Result<Vec<VulkanFrame>, VulkanError> {
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, &mut self.sink, picture);
        let setup_id = picture.id().unwrap_or_else(|| self.scratch.next());
        let setup_slot = session.slots_mut().claim(setup_id)?;
        let extent = {
            let (width, height) = session.geometry().coded();
            vk::Extent2D { width, height }
        };
        let refs = collect_h264_refs(session, picture)?;
        let ref_entries = fill_h264_refs(session, &refs);
        let begin_entries = begin_h264_entries(session);
        let mut begin_resources = Vec::with_capacity(begin_entries.len());
        let mut begin_stds = Vec::with_capacity(begin_entries.len());
        for (slot, std, view) in &begin_entries {
            let layer = u32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "begin slot",
            })?;
            begin_resources.push(picture_resource(*view, extent, layer));
            begin_stds.push(*std);
        }
        let mut begin_dpb = Vec::with_capacity(begin_entries.len());
        for std in &begin_stds {
            begin_dpb.push(vk::VideoDecodeH264DpbSlotInfoKHR {
                p_std_reference_info: std::ptr::from_ref(std),
                ..Default::default()
            });
        }
        let mut begin_slots = Vec::with_capacity(begin_entries.len());
        for (index, (slot, _, _)) in begin_entries.iter().enumerate() {
            let slot_index = i32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "begin slot",
            })?;
            let mut info = reference_slot(slot_index, &begin_resources[index]);
            info.p_next = std::ptr::from_ref(&begin_dpb[index]).cast();
            begin_slots.push(info);
        }
        let mut ref_resources = Vec::with_capacity(ref_entries.len());
        let mut ref_stds = Vec::with_capacity(ref_entries.len());
        for (slot, std, view) in &ref_entries {
            let layer = u32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "decode slot",
            })?;
            ref_resources.push(picture_resource(*view, extent, layer));
            ref_stds.push(*std);
        }
        let mut ref_dpb = Vec::with_capacity(ref_entries.len());
        for std in &ref_stds {
            ref_dpb.push(vk::VideoDecodeH264DpbSlotInfoKHR {
                p_std_reference_info: std::ptr::from_ref(std),
                ..Default::default()
            });
        }
        let mut ref_slots = Vec::with_capacity(ref_entries.len());
        for (index, (slot, _, _)) in ref_entries.iter().enumerate() {
            let slot_index = i32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "decode slot",
            })?;
            let mut info = reference_slot(slot_index, &ref_resources[index]);
            info.p_next = std::ptr::from_ref(&ref_dpb[index]).cast();
            ref_slots.push(info);
        }
        let setup_std = fill_reference(picture.frame_num(), picture.poc(), false);
        let (setup_resource, setup_index) = setup_target(session, extent, setup_slot)?;
        let std_picture = fill_picture(picture);
        if std::env::var("VKPROBE_DUMP").is_ok() {
            dump_h264_probe(
                &self.sps,
                &self.pps,
                picture,
                &refs,
                setup_slot,
                begin_slots.len(),
            );
        }
        finish_h264_submit(
            session,
            &mut self.sink,
            picture,
            &std_picture,
            &SubmitPlan {
                setup_id,
                setup_slot,
                setup_index,
                setup_std,
                begin: &begin_slots,
                refs: &ref_slots,
                dst: setup_resource,
            },
        )
    }

    /// Run one access unit through the context, submitting its picture.
    fn run_unit(
        &mut self,
        unit: crate::parse::au::AccessUnit<crate::parse::h264::Nalu>,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H264Instruction::Sps(sps) => {
                    self.tables.insert_sps(&sps)?;
                    self.sink
                        .reorder()
                        .set_capacity(sps.max_num_reorder_frames());
                    self.sps.insert(sps.id(), sps.clone());
                    self.ensure_session(&sps)?;
                    let session = self.session.as_mut().expect("session ensured");
                    if let Some(std) = self.tables.sps(sps.id(), &sps) {
                        let std_sps = [std];
                        let mut add = h264_params_add(&std_sps, &[])?;
                        let update =
                            vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
                        session.params_mut().update(&update)?;
                    }
                }
                H264Instruction::Pps(pps) => {
                    self.tables.insert_pps(&pps)?;
                    self.pps.insert(pps.id(), pps.clone());
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
                H264Instruction::Picture(picture) => {
                    out.extend(self.submit_picture(&picture)?);
                }
            }
        }
        Ok(out)
    }

    /// Decode raw Annex-B bytes, returning display-ready frames.
    fn run_bytes(&mut self, bytes: &[u8]) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.push(bytes) {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h264::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit)?);
            }
        }
        Ok(out)
    }
}

impl Decoder for H264Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data())
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        for raw in self.splitter.flush() {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h264::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit)?);
            }
        }
        if let Some(unit) = self.aus.flush() {
            out.extend(self.run_unit(unit)?);
        }
        out.extend(self.sink.reorder().flush());
        Ok(out)
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
    let lists = if std::env::var("VKPROBE_SWAP").is_ok() {
        [lists[1], lists[0]]
    } else {
        lists
    };
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
        let mut poc = reference.poc();
        let mut frame_num = reference.frame_num();
        if std::env::var("VKPROBE_POISON").is_ok() && *slot == 1 {
            poc = 999;
        }
        if std::env::var("VKPROBE_POISON_FN").is_ok() && *slot == 1 {
            frame_num = 9;
        }
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

/// Probe dump of one H.264 picture and its parameter sets.
fn dump_h264_probe(
    sps: &HashMap<u8, Rc<crate::parse::h264::Sps>>,
    pps: &HashMap<u8, Rc<crate::parse::h264::Pps>>,
    picture: &Picture,
    refs: &[(usize, &RefPicture)],
    setup_slot: usize,
    begin_len: usize,
) {
    for (id, sps) in sps {
        eprintln!(
            "sps id={id} prof={} level={} poct={} lsbminus4={} fnminus4={} ref-frames={} mbs={}x{} crop={:?} gaps={} direct8={} topbottom={} nonref={}",
            sps.profile_idc(),
            sps.level_idc(),
            sps.pic_order_cnt_type(),
            sps.log2_max_pic_order_cnt_lsb_minus4(),
            sps.log2_max_frame_num_minus4(),
            sps.max_num_ref_frames(),
            sps.pic_width_in_mbs_minus1(),
            sps.pic_height_in_map_units_minus1(),
            sps.crop(),
            sps.gaps_in_frame_num_allowed(),
            sps.direct_8x8_inference(),
            sps.offset_for_top_to_bottom_field(),
            sps.offset_for_non_ref_pic(),
        );
    }
    for (id, pps) in pps {
        eprintln!(
            "pps id={id} wpred={} wbipred={} l0def={} l1def={}",
            pps.weighted_pred(),
            pps.weighted_bipred_idc(),
            pps.num_ref_idx_l0_default_minus1(),
            pps.num_ref_idx_l1_default_minus1(),
        );
    }
    let l0: Vec<(u16, i32)> = picture
        .l0()
        .map(|list| {
            list.iter()
                .map(|entry| (entry.frame_num(), entry.poc()))
                .collect()
        })
        .unwrap_or_default();
    let l1: Vec<(u16, i32)> = picture
        .l1()
        .map(|list| {
            list.iter()
                .map(|entry| (entry.frame_num(), entry.poc()))
                .collect()
        })
        .unwrap_or_default();
    eprintln!(
        "pic fn={} poc={} idrpic={} ref={} intra={} idr={} l0={l0:?} l1={l1:?} slices={} bytes={}",
        picture.frame_num(),
        picture.poc(),
        picture.idr_pic_id(),
        picture.is_reference(),
        picture.is_intra(),
        picture.is_idr(),
        picture.slice_offsets().len(),
        picture.bytes().len(),
    );
    if std::env::var("VKPROBE_DUMP_BYTES").is_ok() {
        let path = format!("/tmp/vkbytes_{}.bin", picture.poc());
        let _ = std::fs::write(path, picture.bytes());
    }
    for (slot, reference) in refs {
        eprintln!(
            "  ref slot={slot} fn={} poc={} lt={}",
            reference.frame_num(),
            reference.poc(),
            reference.long_term().is_some(),
        );
    }
    eprintln!("  setup slot={setup_slot} begin={begin_len}");
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

/// Owned decode target and borrowed slot views of one picture submit.
struct SubmitPlan<'a, 'b, S> {
    /// Scratch identity for non-reference pictures.
    setup_id: ReferenceId,
    /// Claimed slot the picture decodes into.
    setup_slot: usize,
    /// Slot index of the setup slot.
    setup_index: i32,
    /// Standard reference info of the setup picture.
    setup_std: S,
    /// Active DPB slots at begin time, with codec metadata.
    begin: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Active reference slots for the decode, with codec metadata.
    refs: &'a [vk::VideoReferenceSlotInfoKHR<'b>],
    /// Decode target resource: the setup slot's layer.
    dst: vk::VideoPictureResourceInfoKHR<'static>,
}

/// Codec chain, submission and display frames of one H.264 picture.
fn finish_h264_submit(
    session: &mut Session,
    sink: &mut FrameSink,
    picture: &Picture,
    std_picture: &ash::vk::native::StdVideoDecodeH264PictureInfo,
    plan: &SubmitPlan<'_, '_, ash::vk::native::StdVideoDecodeH264ReferenceInfo>,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let codec = vk::VideoDecodeH264PictureInfoKHR {
        p_std_picture_info: std::ptr::from_ref(std_picture),
        slice_count: u32::try_from(picture.slice_offsets().len()).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "slice count",
            }
        })?,
        p_slice_offsets: picture.slice_offsets().as_ptr(),
        ..Default::default()
    };
    let setup_dpb = vk::VideoDecodeH264DpbSlotInfoKHR {
        p_std_reference_info: std::ptr::from_ref(&plan.setup_std),
        ..Default::default()
    };
    let mut setup = reference_slot(plan.setup_index, &plan.dst);
    setup.p_next = std::ptr::from_ref(&setup_dpb).cast();
    let mut begin_setup_slot = reference_slot(-1, &plan.dst);
    begin_setup_slot.p_next = std::ptr::from_ref(&setup_dpb).cast();
    let begin_all;
    let begin = if std::env::var("VKPROBE_BEGIN_SETUP").is_ok()
        && !plan
            .begin
            .iter()
            .any(|slot| slot.slot_index == plan.setup_index)
    {
        begin_all = plan
            .begin
            .iter()
            .chain(std::iter::once(&begin_setup_slot))
            .copied()
            .collect::<Vec<_>>();
        &begin_all
    } else {
        plan.begin
    };
    let sub = Submission {
        bytes: picture.bytes(),
        setup_slot: plan.setup_slot,
        reset: picture.resets_dpb(),
        begin,
        refs: plan.refs,
        setup: &setup,
        dst: plan.dst,
        picture_next: std::ptr::from_ref(&codec).cast(),
    };
    let nv12 = session.submit(&sub)?;
    if picture.is_reference() {
        session.set_meta(plan.setup_slot, Some(DpbMeta::H264(plan.setup_std)));
    } else {
        session.slots_mut().release(plan.setup_id);
    }
    sink.emit(session, &nv12, picture.poc(), picture.resets_dpb())
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

/// Codec chain, submission and display frames of one H.265 picture.
fn finish_h265_submit(
    session: &mut Session,
    sink: &mut FrameSink,
    body: &Picture,
    std_picture: &ash::vk::native::StdVideoDecodeH265PictureInfo,
    plan: &SubmitPlan<'_, '_, ash::vk::native::StdVideoDecodeH265ReferenceInfo>,
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let codec = vk::VideoDecodeH265PictureInfoKHR {
        p_std_picture_info: std::ptr::from_ref(std_picture),
        slice_segment_count: u32::try_from(body.slice_offsets().len()).map_err(|_| {
            ParseError::OutOfRange {
                syntax: "slice count",
            }
        })?,
        p_slice_segment_offsets: body.slice_offsets().as_ptr(),
        ..Default::default()
    };
    let setup_dpb = vk::VideoDecodeH265DpbSlotInfoKHR {
        p_std_reference_info: std::ptr::from_ref(&plan.setup_std),
        ..Default::default()
    };
    let mut setup = reference_slot(plan.setup_index, &plan.dst);
    setup.p_next = std::ptr::from_ref(&setup_dpb).cast();
    let sub = Submission {
        bytes: body.bytes(),
        setup_slot: plan.setup_slot,
        reset: body.resets_dpb(),
        begin: plan.begin,
        refs: plan.refs,
        setup: &setup,
        dst: plan.dst,
        picture_next: std::ptr::from_ref(&codec).cast(),
    };
    let nv12 = session.submit(&sub)?;
    if body.is_reference() {
        session.set_meta(plan.setup_slot, Some(DpbMeta::H265(plan.setup_std)));
    } else {
        session.slots_mut().release(plan.setup_id);
    }
    sink.emit_queued(session, &nv12, body.poc())
}

/// H.265 decoder over a negotiated Vulkan device.
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
            sink: FrameSink::new(device.shared().clone()),
            scratch: ReferenceId(1 << 30),
        })
    }

    /// Session for `sps`, rebuilding when the geometry changed.
    fn ensure_session(&mut self, sps: &crate::parse::h265::Sps) -> Result<(), VulkanError> {
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
            return Ok(());
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
        Ok(())
    }

    /// Submit one decoded picture, returning display-ready frames.
    fn submit_picture(
        &mut self,
        picture: &crate::parse::refs::H265Picture,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        use crate::session::std_h265::{fill_picture, fill_reference};
        let body = picture.picture();
        let echo = picture.echo();
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        retire_slots(session, &mut self.sink, body);
        let setup_id = body.id().unwrap_or_else(|| self.scratch.next());
        let setup_slot = session.slots_mut().claim(setup_id)?;
        let extent = {
            let (width, height) = session.geometry().coded();
            vk::Extent2D { width, height }
        };
        let refs = collect_h265_refs(session, echo)?;
        let ref_entries = fill_h265_refs(session, &refs, echo);
        let begin_entries = begin_h265_entries(session);
        let mut begin_resources = Vec::with_capacity(begin_entries.len());
        let mut begin_stds = Vec::with_capacity(begin_entries.len());
        for (slot, std, view) in &begin_entries {
            let layer = u32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "begin slot",
            })?;
            begin_resources.push(picture_resource(*view, extent, layer));
            begin_stds.push(*std);
        }
        let mut begin_dpb = Vec::with_capacity(begin_entries.len());
        for std in &begin_stds {
            begin_dpb.push(vk::VideoDecodeH265DpbSlotInfoKHR {
                p_std_reference_info: std::ptr::from_ref(std),
                ..Default::default()
            });
        }
        let mut begin_slots = Vec::with_capacity(begin_entries.len());
        for (index, (slot, _, _)) in begin_entries.iter().enumerate() {
            let slot_index = i32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "begin slot",
            })?;
            let mut info = reference_slot(slot_index, &begin_resources[index]);
            info.p_next = std::ptr::from_ref(&begin_dpb[index]).cast();
            begin_slots.push(info);
        }
        let mut ref_resources = Vec::with_capacity(ref_entries.len());
        let mut ref_stds = Vec::with_capacity(ref_entries.len());
        for (slot, std, view) in &ref_entries {
            let layer = u32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "decode slot",
            })?;
            ref_resources.push(picture_resource(*view, extent, layer));
            ref_stds.push(*std);
        }
        let mut ref_dpb = Vec::with_capacity(ref_entries.len());
        for std in &ref_stds {
            ref_dpb.push(vk::VideoDecodeH265DpbSlotInfoKHR {
                p_std_reference_info: std::ptr::from_ref(std),
                ..Default::default()
            });
        }
        let mut ref_slots = Vec::with_capacity(ref_entries.len());
        for (index, (slot, _, _)) in ref_entries.iter().enumerate() {
            let slot_index = i32::try_from(*slot).map_err(|_| ParseError::OutOfRange {
                syntax: "decode slot",
            })?;
            let mut info = reference_slot(slot_index, &ref_resources[index]);
            info.p_next = std::ptr::from_ref(&ref_dpb[index]).cast();
            ref_slots.push(info);
        }
        let setup_std = fill_reference(body.poc(), false);
        let (setup_resource, setup_index) = setup_target(session, extent, setup_slot)?;
        let std_picture = fill_picture(picture, session.slots())?;
        finish_h265_submit(
            session,
            &mut self.sink,
            body,
            &std_picture,
            &SubmitPlan {
                setup_id,
                setup_slot,
                setup_index,
                setup_std,
                begin: &begin_slots,
                refs: &ref_slots,
                dst: setup_resource,
            },
        )
    }

    /// Run one access unit through the context, submitting its picture.
    fn run_unit(
        &mut self,
        unit: crate::parse::au::AccessUnit<crate::parse::h265::Nalu>,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        use crate::parse::refs::H265Instruction;
        let mut out = Vec::new();
        for instruction in self.context.compile(unit) {
            match instruction {
                H265Instruction::Vps(vps) => {
                    self.tables.insert_vps(&vps)?;
                    self.vps.insert(vps.id(), vps.clone());
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
                H265Instruction::Sps(sps) => {
                    self.tables.insert_sps(&sps)?;
                    self.sink.reorder().set_capacity(sps.max_num_reorder_pics());
                    self.sps.insert(sps.id(), sps.clone());
                    self.ensure_session(&sps)?;
                    let session = self.session.as_mut().expect("session ensured");
                    if let Some(std) = self.tables.sps(sps.id(), &sps).transpose()? {
                        let std_sps = [std];
                        let mut add = crate::session::params::h265_params_add(&[], &std_sps, &[])?;
                        let update =
                            vk::VideoSessionParametersUpdateInfoKHR::default().push_next(&mut add);
                        session.params_mut().update(&update)?;
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
                    self.pps.insert(pps.id(), pps.clone());
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
                H265Instruction::Picture(picture) => {
                    out.extend(self.submit_picture(&picture)?);
                }
            }
        }
        Ok(out)
    }

    /// Decode raw Annex-B bytes, returning display-ready frames.
    fn run_bytes(&mut self, bytes: &[u8]) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        for raw in self.splitter.push(bytes) {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h265::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit)?);
            }
        }
        Ok(out)
    }
}

impl Decoder for H265Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data())
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        let mut out = Vec::new();
        for raw in self.splitter.flush() {
            let parsed = self.parser.parse(&raw)?;
            let nalu = crate::parse::h265::Nalu::new(parsed);
            if let Some(unit) = self.aus.put(nalu) {
                out.extend(self.run_unit(unit)?);
            }
        }
        if let Some(unit) = self.aus.flush() {
            out.extend(self.run_unit(unit)?);
        }
        out.extend(self.sink.reorder().flush());
        Ok(out)
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

    use rust_video::{GpuExport, GpuFrame, PixelFormat, SharedDevice};
    use rust_video_caps::common::PictureSize;

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
        }
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
