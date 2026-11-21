//! AV1 decoder over a negotiated Vulkan device.

use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ash::vk;
use bytes::{Bytes, BytesMut};
use rust_video_caps::common::PictureSize;
use rust_video_caps::delivery::{PhaseTiming, SplitDecode, SubmitStats};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_caps::transcode::GpuCaps;

use super::dpb::{Av1Context, Av1Instruction, Av1Picture};
use crate::decoder::{
    apply_armed_fault, claim_slot, fold_collection, launch_waited, plan_submit, setup_target,
    ChainBufs, DrainMode, FrameSink, LaunchedPicture, PendingPicture, SubmitPlan, SurfaceLayout,
    VulkanFrame,
};
use crate::device::VulkanDevice;
use crate::dpb::ReferenceId;
use crate::error::{ParseError, VulkanError};
use crate::ladder::CodecOp;
use crate::raw::ExportPair;
use crate::session::decode::{as_extent, reference_slot, DpbMeta, Session};
use crate::session::geometry::SessionGeometry;
use crate::session::params::ParamsTemplate;
use crate::session::slots::{MAX_SUBMIT_SLOTS, SUBMIT_SLOTS};

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

/// Held show-existing display, built now or when its frame lands.
enum HeldKind {
    /// Display frames built at the replay.
    Ready(Vec<VulkanFrame>),
    /// Frame to show, unlanded at the replay.
    Pending {
        /// Stable identity of the stored frame.
        id: ReferenceId,
        /// Display frames skip the readback on the GPU path.
        gpu: bool,
        /// Display order count of the replay.
        poc: i32,
    },
}

/// Show-existing display held past earlier-submitted pictures.
///
/// Arrival order is display order at reorder depth zero, so the replay
/// emits only after everything submitted before it lands.
struct HeldReplay {
    /// Display payload, ready or awaiting its frame.
    kind: HeldKind,
    /// Ids leaving the DPB with this display.
    releases: Vec<ReferenceId>,
    /// Submit order at the replay; smaller orders precede it.
    seq: u64,
}

