//! Shared overlap, parity, and sustained drivers over caps traits.
//!
//! Every tier suite feeds the same coded packets through the same
//! split-phase loops; only vectors, bars, and device open differ.

use std::num::NonZeroUsize;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;

/// Process-wide suite serialization.
static SERIAL: Mutex<()> = Mutex::new(());

/// Split-phase drive over caps delivery traits.
pub struct Drive;

impl Drive {
    /// Bound of `capacity` frames under `policy`.
    ///
    /// # Panics
    ///
    /// Panics when `capacity` is zero.
    #[must_use]
    pub fn channel(capacity: usize, policy: DropPolicy) -> ChannelConfig {
        ChannelConfig::new(
            NonZeroUsize::new(capacity).expect("nonzero capacity"),
            policy,
        )
    }

    /// Frames decoded straight through `decoder`, the pump oracle.
    ///
    /// # Panics
    ///
    /// Panics when `decoder` fails a packet or its flush.
    #[must_use]
    pub fn decode_all<D>(decoder: &mut D, packets: &[Packet]) -> Vec<D::Frame>
    where
        D: Decoder,
        D::Error: std::fmt::Debug,
    {
        let mut frames = Vec::new();
        for (index, packet) in packets.iter().enumerate() {
            frames.extend(
                decoder
                    .decode(packet)
                    .unwrap_or_else(|error| panic!("sync packet {index}: {error}")),
            );
        }
        frames.extend(decoder.flush().expect("sync flush drains"));
        frames
    }

    /// Input finished, every trailing frame.
    ///
    /// # Panics
    ///
    /// Panics when `pump` fails its finish or a trailing receive.
    #[must_use]
    pub fn finish_all<P, F, E>(pump: &mut P) -> Vec<F>
    where
        P: SendPackets<Error = E> + ReceiveFrames<Frame = F, Error = E>,
        E: std::error::Error,
    {
        pump.finish().expect("finish drains");
        let mut frames = Vec::new();
        while let Some(frame) = pump.receive().expect("drain") {
            frames.push(frame);
        }
        frames
    }

    /// Every packet sent; `drain` frees room on each refusal.
    ///
    /// # Panics
    ///
    /// Panics when `pump` rejects a packet or a ready poll fails.
    ///
    /// # Arguments
    ///
    /// * `pump` - split-phase pump under test.
    /// * `packets` - packets to send.
    /// * `drain` - refusal drain; frees room per refusal.
    ///
    /// # Returns
    ///
    /// Frames surfaced so far, plus the refusal count.
    ///
    /// # Usage Notes
    ///
    /// Polls ready frames after the last send; the caller finishes the
    /// pump.
    #[must_use]
    pub fn send_all<P, F, E>(
        pump: &mut P,
        packets: &[Packet],
        mut drain: impl FnMut(&mut P, &mut Vec<F>),
    ) -> (Vec<F>, u64)
    where
        P: SendPackets<Error = E> + PollFrames<Frame = F, Error = E>,
        E: std::error::Error,
    {
        let mut frames = Vec::new();
        let mut fulls = 0;
        for (index, packet) in packets.iter().enumerate() {
            let mut packet = packet.clone();
            loop {
                match pump.try_send(packet) {
                    Ok(()) => break,
                    Err(TrySendError::Full { packet: back }) => {
                        fulls += 1;
                        packet = back;
                        drain(pump, &mut frames);
                    }
                    Err(TrySendError::Failed { error }) => {
                        panic!("overlap packet {index}: {error}")
                    }
                }
            }
        }
        while let Some(frame) = pump.try_receive().expect("overlap poll") {
            frames.push(frame);
        }
        (frames, fulls)
    }

    /// Whole-suite serial guard, poison-tolerant.
    ///
    /// # Usage Notes
    ///
    /// Every test holds the guard for its whole body; parallel device
    /// use and teardown across one binary read as flake or deadlock.
    pub fn serial() -> MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Timed overlap pass over `packets`.
    ///
    /// # Panics
    ///
    /// Panics when `pump` fails a packet, a poll, or its finish.
    ///
    /// # Returns
    ///
    /// Frames per second plus the frame count.
    #[must_use]
    pub fn sustained<P, F, E>(
        pump: &mut P,
        packets: &[Packet],
        drain: impl FnMut(&mut P, &mut Vec<F>),
    ) -> (f64, usize)
    where
        P: SendPackets<Error = E>
            + PollFrames<Frame = F, Error = E>
            + ReceiveFrames<Frame = F, Error = E>,
        E: std::error::Error,
    {
        let start = Instant::now();
        let (mut frames, _) = Self::send_all(pump, packets, drain);
        frames.extend(Self::finish_all(pump));
        let wall = start.elapsed();
        let total = u32::try_from(frames.len()).expect("frame count fits");
        (f64::from(total) / wall.as_secs_f64(), frames.len())
    }
}

