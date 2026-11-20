//! H.264 decoder over a negotiated Vulkan device.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use ash::vk;
use rust_video_caps::delivery::{PhaseTiming, SplitDecode, SubmitStats};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

use super::dpb::{H264Context, H264Instruction};
use super::params::{fill_picture, fill_reference, H264Tables};
use crate::au::{AccessUnit, AuSplitter};
use crate::decoder::{
    apply_armed_fault, claim_slot, collect_oldest_timeout, collect_ready, drain_pending,
    plan_submit, retire_slots, setup_target, ChainBufs, DrainMode, FrameSink, NalFrontend,
    PendingPicture, RunCore, SubmitPlan, VulkanFrame,
};
use crate::device::VulkanDevice;
use crate::dpb::{Picture, RefPicture, ReferenceId};
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::session::decode::{reference_slot, DpbMeta, Session};
use crate::session::geometry::{h264_profile_to_std, SessionGeometry};
use crate::session::params::{
    h264_params_add, h264_params_create, H264ProfileChain, ParamsTemplate, H264_DECODE_STD_NAME,
};

/// H.264 decoder over a negotiated Vulkan device.
///
/// Thread-local: parser state holds `Rc` tables, so the decoder stays on
/// its creating thread while queue submission takes the device lock.
pub struct H264Decoder<'a> {
    /// Negotiated device, borrowed for the decoder's life.
    device: &'a VulkanDevice,
    /// Annex-B splitter.
    splitter: crate::h264::NaluSplitter,
    /// H.264 parser.
    parser: crate::h264::H264Parser,
    /// Access-unit splitter.
    aus: AuSplitter<crate::h264::ParsedNalu>,
    /// Reference context.
    context: H264Context,
    /// Standard-structure tables.
    tables: H264Tables,
    /// Parsed sequence sets by id.
    sps: HashMap<u8, Rc<crate::h264::Sps>>,
    /// Parsed picture sets by id.
    pps: HashMap<u8, Rc<crate::h264::Pps>>,
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
    /// Completions queued by submits, drained by polls.
    ready: Vec<VulkanFrame>,
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
            splitter: crate::h264::NaluSplitter::new(),
            parser: crate::h264::H264Parser::new(),
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
            ready: Vec::new(),
            stats: SubmitStats::new(),
            fault: false,
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
        sps: &crate::h264::Sps,
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
    type Raw = crate::h264::Nalu;
    type Nalu = crate::h264::ParsedNalu;
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

impl SplitDecode for H264Decoder<'_> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn try_submit(&mut self, packet: &Packet) -> Result<bool, VulkanError> {
        if self.pending.len() + self.sink.launched_len() >= crate::session::slots::SUBMIT_SLOTS {
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

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::frame::GpuExport;

    use crate::error::VulkanError;

    /// Kind tag of one instruction, asserting a zero reorder depth on sets.
    fn tag(instruction: &crate::h264::dpb::H264Instruction) -> &'static str {
        use crate::h264::dpb::H264Instruction;
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
        splitter: &mut crate::h264::NaluSplitter,
        parser: &mut crate::h264::H264Parser,
        aus: &mut crate::au::AuSplitter<crate::h264::ParsedNalu>,
        context: &mut crate::h264::dpb::H264Context,
        bytes: &[u8],
    ) -> Vec<&'static str> {
        let mut tags = Vec::new();
        let mut run_nal = |raw: crate::h264::Nalu| {
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

        use crate::au::AuSplitter;
        use crate::h264::dpb::H264Context;
        use crate::h264::{H264Parser, NaluSplitter};

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
    fn caps_agree_with_frame_exports() {
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;
        use rust_video_harness::H264Synth;

        use crate::device::VulkanDevice;
        use crate::ladder::CodecOp;

        let device = match VulkanDevice::open_or_skip(CodecOp::H264Decode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: h264 on this box: {reason}");
                return;
            }
            Err(error) => panic!("negotiation failed: {error}"),
        };
        let mut decoder = match super::H264Decoder::open(&device) {
            Ok(decoder) => decoder,
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM h264 open: {error}");
                return;
            }
            Err(error) => panic!("engine starts: {error}"),
        };
        let caps = decoder.caps();
        assert_eq!(caps.share(), device.share_mechanism());
        assert!(!caps.decode_export());
        assert!(!caps.encode_import());
        let mut frames = Vec::new();
        for raw in H264Synth::annex_b_packets() {
            let packet = Packet::new(Bytes::copy_from_slice(&raw));
            match decoder.decode(&packet) {
                Ok(emitted) => frames.extend(emitted),
                Err(error) if error.is_out_of_device_memory() => {
                    println!("declined:OOM h264 decode: {error}");
                    return;
                }
                Err(error) => panic!("decode runs: {error}"),
            }
        }
        match decoder.flush() {
            Ok(drained) => frames.extend(drained),
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM h264 flush: {error}");
                return;
            }
            Err(error) => panic!("flush runs: {error}"),
        }
        assert!(!frames.is_empty());
        let caps = decoder.caps();
        assert_eq!(caps.share(), device.share_mechanism());
        for frame in &frames {
            assert_ne!(frame.nv12(), []);
            assert!(matches!(frame.export(), GpuExport::Opaque { .. }));
        }
        let mut surface_decoder = match super::H264Decoder::open(&device) {
            Ok(decoder) => decoder,
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM h264 surface open: {error}");
                return;
            }
            Err(error) => panic!("surface engine starts: {error}"),
        };
        let mut surfaces = Vec::new();
        for raw in H264Synth::annex_b_packets() {
            let packet = Packet::new(Bytes::copy_from_slice(&raw));
            match surface_decoder.decode_surface(&packet) {
                Ok(emitted) => surfaces.extend(emitted),
                Err(error) if error.is_out_of_device_memory() => {
                    println!("declined:OOM h264 surface decode: {error}");
                    return;
                }
                Err(error) => panic!("surface decode runs: {error}"),
            }
        }
        match surface_decoder.flush_surface() {
            Ok(drained) => surfaces.extend(drained),
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM h264 surface flush: {error}");
                return;
            }
            Err(error) => panic!("surface flush runs: {error}"),
        }
        assert!(!surfaces.is_empty());
        if caps.share().is_none() {
            for surface in &surfaces {
                assert!(
                    matches!(surface.export(), GpuExport::Opaque { .. }),
                    "surface handle without advertised share"
                );
            }
        }
    }
}
