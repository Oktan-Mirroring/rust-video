//! Overlapped decode over one Vulkan decoder.

use std::time::{Duration, Instant};

use rust_video_caps::delivery::{
    ChannelConfig, FrameChannel, PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets,
    TrySendError,
};
use rust_video_caps::packet::Packet;

use crate::decoder::{OverlappedDecode, VulkanFrame};
use crate::error::VulkanError;

/// Overlapped decode over one Vulkan decoder.
///
/// `try_send` submits without waiting on completion and returns
/// `Full` past [`MAX_INFLIGHT`](Self::MAX_INFLIGHT) submitted pictures;
/// consumers poll, block, or drain through a callback. The readback of
/// frame N overlaps the engine decode of frame N+1: submits queue while
/// earlier frames still run. Session rebuilds and show-existing
/// displays drain blocking for correctness.
pub struct OverlapPump<D: OverlappedDecode> {
    /// Split-phase decode behind sends.
    decoder: D,
    /// Bounded output with drop policy.
    channel: FrameChannel<VulkanFrame>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
}

impl<D: OverlappedDecode> OverlapPump<D> {
    /// Submitted-but-uncollected picture bound, the submit ring depth.
    pub const MAX_INFLIGHT: usize = crate::session::slots::SUBMIT_SLOTS;

    /// Pump decoding through `decoder` into `config` frames of queue.
    ///
    /// # Arguments
    ///
    /// * `decoder` - split-phase Vulkan decoder behind sends.
    /// * `config` - bound plus drop policy.
    #[must_use]
    pub fn open(decoder: D, config: ChannelConfig) -> Self {
        Self {
            decoder,
            channel: FrameChannel::new(config),
            finished: false,
        }
    }

    /// Split-phase decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        &self.decoder
    }

    /// Bound plus drop policy.
    #[must_use]
    pub fn config(&self) -> ChannelConfig {
        self.channel.config()
    }

    /// Queued frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.channel.len()
    }

    /// True when no frame is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channel.is_empty()
    }

    /// True once input ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Submitted but uncollected picture count.
    #[must_use]
    pub fn inflight_len(&self) -> usize {
        self.decoder.inflight_len()
    }

    /// Frames dropped under the policy, plus post-close sends.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.channel.dropped()
    }

    /// Logged phase timings, oldest first, clearing the log.
    pub fn take_timings(&mut self) -> Vec<PhaseTiming> {
        self.decoder.take_timings()
    }

    /// Consumer handle over the queue; later sends arrive here.
    #[must_use]
    pub fn channel(&self) -> FrameChannel<VulkanFrame> {
        self.channel.clone()
    }

    /// Queue `frames` through the bound plus policy.
    fn enqueue(&self, frames: Vec<VulkanFrame>) {
        for frame in frames {
            let _dropped = self.channel.send(frame);
        }
    }
}

impl<D: OverlappedDecode> SendPackets for OverlapPump<D> {
    type Error = VulkanError;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        if self.decoder.inflight_len() >= Self::MAX_INFLIGHT {
            return Err(TrySendError::Full { packet });
        }
        let frames = self
            .decoder
            .submit_packet(&packet)
            .map_err(|error| TrySendError::Failed { error })?;
        self.enqueue(frames);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let frames = self.decoder.finish_overlapped()?;
        self.enqueue(frames);
        self.channel.close();
        Ok(())
    }
}

impl<D: OverlappedDecode> PollFrames for OverlapPump<D> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        let frames = self.decoder.poll_ready()?;
        self.enqueue(frames);
        Ok(self.channel.try_receive())
    }
}

impl<D: OverlappedDecode> ReceiveFrames for OverlapPump<D> {
    type Frame = VulkanFrame;
    type Error = VulkanError;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        loop {
            let frames = self.decoder.poll_ready()?;
            self.enqueue(frames);
            if let Some(frame) = self.channel.try_receive() {
                return Ok(Some(frame));
            }
            if self.decoder.inflight_len() == 0 {
                if self.finished {
                    return Ok(None);
                }
                return Ok(self.channel.receive());
            }
            let frames = self.decoder.collect_next()?;
            self.enqueue(frames);
        }
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        let start = Instant::now();
        loop {
            let frames = self
                .decoder
                .poll_ready()
                .map_err(|error| ReceiveError::Failed { error })?;
            self.enqueue(frames);
            if let Some(frame) = self.channel.try_receive() {
                return Ok(Some(frame));
            }
            if self.decoder.inflight_len() == 0 {
                if self.finished {
                    return Ok(None);
                }
                return Err(ReceiveError::Timeout);
            }
            let elapsed = start.elapsed();
            let Some(remaining) = timeout.checked_sub(elapsed) else {
                return Err(ReceiveError::Timeout);
            };
            match self
                .decoder
                .collect_next_timeout(remaining)
                .map_err(|error| ReceiveError::Failed { error })?
            {
                Some(frames) => self.enqueue(frames),
                None => return Err(ReceiveError::Timeout),
            }
        }
    }
}
