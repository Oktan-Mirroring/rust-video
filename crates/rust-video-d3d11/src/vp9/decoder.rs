//! VP9 decoder over a D3D11 device.

use std::time::Duration;
use std::time::Instant;

use crate::decoder::{emit_surface, Active};
use crate::device::D3d11Device;
use crate::dpb::Output;
use crate::error::{D3d11Error, UnsupportedReason};
use crate::frame::D3d11Frame;
use crate::guid::Codec;
use crate::overlap::{FrameQueue, PumpGauges, SplitParts};
use crate::readback::Readback;
use crate::session::Session;
use crate::texture::GpuSurface;

use rust_video_caps::delivery::{PhaseTiming, SplitDecode, SubmitStats};
use rust_video_caps::packet::Packet;
use rust_video_caps::reason::UnsupportedReason as StreamCore;
use rust_video_caps::traits::Decoder;
use rust_video_caps::traits::SharedDevice;
use rust_video_caps::transcode::{FallbackCause, GpuCaps};

/// Submitted surfaces held back across surface calls.
const SURFACE_HOLD: usize = 1;

/// VP9 decoder over a D3D11 device.
pub struct Vp9Decoder<'a> {
    /// Device owning the decode session.
    device: &'a D3d11Device,
    /// Frame ingest: uncompressed headers plus tile assembly.
    ingest: crate::vp9::Ingest,
    /// Live reference slots, created from the first frame.
    dpb: Option<super::dpb::Vp9Dpb>,
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
    /// Phase-split timing per collected frame, oldest first.
    timings: Vec<PhaseTiming>,
    /// Input ended; submits decline, receives drain then end.
    finished: bool,
    /// Copy worker behind launches.
    readback: Readback,
    /// Submitted surfaces held back one packet for submit/wait overlap.
    held_surfaces: Vec<GpuSurface>,
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
            timings: Vec::new(),
            finished: false,
            readback: Readback::spawn(),
            held_surfaces: Vec::new(),
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

    /// Submit gauges since decoder open.
    #[must_use]
    pub fn submit_stats(&self) -> SubmitStats {
        let mut stats = SubmitStats::new();
        stats.add_resets(self.resets());
        stats.fold_peak(self.max_inflight());
        stats
    }

    /// Phase-split timing per collected frame, oldest first.
    #[must_use]
    pub fn timings(&self) -> &[PhaseTiming] {
        &self.timings
    }

    /// Queue mechanics over the live session and queue.
    fn parts(&mut self) -> SplitParts<'_> {
        SplitParts::new(
            self.session.as_ref(),
            self.shared.as_ref(),
            &mut self.queue,
            &mut self.generation,
            &mut self.timings,
            &self.readback,
        )
    }

    /// Frames of `packet` submitted, without waiting on completion.
    ///
    /// Returns the outputs queued behind `packet`.
    fn submit_packet_inner(&mut self, packet: &Packet) -> Result<usize, D3d11Error> {
        let frames_in = match self.ingest.ingest_packet(packet.data()) {
            Ok(frames) => frames,
            Err(error) => {
                self.reset_stream();
                self.queue.clear_pending();
                return Err(error);
            }
        };
        let mut queued = 0;
        for frame in &frames_in {
            queued += self.submit_frame_split(frame)?;
        }
        Ok(queued)
    }

    /// Decode `frame`, queueing its outputs for completion reads.
    ///
    /// Returns the outputs queued behind `frame`.
    fn submit_frame_split(&mut self, frame: &crate::vp9::Frame) -> Result<usize, D3d11Error> {
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
        Ok(decoded.outputs.len())
    }

    /// `decoded` submitted plus its outputs copied, in VP9 order.
    fn submit_ordered(&self, decoded: &super::dpb::Vp9Decoded) -> Result<(), D3d11Error> {
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
        self.held_surfaces.clear();
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
        self.dpb = Some(super::dpb::Vp9Dpb::new());
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
    ) -> Result<super::dpb::Vp9Decoded, D3d11Error> {
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
        let slots: Vec<u8> = super::dpb::Vp9Dpb::drain()
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
                } else if let Err(error) = session.copy_slot_light(submission.slot) {
                    failed = Some(error);
                }
            }
            if failed.is_none() {
                for output in &decoded.outputs {
                    let Some(surface) = emit_surface(session, output, &active, &pins) else {
                        failed = Some(D3d11Error::Unsupported(UnsupportedReason::Stream(
                            StreamCore::Fallback {
                                cause: FallbackCause::NoSharedExport,
                            },
                        )));
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
        for output in super::dpb::Vp9Dpb::drain() {
            let Some(surface) = emit_surface(session, &output, active, &pins) else {
                return Err(D3d11Error::Unsupported(UnsupportedReason::Stream(
                    StreamCore::Fallback {
                        cause: FallbackCause::NoSharedExport,
                    },
                )));
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
        let mut fresh = Vec::new();
        for frame in &frames_in {
            self.surface_frame(frame, &mut fresh)?;
        }
        // One packet held back: the previous call's surfaces return
        // complete while the new submit's engine work runs. Past the
        // hold the oldest waits, retiring driver queue under the
        // ~30-submit overrun cliff.
        self.held_surfaces.append(&mut fresh);
        let mut surfaces = Vec::new();
        while self.held_surfaces.len() > SURFACE_HOLD {
            let oldest = self.held_surfaces.remove(0);
            oldest.try_wait()?;
            surfaces.push(oldest);
        }
        Ok(surfaces)
    }

    /// GPU surfaces held back for reordering or buffering.
    ///
    /// # Errors
    ///
    /// Returns [`D3d11Error`] when the trailing frame fails to convert.
    pub fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        let mut fresh = Vec::new();
        let trailing = match self.ingest.finish() {
            Ok(trailing) => trailing,
            Err(error) => {
                self.reset_stream();
                return Err(error);
            }
        };
        if let Some(frame) = trailing {
            self.surface_frame(&frame, &mut fresh)?;
        }
        if let Some(active) = self.active {
            self.drain_live_surface(&active, &mut fresh)?;
        }
        self.held_surfaces.append(&mut fresh);
        Ok(std::mem::take(&mut self.held_surfaces))
    }
}

impl Decoder for Vp9Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn caps(&self) -> GpuCaps {
        let share = self.device.share_mechanism().filter(|_| {
            self.session
                .as_ref()
                .is_none_or(|session| session.export().is_some())
        });
        GpuCaps::new(share, share.is_some(), false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        // One packet held back: the previous call drains while the new
        // submit's engine work runs. Siblings stay per-call pinned by
        // surface_contract; VP9 carries no such pin.
        let mut frames = Vec::new();
        let submitted = match self.submit_packet_inner(packet) {
            Ok(queued) => queued,
            Err(error) => {
                self.queue.clear();
                return Err(error);
            }
        };
        while self.queue.pending() > submitted {
            if let Err(error) = self.parts().drain_oldest() {
                self.queue.clear();
                self.reset_stream();
                return Err(error);
            }
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

impl SplitDecode for Vp9Decoder<'_> {
    type Frame = D3d11Frame;
    type Error = D3d11Error;

    fn try_submit(&mut self, packet: &Packet) -> Result<bool, D3d11Error> {
        if self.finished {
            return Ok(false);
        }
        self.parts().sweep_ready()?;
        if !self.queue.admits() {
            return Ok(false);
        }
        self.submit_packet_inner(packet)?;
        Ok(true)
    }

    fn poll(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        self.parts().sweep_ready()?;
        let mut frames = Vec::new();
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn poll_wait(&mut self, timeout: Duration) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let deadline = Instant::now() + timeout;
        loop {
            self.parts().sweep_ready()?;
            let mut frames = Vec::new();
            self.parts().take_ready(&mut frames);
            if !frames.is_empty() {
                return Ok(frames);
            }
            if self.finished && self.queue.is_drained() {
                return Ok(Vec::new());
            }
            if self.queue.front().is_some() {
                self.parts().drain_oldest()?;
                continue;
            }
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn flush_split(&mut self) -> Result<Vec<D3d11Frame>, D3d11Error> {
        if !self.finished {
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
        }
        let mut frames = Vec::new();
        self.parts().take_ready(&mut frames);
        Ok(frames)
    }

    fn stats(&self) -> SubmitStats {
        self.submit_stats()
    }
}

impl PumpGauges for Vp9Decoder<'_> {
    fn depth(&self) -> usize {
        self.queue.pending()
    }

    fn max_depth(&self) -> usize {
        self.queue.max_observed()
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn pump_timings(&self) -> &[PhaseTiming] {
        &self.timings
    }
}
