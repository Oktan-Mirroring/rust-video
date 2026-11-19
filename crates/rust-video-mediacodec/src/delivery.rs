//! Split-phase decode over any `MediaCodec` decoder.
//!
//! Caps pump with bound plus policy at open.

use std::time::Duration;

use rust_video_caps::delivery::{
    ChannelConfig, FrameChannel, PollFrames, Pump, ReceiveError, ReceiveFrames, SendPackets,
    TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;

/// Split-phase decode over one `MediaCodec` decoder.
///
/// `try_send` decodes now and queues output through the bound plus policy;
/// consumers poll, block, or drain through a callback. Wraps any decoder
/// behind [`Decoder`].
pub struct DecoderPump<D: Decoder> {
    /// Sync decode plus bounded output.
    pump: Pump<D>,
}

impl<D: Decoder> DecoderPump<D> {
    /// Pump decoding through `decoder` into `config` frames of queue.
    ///
    /// # Arguments
    ///
    /// * `decoder` - open `MediaCodec` decoder behind sends.
    /// * `config` - bound plus drop policy.
    #[must_use]
    pub fn open(decoder: D, config: ChannelConfig) -> Self {
        Self {
            pump: Pump::new(decoder, config),
        }
    }

    /// Decode behind sends.
    #[must_use]
    pub fn decoder(&self) -> &D {
        self.pump.decoder()
    }

    /// Bound plus drop policy.
    #[must_use]
    pub fn config(&self) -> ChannelConfig {
        self.pump.config()
    }

    /// Queued frame count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pump.len()
    }

    /// True when no frame is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pump.is_empty()
    }

    /// True once input ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.pump.is_finished()
    }

    /// Frames dropped under the policy, plus post-close sends.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.pump.dropped()
    }

    /// Consumer handle over the queue; later sends arrive here.
    #[must_use]
    pub fn channel(&self) -> FrameChannel<D::Frame> {
        self.pump.channel()
    }
}

impl<D: Decoder> SendPackets for DecoderPump<D> {
    type Error = D::Error;

    fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
        self.pump.try_send(packet)
    }

    fn finish(&mut self) -> Result<(), Self::Error> {
        self.pump.finish()
    }
}

impl<D: Decoder> PollFrames for DecoderPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.pump.try_receive()
    }
}

impl<D: Decoder> ReceiveFrames for DecoderPump<D> {
    type Frame = D::Frame;
    type Error = D::Error;

    fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
        self.pump.receive()
    }

    fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
        self.pump.receive_timeout(timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::num::NonZeroUsize;

    use super::DecoderPump;

    use bytes::Bytes;
    use rust_video_caps::delivery::{
        ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets,
    };
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;
    use rust_video_caps::transcode::GpuCaps;

    struct HoldbackDecoder {
        next: u64,
        held: Option<u64>,
    }

    impl Decoder for HoldbackDecoder {
        type Frame = u64;
        type Error = Reject;

        fn caps(&self) -> GpuCaps {
            GpuCaps::new(None, false, false)
        }

        fn decode(&mut self, _packet: &Packet) -> Result<Vec<u64>, Reject> {
            let id = self.next;
            self.next += 1;
            Ok(self.held.replace(id).into_iter().collect())
        }

        fn flush(&mut self) -> Result<Vec<u64>, Reject> {
            Ok(self.held.take().into_iter().collect())
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Reject;

    impl fmt::Display for Reject {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "rejected")
        }
    }

    impl std::error::Error for Reject {}

    fn config() -> ChannelConfig {
        ChannelConfig::new(
            NonZeroUsize::new(8).expect("nonzero capacity"),
            DropPolicy::DropOldest,
        )
    }

    #[test]
    fn sends_receive_own_frames_then_drain() {
        let decoder = HoldbackDecoder {
            next: 0,
            held: None,
        };
        let mut pump = DecoderPump::open(decoder, config());
        for byte in 0..4u8 {
            let packet = Packet::new(Bytes::from(vec![byte]));
            pump.try_send(packet).expect("queue room");
        }
        let mut ids = Vec::new();
        while let Some(id) = pump.try_receive().expect("poll") {
            ids.push(id);
        }
        assert_eq!(ids, [0, 1, 2]);
        assert_eq!(pump.dropped(), 0);
        pump.finish().expect("finish");
        assert!(pump.is_finished());
        assert_eq!(pump.receive().expect("trailing"), Some(3));
        assert_eq!(pump.receive().expect("drained"), None);
    }
}