/// AV1 decoder over a negotiated Vulkan device.
///
/// Thread-local: parser state holds `Rc` tables, so the decoder stays on
/// its creating thread while queue submission takes the device lock.
pub struct Av1Decoder<'a> {
    /// Negotiated device, borrowed for the decoder's life.
    device: &'a VulkanDevice,
    /// Reference context.
    context: Av1Context,
    /// Standard-structure tables.
    tables: super::params::Av1Tables,
    /// Active sequence header.
    seq: Option<Rc<crate::av1::SequenceHeader>>,
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
    /// Completions queued by submits, drained by polls.
    ready: Vec<VulkanFrame>,
    /// Retained display frames by reference identity.
    shown: ShowStore,
    /// Display order counter for show-existing frames.
    show_seq: i32,
    /// Store evictions withheld for outstanding replays.
    deferred_evicts: Vec<ReferenceId>,
    /// Submit order counter behind holdback.
    submit_seq: u64,
    /// Replays awaiting earlier-submitted pictures.
    held: VecDeque<HeldReplay>,
    /// Landed frames newer than an outstanding replay, by submit order.
    emit_buf: BTreeMap<u64, Vec<VulkanFrame>>,
    /// Accumulated submit-core gauges.
    stats: SubmitStats,
    /// Armed fault for the next submit, applied once a session runs.
    fault: bool,
    /// Submitted pictures before sends refuse.
    depth: usize,
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
    /// Submitted pictures before sends refuse, by default.
    pub const DEFAULT_DEPTH: usize = SUBMIT_SLOTS;
    /// Submitted pictures before sends refuse, at most.
    pub const MAX_DEPTH: usize = MAX_SUBMIT_SLOTS;

    /// Decoder over `device`, negotiated for AV1 decode.
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
        if device.op() != CodecOp::Av1Decode {
            return Err(VulkanError::OverCaps { what: "codec op" });
        }
        Ok(Self {
            device,
            context: Av1Context::new(),
            tables: super::params::Av1Tables::new(),
            seq: None,
            session: None,
            sink: FrameSink::new(device.shared().clone(), device.owned()),
            scratch: ReferenceId(1 << 30),
            begin_bufs: ChainBufs::new(),
            ref_bufs: ChainBufs::new(),
            pending: Vec::new(),
            ready: Vec::new(),
            shown: ShowStore::new(),
            show_seq: 0,
            deferred_evicts: Vec::new(),
            submit_seq: 0,
            held: VecDeque::new(),
            emit_buf: BTreeMap::new(),
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

    /// Session for `seq` at `display`, recreated on geometry change.
    fn ensure_session(
        &mut self,
        seq: &Rc<crate::av1::SequenceHeader>,
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
            self.depth,
        )?;
        self.session = Some(session);
        Ok(())
    }

    /// Order hints, sign bias, and expected ids of `picture` references.
    fn resolve_ref_state(&self, picture: &Av1Picture) -> Result<Av1RefState, VulkanError> {
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
            for name in 0..crate::av1::REFS_PER_FRAME {
                if crate::av1::relative_dist(
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
            for name in 0..crate::av1::REFS_PER_FRAME {
                expected[name + 1] = super::params::expected_frame_id(seq, header, name);
            }
        }
        Ok(Av1RefState {
            order_hints,
            bias,
            expected,
        })
    }

    /// Queue-submit one decoded picture without waiting.
    fn submit_picture(&mut self, picture: &Av1Picture, gpu: bool) -> Result<(), VulkanError> {
        use super::params::{fill_reference, Av1PictureStd};
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
            shown,
            submit_seq,
            held,
            deferred_evicts,
            ..
        } = self;
        let Some(session) = session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let body = picture.picture();
        let header = picture.header();
        Self::evict_partitioned(
            &mut *session,
            &mut *shown,
            &mut *deferred_evicts,
            held,
            picture.pre(),
        );
        let setup_id = body.id().unwrap_or_else(|| scratch.next());
        let setup_slot = claim_slot(session.slots_mut(), setup_id)?;
        let extent = as_extent(session.geometry().coded());
        let (ref_slots, begin_slots) =
            fill_av1_chains(session, picture, ref_bufs, begin_bufs, extent)?;
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
                meta: DpbMeta::Av1(setup_std),
                flush_on_reset: true,
                emit: header.show_frame(),
            },
            gpu,
        )
        .inspect_err(|_| release_ids(session, shown, picture.post()))?;
        Self::evict_partitioned(
            &mut *session,
            &mut *shown,
            &mut *deferred_evicts,
            held,
            picture.post(),
        );
        Self::enqueue(submit_seq, pending, submitted);
        Ok(())
    }

    /// Submit one picture, then collect per `mode`.
    fn run_picture(
        &mut self,
        picture: &Av1Picture,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        if mode == DrainMode::Overlap {
            while self.pending.len() >= self.depth {
                out.extend(self.collect_one()?);
            }
        }
        self.submit_picture(picture, gpu)?;
        match mode {
            DrainMode::Sync => {
                if self.pending.len() >= self.depth {
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
        unit: &crate::av1::TemporalUnit<'_>,
        gpu: bool,
        mode: DrainMode,
    ) -> Result<Vec<VulkanFrame>, VulkanError> {
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
                        self.deferred_evicts.clear();
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
                    // Refuse cross-path replays before touching state.
                    let foreign = self.shown.get(id).is_some_and(|stored| {
                        if gpu {
                            stored.gpu.is_none()
                        } else {
                            stored.cpu.is_none()
                        }
                    });
                    if foreign {
                        return Err(VulkanError::Unsupported {
                            what: "av1 show-existing across cpu/gpu paths".to_owned(),
                        });
                    }
                    if releases.contains(&id) {
                        out.extend(self.drain()?);
                        if let Some(session) = self.session.as_mut() {
                            release_ids(session, &mut self.shown, &releases);
                        } else {
                            self.shown.evict(&releases);
                        }
                        return Err(ParseError::MissingSet {
                            id: u32::try_from(id.0).unwrap_or(0),
                        }
                        .into());
                    }
                    let stored = self.shown.get(id).map(|stored| {
                        (
                            stored.size,
                            stored.cpu.clone(),
                            stored.gpu.clone(),
                            stored.layout,
                        )
                    });
                    let poc = self.show_seq;
                    self.show_seq = self.show_seq.wrapping_add(1);
                    let seq = self.submit_seq;
                    if let Some((size, cpu, gpu_pair, layout)) = stored {
                        let (cpu, pair) = if gpu {
                            (None, gpu_pair.as_ref())
                        } else {
                            (cpu.as_ref(), None)
                        };
                        let frames = self.sink.emit_stored(size, cpu, pair, poc, layout)?;
                        self.held.push_back(HeldReplay {
                            kind: HeldKind::Ready(frames),
                            releases,
                            seq,
                        });
                    } else if self.inflight_has(id) {
                        self.held.push_back(HeldReplay {
                            kind: HeldKind::Pending { id, gpu, poc },
                            releases,
                            seq,
                        });
                    } else {
                        return Err(ParseError::MissingSet {
                            id: u32::try_from(id.0).unwrap_or(0),
                        }
                        .into());
                    }
                    out.extend(self.release_ready()?);
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
            // Reachable refs only: future replays name live slots, outstanding
            // ones await; anything else orphans its export pair in the pool.
            let live = self.context.is_live(id) || held_awaits(&self.held, id);
            if live {
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
        let mut out = self.collect_one_inner()?;
        out.extend(self.release_ready()?);
        Ok(out)
    }

    /// Oldest submitted picture without releasing held replays.
    fn collect_one_inner(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
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
        let seq = picture.seq;
        let start = Instant::now();
        session.wait_pending(picture.pending)?;
        let engine_wait = start.elapsed();
        let emit = picture.emit;
        let show_id = picture.show_id;
        let pair = picture.lease.as_ref().map(|lease| lease.pair().clone());
        let frames = launch_waited(
            session,
            &mut self.sink,
            &mut self.stats,
            picture,
            engine_wait,
        )?;
        // Inline surface landings retain like launched copies.
        let landed = if pair.is_some() {
            self.finish_retained(emit, show_id, pair.as_ref(), frames)
        } else {
            frames
        };
        let mut out = self.emit_or_buffer(seq, landed);
        if !out.is_empty() || self.sink.launched.is_empty() {
            return Ok(out);
        }
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
        let (bytes, copy) = done.into_parts();
        let seq = launched.picture.seq;
        self.land_retained(launched, bytes, copy)
            .map(|frames| Some(self.emit_or_buffer(seq, frames)))
    }

    /// Oldest launched copy into display frames, waiting for it.
    fn collect_launched_one(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let Some(launched) = self.sink.launched.pop_front() else {
            return Ok(Vec::new());
        };
        if let Some(done) = self.sink.readback.collect() {
            let (bytes, copy) = done.into_parts();
            let seq = launched.picture.seq;
            let frames = self.land_retained(launched, bytes, copy)?;
            return Ok(self.emit_or_buffer(seq, frames));
        }
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let seq = launched.picture.seq;
        let mut buffer = self.sink.pooled(launched.size, launched.coded);
        let copy_start = Instant::now();
        session.copy_pending_into(launched.picture.pending, &mut buffer)?;
        let frames = self.land_retained(launched, buffer, copy_start.elapsed())?;
        Ok(self.emit_or_buffer(seq, frames))
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
        let timing = PhaseTiming::new(
            self.sink.next_timing_seq(),
            picture.submit,
            engine_wait,
            copy,
            picture.submitted_at,
            Instant::now(),
        );
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        fold_collection(session, &mut self.sink, &mut self.stats, timing);
        Ok(self.finish_retained(emit, show_id, pair.as_ref(), frames))
    }

    /// Collect the oldest submitted picture, waiting up to `timeout` for it.
    ///
    /// `None` reports the lapsed budget with the picture still pending.
    /// Empty when nothing is submitted. Launched-only work waits for
    /// the oldest copy within the budget instead of returning at once.
    fn collect_one_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Vec<VulkanFrame>>, VulkanError> {
        match self.collect_one_timeout_inner(timeout)? {
            Some(mut frames) => {
                frames.extend(self.release_ready()?);
                Ok(Some(frames))
            }
            None => Ok(None),
        }
    }

    /// Oldest submitted picture within `timeout`, without held replays.
    fn collect_one_timeout_inner(
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
            if self.sink.launched.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let Some(done) = self.sink.readback.collect_timeout(timeout) else {
                return Ok(None);
            };
            let Some(launched) = self.sink.launched.pop_front() else {
                debug_assert!(false, "landed copy without launched picture");
                return Ok(Some(Vec::new()));
            };
            let (bytes, copy) = done.into_parts();
            let seq = launched.picture.seq;
            let frames = self.land_retained(launched, bytes, copy)?;
            return Ok(Some(self.emit_or_buffer(seq, frames)));
        }
        let Some(session) = self.session.as_mut() else {
            return Err(VulkanError::NoSession);
        };
        let picture = self.pending.remove(0);
        let seq = picture.seq;
        let budget = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        let start = Instant::now();
        if !session.wait_pending_timeout(picture.pending, budget)? {
            self.pending.insert(0, picture);
            return Ok(None);
        }
        let engine_wait = start.elapsed();
        let emit = picture.emit;
        let show_id = picture.show_id;
        let pair = picture.lease.as_ref().map(|lease| lease.pair().clone());
        let frames = launch_waited(
            session,
            &mut self.sink,
            &mut self.stats,
            picture,
            engine_wait,
        )?;
        // Inline surface landings retain like launched copies.
        let landed = if pair.is_some() {
            self.finish_retained(emit, show_id, pair.as_ref(), frames)
        } else {
            frames
        };
        let frames = self.emit_or_buffer(seq, landed);
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
        let (bytes, copy) = done.into_parts();
        let seq = launched.picture.seq;
        let frames = self.land_retained(launched, bytes, copy)?;
        Ok(Some(self.emit_or_buffer(seq, frames)))
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
        while !self.pending.is_empty() || !self.sink.launched.is_empty() || !self.held.is_empty() {
            if self.pending.is_empty() && self.sink.launched.is_empty() {
                out.extend(self.release_ready()?);
                continue;
            }
            out.extend(self.collect_one()?);
        }
        Ok(out)
    }

    /// Landed `frames` of submit order `seq`, buffered past held replays.
    ///
    /// Older than every outstanding replay emits at once; newer waits in
    /// submit order until the replays ahead of it release.
    fn emit_or_buffer(&mut self, seq: u64, frames: Vec<VulkanFrame>) -> Vec<VulkanFrame> {
        if frames.is_empty() {
            return frames;
        }
        let newer = self.held.front().is_some_and(|head| seq >= head.seq);
        if !newer {
            return frames;
        }
        self.emit_buf.entry(seq).or_default().extend(frames);
        Vec::new()
    }

    /// Evict `ids`, withholding store bytes awaited by held replays.
    ///
    /// Slots release at once; bytes stay until the last awaiting replay
    /// resolves, so a land between hold and resolve still finds them.
    fn evict_partitioned(
        session: &mut Session,
        shown: &mut ShowStore,
        deferred: &mut Vec<ReferenceId>,
        held: &VecDeque<HeldReplay>,
        ids: &[ReferenceId],
    ) {
        let (awaited, free): (Vec<ReferenceId>, Vec<ReferenceId>) =
            ids.iter().copied().partition(|id| held_awaits(held, *id));
        release_ids(session, shown, &free);
        release_slots(session, &awaited);
        deferred.extend(awaited);
    }

    /// Store-evict deferred ids no held replay awaits.
    fn sweep_deferred(&mut self) {
        let mut drop = Vec::new();
        for id in &self.deferred_evicts {
            if !held_awaits(&self.held, *id) {
                drop.push(*id);
            }
        }
        if !drop.is_empty() {
            self.deferred_evicts.retain(|id| !drop.contains(id));
            self.shown.evict(&drop);
        }
    }

    /// Tag `submitted` in submit order and queue it.
    fn enqueue(
        submit_seq: &mut u64,
        pending: &mut Vec<PendingPicture>,
        mut submitted: PendingPicture,
    ) {
        submitted.seq = *submit_seq;
        *submit_seq = submit_seq.wrapping_add(1);
        pending.push(submitted);
    }

    /// True when `id` rides an unlanded picture.
    fn inflight_has(&self, id: ReferenceId) -> bool {
        self.pending
            .iter()
            .any(|picture| picture.show_id == Some(id))
            || self
                .sink
                .launched
                .iter()
                .any(|launched| launched.picture.show_id == Some(id))
    }

    /// Replays whose earlier-submitted pictures all landed, in order.
    ///
    /// Each release applies its deferred DPB eviction, then emits its
    /// frames with buffered newer frames in submit order.
    fn release_ready(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = Vec::new();
        loop {
            let Some(replay) = self.held.pop_front() else {
                break;
            };
            let HeldReplay {
                kind,
                releases,
                seq,
            } = replay;
            let older = self.pending.iter().any(|picture| picture.seq < seq)
                || self
                    .sink
                    .launched
                    .iter()
                    .any(|launched| launched.picture.seq < seq);
            if older {
                self.held.push_front(HeldReplay {
                    kind,
                    releases,
                    seq,
                });
                break;
            }
            let frames = match kind {
                HeldKind::Ready(frames) => frames,
                HeldKind::Pending { id, gpu, poc } => {
                    let stored = self.shown.get(id).map(|stored| {
                        (
                            stored.size,
                            stored.cpu.clone(),
                            stored.gpu.clone(),
                            stored.layout,
                        )
                    });
                    let Some((size, cpu, gpu_pair, layout)) = stored else {
                        if self.inflight_has(id) {
                            self.held.push_front(HeldReplay {
                                kind: HeldKind::Pending { id, gpu, poc },
                                releases,
                                seq,
                            });
                            break;
                        }
                        return Err(ParseError::MissingSet {
                            id: u32::try_from(id.0).unwrap_or(0),
                        }
                        .into());
                    };
                    let foreign = if gpu {
                        gpu_pair.is_none()
                    } else {
                        cpu.is_none()
                    };
                    if foreign {
                        return Err(VulkanError::Unsupported {
                            what: "av1 show-existing across cpu/gpu paths".to_owned(),
                        });
                    }
                    let (cpu, pair) = if gpu {
                        (None, gpu_pair.as_ref())
                    } else {
                        (cpu.as_ref(), None)
                    };
                    self.sink.emit_stored(size, cpu, pair, poc, layout)?
                }
            };
            if let Some(session) = self.session.as_mut() {
                release_ids(session, &mut self.shown, &releases);
            } else {
                self.shown.evict(&releases);
            }
            self.emit_buf.insert(seq, frames);
            let next = self.held.front().map_or(u64::MAX, |head| head.seq);
            let ready: Vec<u64> = self
                .emit_buf
                .keys()
                .copied()
                .take_while(|order| *order < next)
                .collect();
            for order in ready {
                out.extend(self.emit_buf.remove(&order).unwrap_or_default());
            }
            self.sweep_deferred();
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
        let obus = crate::av1::split_obus(bytes)?;
        for unit in crate::av1::temporal_units(obus) {
            out.extend(self.run_unit(&unit, gpu, mode)?);
        }
        match mode {
            DrainMode::Sync => {
                // Pipeline one deep: the kept submit overlaps the next
                // call instead of draining fully here.
                while self.pending.len() > 1 {
                    out.extend(self.collect_one()?);
                }
                out.extend(self.collect_ready_pictures()?);
            }
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

    fn caps(&self) -> GpuCaps {
        GpuCaps::new(self.device.share_mechanism(), false, false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
        self.run_bytes(packet.data(), false, DrainMode::Sync)
    }

    fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
        self.flush_inner()
    }
}

impl SplitDecode for Av1Decoder<'_> {
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
        out.extend(self.collect_ready_pictures()?);
        Ok(out)
    }

    fn poll_wait(&mut self, timeout: Duration) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = std::mem::take(&mut self.ready);
        if !out.is_empty() {
            return Ok(out);
        }
        out.extend(self.collect_ready_pictures()?);
        if !out.is_empty() {
            return Ok(out);
        }
        if self.pending.is_empty() && self.sink.launched_len() == 0 {
            std::thread::sleep(timeout);
            return Ok(out);
        }
        match self.collect_one_timeout(timeout)? {
            Some(frames) => {
                out.extend(frames);
                Ok(out)
            }
            None => Ok(out),
        }
    }

    fn flush_split(&mut self) -> Result<Vec<VulkanFrame>, VulkanError> {
        let mut out = std::mem::take(&mut self.ready);
        out.extend(self.flush_inner()?);
        Ok(out)
    }

    fn stats(&self) -> SubmitStats {
        self.stats
    }
}

/// True when a held replay awaits the landing frame of `id`.
fn held_awaits(held: &VecDeque<HeldReplay>, id: ReferenceId) -> bool {
    held.iter()
        .any(|held| matches!(&held.kind, HeldKind::Pending { id: want, .. } if *want == id))
}

/// Release `ids` from the session DPB, clearing metadata.
fn release_slots(session: &mut Session, ids: &[ReferenceId]) {
    for id in ids {
        if let Some(slot) = session.slots().slot_of(*id) {
            session.set_meta(slot, None);
        }
        session.slots_mut().release(*id);
    }
}

/// Release `ids` from the session DPB, clearing metadata and retained frames.
fn release_ids(session: &mut Session, shown: &mut ShowStore, ids: &[ReferenceId]) {
    shown.evict(ids);
    release_slots(session, ids);
}

/// Reference name slot indices of `picture`, negative when unclaimed.
///
/// # Errors
///
/// Returns `Err` when a slot id exceeds its field width.
fn av1_name_indices(
    session: &Session,
    picture: &Av1Picture,
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
    picture: &'a Av1Picture,
    std_sets: &'a super::params::Av1PictureStd,
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
/// Reference plus begin slot chains over the live DPB.
fn fill_av1_chains<'b>(
    session: &Session,
    picture: &Av1Picture,
    ref_bufs: &'b mut ChainBufs<
        ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
        vk::VideoDecodeAV1DpbSlotInfoKHR<'static>,
    >,
    begin_bufs: &'b mut ChainBufs<
        ash::vk::native::StdVideoDecodeAV1ReferenceInfo,
        vk::VideoDecodeAV1DpbSlotInfoKHR<'static>,
    >,
    extent: vk::Extent2D,
) -> Result<
    (
        Vec<vk::VideoReferenceSlotInfoKHR<'b>>,
        Vec<vk::VideoReferenceSlotInfoKHR<'b>>,
    ),
    VulkanError,
> {
    let refs = collect_av1_refs(session, picture)?;
    let ref_entries = fill_av1_refs(session, &refs);
    let begin_entries = begin_av1_entries(session);
    let dpb_of = |std| vk::VideoDecodeAV1DpbSlotInfoKHR {
        p_std_reference_info: std,
        ..Default::default()
    };
    ref_bufs.fill(&ref_entries, extent, "decode slot", dpb_of)?;
    begin_bufs.fill(&begin_entries, extent, "begin slot", dpb_of)?;
    Ok((
        ref_bufs.slots(&ref_entries, "decode slot")?,
        begin_bufs.slots(&begin_entries, "begin slot")?,
    ))
}

fn collect_av1_refs(
    session: &Session,
    picture: &Av1Picture,
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

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use rust_video_caps::frame::GpuExport;

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

    /// Per-temporal-unit packets of `bytes` in order.
    fn unit_packets(bytes: &[u8]) -> Vec<Vec<u8>> {
        use crate::av1::{split_obus, temporal_units};

        let base = bytes.as_ptr() as usize;
        let mut packets = Vec::new();
        let mut start = 0;
        for unit in temporal_units(split_obus(bytes).expect("vector splits")) {
            let mut end = start;
            for obu in unit.obus() {
                let payload = obu.payload();
                end = payload.as_ptr() as usize - base + payload.len();
            }
            assert!(end > start, "unit holds bytes");
            packets.push(bytes[start..end].to_vec());
            start = end;
        }
        assert_eq!(start, bytes.len(), "units partition the buffer");
        packets
    }

    /// Reference decode units plus show-existing displays of `bytes`.
    fn reference_map(
        bytes: &[u8],
    ) -> (
        std::collections::HashMap<crate::dpb::ReferenceId, usize>,
        Vec<(usize, crate::dpb::ReferenceId)>,
    ) {
        use crate::av1::dpb::{Av1Context, Av1Instruction};
        use crate::av1::{split_obus, temporal_units};

        let mut context = Av1Context::new();
        let mut decoded = std::collections::HashMap::new();
        let mut shows = Vec::new();
        for (index, unit) in temporal_units(split_obus(bytes).expect("vector splits"))
            .iter()
            .enumerate()
        {
            for instruction in context.compile(unit).expect("vector compiles") {
                match instruction {
                    Av1Instruction::Picture(picture) => {
                        if let Some(id) = picture.picture().id() {
                            decoded.insert(id, index);
                        }
                    }
                    Av1Instruction::ShowExisting { id, .. } => shows.push((index, id)),
                    Av1Instruction::Sequence(_) => {}
                }
            }
        }
        (decoded, shows)
    }

    /// Trait frames carry bytes behind an opaque backend descriptor.
    fn check_trait_frame(frame: &crate::decoder::VulkanFrame) {
        assert!(
            matches!(frame.export(), GpuExport::Opaque { .. }),
            "trait frame carries a handle"
        );
        assert!(!frame.nv12().is_empty(), "trait frame holds bytes");
        assert!(frame.planes().is_none(), "trait frame exposes no planes");
    }

    /// Surface frames pin export planes behind empty bytes.
    fn check_surface_frame(frame: &crate::decoder::VulkanFrame) {
        assert!(frame.planes().is_some(), "surface frame pins planes");
        assert!(frame.nv12().is_empty(), "surface frame skips the readback");
        assert!(
            frame.device_export().is_some(),
            "surface frame carries its export"
        );
    }

    /// Flushed frames hold exactly one form: bytes or export.
    fn check_either_frame(frame: &crate::decoder::VulkanFrame) {
        let gpu = frame.planes().is_some();
        assert_eq!(gpu, frame.nv12().is_empty(), "frame mixes bytes and export");
        assert_eq!(
            gpu,
            frame.device_export().is_some(),
            "planes agree with export"
        );
        if !gpu {
            assert!(
                matches!(frame.export(), GpuExport::Opaque { .. }),
                "flushed trait frame carries a handle"
            );
        }
    }

    /// Export variant tag behind consistency checks.
    fn export_tag(frame: &crate::decoder::VulkanFrame) -> &'static str {
        match frame.export() {
            GpuExport::Opaque { .. } => "opaque",
            #[cfg(unix)]
            GpuExport::DmaBuf { .. } => "dmabuf",
            #[cfg(windows)]
            GpuExport::NtHandle { .. } => "nthandle",
            _ => "other",
        }
    }

    /// Frame count of `pattern` (`true` decodes the surface path) on a fresh decoder.
    ///
    /// `None` declines on VRAM exhaustion; every other error panics.
    fn run_pattern(
        device: &crate::device::VulkanDevice,
        packets: &[Vec<u8>],
        pattern: &[bool],
        surface_flush: bool,
    ) -> Option<usize> {
        use bytes::Bytes;
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;

        let mut decoder = match super::Av1Decoder::open(device) {
            Ok(decoder) => decoder,
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM av1 open: {error}");
                return None;
            }
            Err(error) => panic!("engine starts: {error}"),
        };
        let mut count = 0;
        let mut variants = std::collections::HashSet::new();
        let mut saw_surface = false;
        let mut saw_trait = false;
        for (gpu, raw) in pattern.iter().zip(packets.iter()) {
            let packet = Packet::new(Bytes::copy_from_slice(raw));
            let frames = if *gpu {
                match decoder.decode_surface(&packet) {
                    Ok(frames) => frames,
                    Err(error) if error.is_out_of_device_memory() => {
                        println!("declined:OOM av1 surface decode: {error}");
                        return None;
                    }
                    Err(error) => panic!("surface decode runs: {error}"),
                }
            } else {
                match decoder.decode(&packet) {
                    Ok(frames) => frames,
                    Err(error) if error.is_out_of_device_memory() => {
                        println!("declined:OOM av1 decode: {error}");
                        return None;
                    }
                    Err(error) => panic!("decode runs: {error}"),
                }
            };
            for frame in &frames {
                check_either_frame(frame);
                if frame.planes().is_some() {
                    saw_surface = true;
                    variants.insert(export_tag(frame));
                } else {
                    saw_trait = true;
                }
            }
            count += frames.len();
        }
        let flushed = if surface_flush {
            match decoder.flush_surface() {
                Ok(frames) => frames,
                Err(error) if error.is_out_of_device_memory() => {
                    println!("declined:OOM av1 surface flush: {error}");
                    return None;
                }
                Err(error) => panic!("surface flush runs: {error}"),
            }
        } else {
            match decoder.flush() {
                Ok(frames) => frames,
                Err(error) if error.is_out_of_device_memory() => {
                    println!("declined:OOM av1 flush: {error}");
                    return None;
                }
                Err(error) => panic!("flush runs: {error}"),
            }
        };
        for frame in &flushed {
            check_either_frame(frame);
            if frame.planes().is_some() {
                saw_surface = true;
                variants.insert(export_tag(frame));
            } else {
                saw_trait = true;
            }
        }
        count += flushed.len();
        assert!(variants.len() <= 1, "surface exports stay consistent");
        assert_eq!(
            saw_surface,
            pattern.iter().any(|gpu| *gpu),
            "surface arrivals follow surface submits"
        );
        assert_eq!(
            saw_trait,
            pattern.iter().any(|gpu| !*gpu),
            "trait arrivals follow trait submits"
        );
        Some(count)
    }

    /// Cross-path replay of the first show-existing fails clean.
    ///
    /// The show-prefix decodes on one path, the show-existing unit on
    /// the other; the retained form cannot cross, so the call errors
    /// instead of leaking a handle. `None` declines on VRAM exhaustion.
    fn run_cross_leg(
        device: &crate::device::VulkanDevice,
        packets: &[Vec<u8>],
        decode_units: &std::collections::HashMap<crate::dpb::ReferenceId, usize>,
        shows: &[(usize, crate::dpb::ReferenceId)],
        gpu_first: bool,
    ) -> Option<()> {
        use crate::error::VulkanError;

        use bytes::Bytes;
        use rust_video_caps::packet::Packet;
        use rust_video_caps::traits::Decoder;

        let (show, id) = shows[0];
        let found = decode_units[&id];
        assert!(show > found, "show-existing replays a past decode");
        let mut decoder = match super::Av1Decoder::open(device) {
            Ok(decoder) => decoder,
            Err(error) if error.is_out_of_device_memory() => {
                println!("declined:OOM av1 open: {error}");
                return None;
            }
            Err(error) => panic!("engine starts: {error}"),
        };
        for raw in packets.iter().take(show) {
            let packet = Packet::new(Bytes::copy_from_slice(raw));
            let frames = if gpu_first {
                match decoder.decode_surface(&packet) {
                    Ok(frames) => frames,
                    Err(error) if error.is_out_of_device_memory() => {
                        println!("declined:OOM av1 surface decode: {error}");
                        return None;
                    }
                    Err(error) => panic!("surface decode runs: {error}"),
                }
            } else {
                match decoder.decode(&packet) {
                    Ok(frames) => frames,
                    Err(error) if error.is_out_of_device_memory() => {
                        println!("declined:OOM av1 decode: {error}");
                        return None;
                    }
                    Err(error) => panic!("decode runs: {error}"),
                }
            };
            for frame in &frames {
                if gpu_first {
                    check_surface_frame(frame);
                } else {
                    check_trait_frame(frame);
                }
            }
        }
        let packet = Packet::new(Bytes::copy_from_slice(&packets[show]));
        let crossed = if gpu_first {
            decoder.decode(&packet)
        } else {
            decoder.decode_surface(&packet)
        };
        match crossed {
            Err(VulkanError::Unsupported { what }) if what.contains("show-existing") => Some(()),
            Err(error) => panic!("cross-path replay fails clean: {error}"),
            Ok(_) => panic!("cross-path replay must not leak across paths"),
        }
    }

    /// Mixed surface+trait AV1 keeps trait frames opaque, surfaces pinned.
    ///
    /// Same-decoder interleavings over a show-existing vector pin both
    /// path contracts plus the clean cross-path replay refusal: no
    /// retained export ever lands on the other path.
    #[test]
    fn av1_mixed_surface_trait_keeps_exports_consistent() {
        use crate::device::VulkanDevice;
        use crate::error::VulkanError;
        use crate::ladder::CodecOp;

        use rust_video_harness::decode_dir;

        let device = match VulkanDevice::open_or_skip(CodecOp::Av1Decode) {
            Ok(device) => device,
            Err(VulkanError::BackendAbsent(reason)) => {
                println!("declined: av1 on this box: {reason}");
                return;
            }
            Err(error) => panic!("negotiation failed: {error}"),
        };
        if device.share_mechanism().is_none() {
            println!("declined: mixed-use pin needs platform export");
            return;
        }
        let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector reads");
        let packets = unit_packets(&bytes);
        assert!(packets.len() > 1, "vector mixes across packets");
        let (decoded, shows) = reference_map(&bytes);
        assert!(!shows.is_empty(), "vector pins the retain path");
        for (unit, id) in &shows {
            assert!(
                decoded.get(id).is_some_and(|found| found != unit),
                "show-existing names a decoded reference"
            );
        }
        let units = packets.len();
        let all_trait = vec![false; units];
        let all_surface = vec![true; units];
        let mut surface_first: Vec<bool> = (0..units).map(|index| index % 2 == 0).collect();
        let mut trait_first: Vec<bool> = (0..units).map(|index| index % 2 == 1).collect();
        for pattern in [&mut surface_first, &mut trait_first] {
            for _ in 0..units {
                let mut changed = false;
                for (show, id) in &shows {
                    let found = decoded[id];
                    if pattern[*show] != pattern[found] {
                        pattern[*show] = pattern[found];
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            assert!(pattern.iter().any(|gpu| *gpu), "pattern uses surfaces");
            assert!(pattern.iter().any(|gpu| !gpu), "pattern uses the trait");
        }
        assert_ne!(surface_first, trait_first, "interleavings differ");
        let Some(expected) = run_pattern(&device, &packets, &all_trait, false) else {
            return;
        };
        assert_eq!(expected, 15, "reference run yields every frame");
        let Some(surface_count) = run_pattern(&device, &packets, &all_surface, true) else {
            return;
        };
        assert_eq!(surface_count, expected, "surface run yields every frame");
        let Some(first) = run_pattern(&device, &packets, &surface_first, true) else {
            return;
        };
        assert_eq!(first, expected, "mixed run yields every frame");
        let Some(second) = run_pattern(&device, &packets, &trait_first, false) else {
            return;
        };
        assert_eq!(second, expected, "mixed run yields every frame");
        for gpu_first in [true, false] {
            if run_cross_leg(&device, &packets, &decoded, &shows, gpu_first).is_none() {
                return;
            }
        }
    }
}