/// Coded packets built once outside the pump.
pub struct Packets;

impl Packets {
    /// Packets over `raws` in order.
    #[must_use]
    pub fn from_raws(raws: &[Vec<u8>]) -> Vec<Packet> {
        raws.iter()
            .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
            .collect()
    }
}

/// Byte-parity assertions across decode legs.
pub struct Parity;

impl Parity {
    /// Equal counts with byte-exact frames in order.
    ///
    /// # Panics
    ///
    /// Panics when the counts differ or any frame pair differs.
    pub fn assert_bytes<F>(tag: &str, left: &[F], right: &[F], bytes: impl Fn(&F) -> &[u8]) {
        assert_eq!(left.len(), right.len(), "{tag}: frame count");
        for (index, (l, r)) in left.iter().zip(right.iter()).enumerate() {
            assert_eq!(bytes(l), bytes(r), "{tag}: frame {index} bytes");
        }
    }

    /// Same geometry, format, and plane bytes in order.
    ///
    /// # Panics
    ///
    /// Panics when the counts differ or any frame pair differs.
    pub fn assert_cpu_frames(codec: &str, frames: &[CpuFrame], expected: &[CpuFrame]) {
        assert_eq!(frames.len(), expected.len(), "{codec} frame count");
        for (index, (frame, want)) in frames.iter().zip(expected.iter()).enumerate() {
            assert_eq!(frame.size(), want.size(), "{codec} frame {index} size");
            assert_eq!(
                frame.format(),
                want.format(),
                "{codec} frame {index} format"
            );
            assert_eq!(
                frame.planes().len(),
                want.planes().len(),
                "{codec} frame {index} plane count"
            );
            for (plane, (l, r)) in frame.planes().iter().zip(want.planes().iter()).enumerate() {
                assert_eq!(
                    l.stride(),
                    r.stride(),
                    "{codec} frame {index} plane {plane}"
                );
                assert_eq!(l.data(), r.data(), "{codec} frame {index} plane {plane}");
            }
        }
    }

    /// Equal generations in order.
    ///
    /// # Panics
    ///
    /// Panics when any frame pair carries different generations.
    pub fn assert_generations<F>(
        tag: &str,
        left: &[F],
        right: &[F],
        generation: impl Fn(&F) -> u64,
    ) {
        for (index, (l, r)) in left.iter().zip(right.iter()).enumerate() {
            assert_eq!(
                generation(l),
                generation(r),
                "{tag}: frame {index} generation"
            );
        }
    }

