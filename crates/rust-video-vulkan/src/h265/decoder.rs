//! H.265 decoder over a negotiated Vulkan device.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use ash::vk;
use rust_video_caps::delivery::{PhaseTiming, SplitDecode, SubmitStats};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

use super::dpb::{H265Context, H265Instruction, H265Picture, RpsEcho};
use crate::au::{AccessUnit, AuSplitter};
use crate::decoder::{
    apply_armed_fault, claim_slot, collect_oldest_timeout, collect_ready, drain_pending,
    plan_submit, ref_poc, retire_slots, setup_target, ChainBufs, DrainMode, FrameSink, NalFrontend,
    PendingPicture, RunCore, SubmitPlan, VulkanFrame,
};
use crate::device::VulkanDevice;
use crate::dpb::ReferenceId;
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::session::decode::{reference_slot, DpbMeta, Session};
use crate::session::geometry::SessionGeometry;
use crate::session::params::ParamsTemplate;
use crate::session::slots::{MAX_SUBMIT_SLOTS, SUBMIT_SLOTS};

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
    use super::params::fill_reference;
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
    splitter: crate::h265::NaluSplitter,
    /// H.265 parser.
    parser: crate::h265::H265Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::h265::ParsedNalu>,
    /// Reference context.
    context: H265Context,
    /// Standard-structure tables.
    tables: super::params::H265Tables,
    /// Parsed video sets by id.
    vps: HashMap<u8, Rc<crate::h265::Vps>>,
    /// Parsed sequence sets by id.
    sps: HashMap<u8, Rc<crate::h265::Sps>>,
    /// Parsed picture sets by id.
    pps: HashMap<u8, Rc<crate::h265::Pps>>,
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
    /// Completions queued by submits, drained by polls.
    ready: Vec<VulkanFrame>,
    /// Accumulated submit-core gauges.
    stats: SubmitStats,
    /// Armed fault for the next submit, applied once a session runs.
    fault: bool,
    /// Submitted pictures before sends refuse.
    depth: usize,
}

impl Drop for H265Decoder<'_> {
    fn drop(&mut self) {
        self.sink.readback.shutdown();
    }
}

impl<'a> H265Decoder<'a> {
    /// Submitted pictures before sends refuse, by default.
    pub const DEFAULT_DEPTH: usize = SUBMIT_SLOTS;
    /// Submitted pictures before sends refuse, at most.
    pub const MAX_DEPTH: usize = MAX_SUBMIT_SLOTS;

    /// Decoder over `device`, negotiated for H.265 decode.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated a different operation.
    pub fn open(device: &'a VulkanDevice) -> Result<Self, VulkanError> {
        Self::open_depth(device, Self::DEFAULT_DEPTH)
    }

    /// Decoder over `device` submitting at most `depth` pictures.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `device` negotiated a different operation.
    ///
    /// # Arguments
    ///
    /// * `device` - Vulkan device behind decode sessions.
    /// * `depth` - submitted pictures before sends refuse; clamped to `1..=MAX_DEPTH`.
    pub fn open_depth(device: &'a VulkanDevice, depth: usize) -> Result<Self, VulkanError> {
        if device.op() != CodecOp::H265Decode {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            device,
            splitter: crate::h265::NaluSplitter::new(),
            parser: crate::h265::H265Parser::new(),
            aus: AuSplitter::new(),
            context: H265Context::new(),
            tables: super::params::H265Tables::new(),
            vps: HashMap::new(),
            sps: HashMap::new(),
            pps: HashMap::new(),
            session: None,
            sink: FrameSink::new(device.shared().clone(), device.owned()),
            scratch: ReferenceId(1 << 30),
            begin_bufs: ChainBufs::new(),
            ref_bufs: ChainBufs::new(),
            pending: Vec::new(),
            ready: Vec::new(),
            stats: SubmitStats::new(),
            fault: false,
            depth: depth.clamp(1, Self::MAX_DEPTH),
        })
    }

    /// Submit-core gauges of the decoder.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        self.stats
    }

    /// Submitted but uncollected picture count.
    #[must_use]
    pub fn inflight_len(&self) -> usize {
        self.pending.len() + self.sink.launched_len()
    }

    /// Logged phase timings, oldest first, clearing the log.
    pub fn take_timings(&mut self) -> Vec<PhaseTiming> {
        self.sink.take_timings()
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
        sps: &crate::h265::Sps,
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
            self.depth,
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
    type Raw = crate::h265::Nalu;
    type Nalu = crate::h265::ParsedNalu;
    type Picture = H265Picture;

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
            depth: self.depth,
        }
    }

    fn submit_picture(&mut self, picture: &H265Picture, gpu: bool) -> Result<(), VulkanError> {
        use super::params::{fill_picture, fill_reference};
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

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(self.device.share_mechanism(), false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false, DrainMode::Sync)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner(false)
    }
}

impl SplitDecode for H265Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn try_submit(&mut self, packet: &Packet) -> Result<bool, VulkanError> {
        if self.pending.len() + self.sink.launched_len() >= self.depth {
            return Ok(false);
        }
        let frames = self.run_bytes(packet.data(), false, DrainMode::Overlap)?;
        self.ready.extend(frames);
        Ok(true)
    }

    fn poll(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = std::mem::take(&mut self.ready);
        out.extend(collect_ready(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )?);
        Ok(out)
    }

    fn poll_wait(&mut self, timeout: Duration) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = std::mem::take(&mut self.ready);
        if !out.is_empty() {
            return Ok(out);
        }
        out.extend(collect_ready(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
        )?);
        if !out.is_empty() {
            return Ok(out);
        }
        if self.pending.is_empty() && self.sink.launched_len() == 0 {
            std::thread::sleep(timeout);
            return Ok(out);
        }
        match collect_oldest_timeout(
            &mut self.session,
            &mut self.sink,
            &mut self.pending,
            &mut self.stats,
            timeout,
        )? {
            Some(frames) => {
                out.extend(frames);
                Ok(out)
            }
            None => Ok(out),
        }
    }

    fn flush_split(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = std::mem::take(&mut self.ready);
        out.extend(self.flush_inner(false)?);
        Ok(out)
    }

    fn stats(&self) -> SubmitStats {
        self.stats
    }
}
