//! Overlapped decode over one Vulkan decoder.

use std::time::{Duration, Instant};

use rust_video_caps::delivery::{
    ChannelConfig, FrameChannel, PollFrames, ReceiveError, ReceiveFrames, SendPackets, SplitDecode,
    TrySendError,
};
use rust_video_caps::packet::Packet;

/// Overlapped decode over one split-phase decoder.
///
/// `try_send` submits without waiting on completion and returns
/// `Full` past the submit ring depth; consumers poll, block, or
/// drain through a callback. The readback of frame N overlaps the
/// engine decode of frame N+1: submits queue while earlier frames
/// still run. Session rebuilds and show-existing displays drain
/// blocking for correctness.
pub struct OverlapPump<D: SplitDecode> {
    /// Split-phase decode behind sends.
    decoder: D,
    /// Bounded output with drop policy.
    channel: FrameChannel<D::Frame>,
    /// Input ended; sends refuse, receives drain then end.
    finished: bool,
}

impl<D: SplitDecode> OverlapPump<D> {
    /// Submitted-but-uncollected picture bound, the submit ring depth.
    pub const MAX_INFLIGHT: usize = crate::session::slots::SUBMIT_SLOTS;

    /// Bounded wait behind blocking receive.
    const RECEIVE_QUANTUM: Duration = Duration::from_secs(1);

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

    /// Split-phase decode behind sends, mutably.
    pub fn decoder_mut(&mut self) -> &mut D {
        &mut self.decoder
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

    /// Frames dropped under the policy, plus post-close sends.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.channel.dropped()
    }

    /// Consumer handle over the queue; later sends arrive here.
    #[must_use]
    pub fn channel(&self) -> FrameChannel<D::Frame> {
        self.channel.clone()
    }

    /// Queue `frames` through the bound plus policy.
    fn enqueue(&self, frames: Vec<D::Frame>) {
        for frame in frames {
            let _dropped = self.channel.send(frame);
        }
    }
}

impl<D: SplitDecode> SendPackets for OverlapPump<D> {
    type Error = D::Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        if self.finished {
            return Err(TrySendError::Full { packet });
        }
        let accepted = self
            .decoder
            .try_submit(&packet)
            .map_err(|error| TrySendError::Failed { error })?;
        if !accepted {
            return Err(TrySendError::Full { packet });
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let frames = self.decoder.flush_split()?;
        self.enqueue(frames);
        self.channel.close();
        Ok(())
    }
}

impl<D: SplitDecode> PollFrames for OverlapPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        let frames = self.decoder.poll()?;
        self.enqueue(frames);
        Ok(self.channel.try_receive())
    }
}

impl<D: SplitDecode> ReceiveFrames for OverlapPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        loop {
            let frames = self.decoder.poll()?;
            self.enqueue(frames);
            if let Some(frame) = self.channel.try_receive() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            let frames = self.decoder.poll_wait(Self::RECEIVE_QUANTUM)?;
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
                .poll()
                .map_err(|error| ReceiveError::Failed { error })?;
            self.enqueue(frames);
            if let Some(frame) = self.channel.try_receive() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            let elapsed = start.elapsed();
            let Some(remaining) = timeout.checked_sub(elapsed) else {
                return Err(ReceiveError::Timeout);
            };
            if remaining.is_zero() {
                return Err(ReceiveError::Timeout);
            }
            let frames = self
                .decoder
                .poll_wait(remaining)
                .map_err(|error| ReceiveError::Failed { error })?;
            self.enqueue(frames);
        }
    }
}
