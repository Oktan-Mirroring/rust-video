//! H.264 decoder over a D3D11 device.

use std::time::Duration;
use std::time::Instant;

use super::dpb::{Decoded, Dpb};
use crate::decoder::{emit_surface, Active};
use crate::device::D3d11Device;
use crate::dpb::Output;
use crate::error::{D3d11Error, UnsupportedReason};
use crate::frame::D3d11Frame;
use crate::h264::{Ingest, Picture};
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
    /// Phase-split timing per collected frame, oldest first.
    timings: Vec<PhaseTiming>,
    /// Input ended; submits decline, receives drain then end.
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
            timings: Vec::new(),
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
                self.parts().abandon_pending();
                return Err(error);
            }
        };
        let submit_start = Instant::now();
        // Submit before any copy: outputs may name the submission slot.
        if let Err(error) = self
            .session
            .as_ref()
            .ok_or(D3d11Error::NoSession)
            .and_then(|session| session.submit(&decoded.submission))
        {
            self.reset_stream();
            self.parts().abandon_pending();
            return Err(error);
        }
        let submit_end = Instant::now();
        let mut copied = Vec::with_capacity(decoded.outputs.len());
        for output in &decoded.outputs {
            match self.parts().copy_output(output.slot()) {
                Ok(lane) => copied.push(lane),
                Err(error) => {
                    self.reset_stream();
                    self.parts().abandon_pending();
                    return Err(error);
                }
            }
        }
        for lane in copied {
            self.queue.push(lane, active, submit_start, submit_end);
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
            } else if let Err(error) = session.copy_slot_light(decoded.submission.slot) {
                failed = Some(error);
            } else {
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
        let (Some(dpb), Some(session)) = (self.dpb.as_mut(), self.session.as_ref()) else {
            return Ok(());
        };
        let pins = dpb.pins().clone();
        for output in dpb.drain() {
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

    fn caps(&self) -> GpuCaps {
        let share = self.device.share_mechanism().filter(|_| {
            self.session
                .as_ref()
                .is_none_or(|session| session.export().is_some())
        });
        GpuCaps::new(share, share.is_some(), false)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<D3d11Frame>, D3d11Error> {
        let mut frames = Vec::new();
        if let Err(error) = self.submit_packet_inner(packet) {
            self.parts().abandon_queue();
            return Err(error);
        }
        if let Err(error) = self.parts().drain_all() {
            self.parts().abandon_queue();
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
                self.parts().abandon_queue();
                return Err(error);
            }
        }
        if let Err(error) = self.parts().drain_all() {
            self.parts().abandon_queue();
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

impl SplitDecode for H264Decoder<'_> {
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
            if let Some(picture) = self.ingest.finish()? {
                if let Err(error) = self.submit_picture_split(&picture) {
                    self.parts().abandon_pending();
                    return Err(error);
                }
            }
            if let Err(error) = self.parts().drain_all() {
                self.parts().abandon_pending();
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

impl PumpGauges for H264Decoder<'_> {
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

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_surface_keeps_pixels_under_pressure() {
        use rust_video_caps::traits::Encoder;

        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let config =
            crate::encoder::D3d11Encoder::auto_config(crate::guid::Codec::H264).expect("h264 auto");
        let Ok(mut encoder) = crate::encoder::D3d11Encoder::open(&device, config) else {
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
        let crop = decoder.active.expect("live sequence").crop();
        let session = decoder.session.as_ref().expect("live session");
        let lane = session.take_lane().expect("free lane");
        session.copy_lane(slot, lane).expect("copy runs");
        let before = session.read_lane(lane, crop).expect("read runs");
        session.give_lane(lane);
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
        let session = decoder.session.as_ref().expect("live session");
        let lane = session.take_lane().expect("free lane");
        session.copy_lane(slot, lane).expect("copy runs");
        let after = session.read_lane(lane, crop).expect("read runs");
        session.give_lane(lane);
        assert_eq!(before.as_ref(), after.as_ref());
    }

    #[test]
    fn caps_agree_with_frame_exports() {
        use rust_video_caps::frame::GpuExport;
        use rust_video_caps::traits::{Decoder, Encoder};

        let Ok(device) = crate::device::D3d11Device::open() else {
            return;
        };
        let config =
            crate::encoder::D3d11Encoder::auto_config(crate::guid::Codec::H264).expect("h264 auto");
        let Ok(mut encoder) = crate::encoder::D3d11Encoder::open(&device, config) else {
            return;
        };
        let mut decoder = super::H264Decoder::open(&device);
        let idle = decoder.caps();
        assert_eq!(idle.share().is_some(), idle.decode_export());
        assert!(!idle.encode_import());
        let mut frames = Vec::new();
        for seed in 0..4u64 {
            let frame = rust_video_harness::Synth::noise(320, 240, seed);
            for packet in encoder.encode(&frame).expect("encode runs") {
                frames.extend(decoder.decode(&packet).expect("decode runs"));
            }
        }
        for packet in encoder.flush().expect("encode drains") {
            frames.extend(decoder.decode(&packet).expect("decode runs"));
        }
        frames.extend(decoder.flush().expect("decode drains"));
        assert!(!frames.is_empty());
        let live = decoder.caps();
        assert_eq!(live.share().is_some(), live.decode_export());
        assert!(!live.encode_import());
        for frame in &frames {
            match live.share() {
                Some(_) => assert!(matches!(frame.export(), GpuExport::NtHandle { .. })),
                None => assert!(matches!(frame.export(), GpuExport::Opaque { .. })),
            }
        }
    }
}