    /// Every frame expected, tight-coded, in generation order.
    ///
    /// # Panics
    ///
    /// Panics when the count, a byte length, or a generation differs.
    pub fn assert_ordered<F>(
        tag: &str,
        frames: &[F],
        expected: usize,
        frame_bytes: usize,
        bytes: impl Fn(&F) -> &[u8],
        generation: impl Fn(&F) -> u64,
    ) {
        assert_eq!(frames.len(), expected, "{tag} yields every frame");
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(
                bytes(frame).len(),
                frame_bytes,
                "{tag} frame {index} holds tight NV12"
            );
            assert_eq!(
                generation(frame),
                index as u64,
                "{tag} frame {index} keeps order"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fmt;
    use std::time::Duration;

    use rust_video_caps::delivery::{
        DropPolicy, PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
    };
    use rust_video_caps::packet::Packet;
    use rust_video_caps::traits::Decoder;
    use rust_video_caps::transcode::GpuCaps;

    use super::{Drive, Packets, Parity, SERIAL};
    use crate::Synth;

    #[derive(Debug)]
    struct PumpError;

    impl fmt::Display for PumpError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "pump error")
        }
    }

    impl std::error::Error for PumpError {}

    struct EchoPump {
        bound: usize,
        ready: VecDeque<Vec<u8>>,
        finished: bool,
    }

    impl EchoPump {
        fn open(bound: usize) -> Self {
            Self {
                bound,
                ready: VecDeque::new(),
                finished: false,
            }
        }
    }

    impl SendPackets for EchoPump {
        type Error = PumpError;

        fn try_send(&mut self, packet: Packet) -> Result<(), TrySendError<Self::Error>> {
            if self.ready.len() >= self.bound {
                return Err(TrySendError::Full { packet });
            }
            self.ready.push_back(packet.data().to_vec());
            Ok(())
        }

        fn finish(&mut self) -> Result<(), Self::Error> {
            self.finished = true;
            Ok(())
        }
    }

    impl PollFrames for EchoPump {
        type Frame = Vec<u8>;
        type Error = PumpError;

        fn try_receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
            Ok(self.ready.pop_front())
        }
    }

    impl ReceiveFrames for EchoPump {
        type Frame = Vec<u8>;
        type Error = PumpError;

        fn receive(&mut self) -> Result<Option<Self::Frame>, Self::Error> {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            panic!("echo pump never blocks with sends outstanding");
        }

        fn receive_timeout(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<Self::Frame>, ReceiveError<Self::Error>> {
            Ok(self.ready.pop_front())
        }
    }

    struct EchoDecoder;

    impl Decoder for EchoDecoder {
        type Frame = Vec<u8>;
        type Error = PumpError;

        fn caps(&self) -> GpuCaps {
            GpuCaps::new(None, false, false)
        }

        fn decode(&mut self, packet: &Packet) -> Result<Vec<Self::Frame>, Self::Error> {
            Ok(vec![packet.data().to_vec()])
        }

        fn flush(&mut self) -> Result<Vec<Self::Frame>, Self::Error> {
            Ok(Vec::new())
        }
    }

    fn raws() -> Vec<Vec<u8>> {
        vec![vec![1, 2], vec![3, 4, 5], vec![6], vec![7, 8], vec![9]]
    }

    fn blocking(pump: &mut EchoPump, frames: &mut Vec<Vec<u8>>) {
        frames.push(pump.receive().expect("ready frame").expect("unfinished"));
    }

    #[test]
    fn packets_preserve_raw_order() {
        let packets = Packets::from_raws(&raws());
        assert_eq!(packets.len(), 5);
        assert_eq!(packets[1].data(), &[3u8, 4, 5]);
    }

    #[test]
    fn channel_holds_capacity_and_policy() {
        let config = Drive::channel(4, DropPolicy::DropOldest);
        assert_eq!(config.capacity(), 4);
        assert_eq!(config.policy(), DropPolicy::DropOldest);
    }

    #[test]
    #[should_panic(expected = "nonzero capacity")]
    fn channel_rejects_zero() {
        let _ = Drive::channel(0, DropPolicy::DropOldest);
    }

    #[test]
    fn send_all_refuses_past_bound() {
        let packets = Packets::from_raws(&raws());
        let mut pump = EchoPump::open(2);
        let (mut frames, fulls) = Drive::send_all(&mut pump, &packets, blocking);
        assert!(fulls > 0, "rapid sends hit the bound");
        frames.extend(Drive::finish_all(&mut pump));
        assert_eq!(frames, raws());
    }

    #[test]
    fn decode_all_echoes_packets() {
        let packets = Packets::from_raws(&raws());
        assert_eq!(Drive::decode_all(&mut EchoDecoder, &packets), raws());
    }

    #[test]
    fn sustained_counts_every_frame() {
        let wide: Vec<Vec<u8>> = raws().iter().cycle().take(500).cloned().collect();
        let packets = Packets::from_raws(&wide);
        let mut pump = EchoPump::open(2);
        let (fps, count) = Drive::sustained(&mut pump, &packets, blocking);
        assert_eq!(count, wide.len());
        assert!(fps > 0.0, "timed pass measures");
    }

    #[test]
    fn parity_accepts_matching_legs() {
        let left = raws();
        let right = raws();
        Parity::assert_bytes("echo", &left, &right, Vec::as_slice);
        Parity::assert_generations("echo", &left, &right, |frame| frame.len() as u64);
    }

    #[test]
    #[should_panic(expected = "echo: frame 0 bytes")]
    fn parity_rejects_mismatch() {
        Parity::assert_bytes("echo", &[vec![1]], &[vec![2]], Vec::as_slice);
    }

    #[test]
    fn ordered_accepts_tight_sequence() {
        let frames = vec![vec![0, 2], vec![1, 4]];
        Parity::assert_ordered("echo", &frames, 2, 2, Vec::as_slice, |frame| {
            u64::from(frame[0])
        });
    }

    #[test]
    fn cpu_frames_accept_same_seed() {
        let frames = vec![Synth::noise(64, 64, 7)];
        let expected = vec![Synth::noise(64, 64, 7)];
        Parity::assert_cpu_frames("echo", &frames, &expected);
    }

    #[test]
    fn serial_excludes() {
        let _guard = Drive::serial();
        assert!(SERIAL.try_lock().is_err());
    }
}
