//! Pipelined overlap: byte parity, depth, phase timing over libavcodec.
//!
//! Lab-gated: needs the ffmpeg system libraries behind the `ffmpeg` feature.

#![cfg(feature = "ffmpeg")]

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, H264Synth, Ivf};
use rust_video_sw::{
    H264Decoder, PhaseStats, PipelineConfig, PipelinedDecoder, PipelinedPump, RawSource,
    SplitDecoder, SwError, Vp9Decoder,
};

/// Bound of `capacity` frames under `policy`.
fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
    ChannelConfig::new(
        NonZeroUsize::new(capacity).expect("nonzero capacity"),
        policy,
    )
}

/// Packets over `raws`, built once outside the pump.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// Frames decoded straight through `decoder`, the pump oracle.
fn oracle<D>(decoder: &mut D, packets: &[Packet]) -> Vec<CpuFrame>
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut frames = Vec::new();
    for packet in packets {
        frames.extend(decoder.decode(packet).expect("oracle decode"));
    }
    frames.extend(decoder.flush().expect("oracle flush"));
    frames
}

/// Ready frames moved into `frames`, waiting for one when empty.
fn drain_ready<D>(pump: &mut PipelinedPump<D>, frames: &mut Vec<CpuFrame>)
where
    D: SplitDecoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut moved = false;
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
        moved = true;
    }
    if moved {
        return;
    }
    match pump.receive_timeout(Duration::from_secs(5)) {
        Ok(Some(frame)) => frames.push(frame),
        Ok(None) => panic!("stream ended with sends outstanding"),
        Err(error) => panic!("no completion within 5s: {error:?}"),
    }
}

/// Frames through the pipelined path plus its stats.
fn pipelined<D>(inner: D, pace: Duration, packets: &[Packet]) -> (Vec<CpuFrame>, PhaseStats)
where
    D: RawSource<Frame = CpuFrame, Error = SwError>,
{
    let decoder = PipelinedDecoder::open(inner, PipelineConfig::default().with_pace(pace));
    let mut pump = PipelinedPump::open(decoder, config(packets.len() + 8, DropPolicy::DropOldest));
    let mut frames = Vec::new();
    let mut at = 0;
    while at < packets.len() {
        match pump.try_send(packets[at].clone()) {
            Ok(()) => at += 1,
            Err(TrySendError::Full { .. }) => drain_ready(&mut pump, &mut frames),
            Err(TrySendError::Failed { error }) => panic!("submit failed: {error}"),
        }
    }
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    pump.finish().expect("finish drains");
    while let Some(frame) = pump.receive().expect("drain") {
        frames.push(frame);
    }
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    let stats = pump.stats();
    (frames, stats)
}

/// H.264 probe, skipping loud when the backend declines.
fn h264_or_skip() -> Option<impl Fn() -> H264Decoder> {
    match H264Decoder::open(4) {
        Ok(_) => Some(|| H264Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw h264 overlap skipped: {error}");
            None
        }
    }
}

/// VP9 probe, skipping loud when the backend declines.
fn vp9_or_skip() -> Option<impl Fn() -> Vp9Decoder> {
    match Vp9Decoder::open(4) {
        Ok(_) => Some(|| Vp9Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw vp9 overlap skipped: {error}");
            None
        }
    }
}

/// One synth IDR repeated into `count` packets.
fn repeated_idr(count: usize) -> Vec<Packet> {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let one = packets(&raws);
    assert_eq!(one.len(), 1, "synth holds one IDR");
    one.iter().cycle().take(count).cloned().collect()
}

/// Pipelined H.264 matches sync bytes with conversions in flight.
#[test]
fn pipelined_matches_sync_bytes_h264() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let packets = repeated_idr(1);
    let expected = oracle(&mut open(), &packets);
    assert!(!expected.is_empty(), "h264 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::ZERO, &packets);
    assert_same_frames(&frames, &expected, "h264");
    assert!(stats.max_inflight() >= 1, "conversions ran");
    println!(
        "sw h264 overlap: max_inflight={} overlap={} frames={}",
        stats.max_inflight(),
        stats.overlap_observed(),
        frames.len()
    );
}

/// Pipelined VP9 matches sync bytes on the bench vector.
#[test]
fn pipelined_matches_sync_bytes_vp9_bench() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    let packets = packets(&raws);
    let expected = oracle(&mut open(), &packets);
    assert!(!expected.is_empty(), "vp9 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::ZERO, &packets);
    assert_same_frames(&frames, &expected, "vp9");
    println!(
        "sw vp9 overlap: frames={} max_inflight={} overlap={} submit_ms={:.3} engine_ms={:.3} copy_ms={:.3}",
        frames.len(),
        stats.max_inflight(),
        stats.overlap_observed(),
        stats.submit_total().as_secs_f64() * 1000.0,
        stats.engine_total().as_secs_f64() * 1000.0,
        stats.copy_total().as_secs_f64() * 1000.0
    );
}

/// Paced conversions refuse past depth and share time with submits.
#[test]
fn paced_overlap_observes_depth_above_one() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let packets = repeated_idr(32);
    let expected = oracle(&mut open(), &packets);
    assert!(!expected.is_empty(), "h264 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::from_millis(20), &packets);
    assert_same_frames(&frames, &expected, "h264");
    assert!(stats.max_inflight() > 1, "depth observed above one");
    assert!(stats.overlap_observed(), "submit shared conversion time");
    let serial = stats.submit_total() + stats.engine_total() + stats.copy_total();
    println!(
        "sw h264 paced overlap: max_inflight={} wall={:?} serial={serial:?}",
        stats.max_inflight(),
        stats.wall().expect("wall measured")
    );
}

/// Phase table: pipelined per-frame latency matches sync on 1080p VP9.
#[test]
fn phase_table_shows_latency_parity() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    let packets = packets(&raws);
    let mut warm = 0;
    {
        let mut decoder = open();
        for packet in &packets {
            warm += decoder.decode(packet).expect("warmup decode").len();
        }
        warm += decoder.flush().expect("warmup flush").len();
    }
    assert!(warm > 0, "vp9 vector decodes");
    let (warmed, _) = pipelined(open(), Duration::ZERO, &packets);
    assert!(!warmed.is_empty(), "pipelined warmup decodes");
    let start = Instant::now();
    let mut decoder = open();
    let mut first_sync = None;
    let mut sync_frames = 0;
    for packet in &packets {
        for _ in decoder.decode(packet).expect("timed decode") {
            if first_sync.is_none() {
                first_sync = Some(start.elapsed());
            }
            sync_frames += 1;
        }
    }
    for _ in decoder.flush().expect("timed flush") {
        if first_sync.is_none() {
            first_sync = Some(start.elapsed());
        }
        sync_frames += 1;
    }
    let wall_sync = start.elapsed();
    let start = Instant::now();
    let (frames, stats) = pipelined(open(), Duration::ZERO, &packets);
    let wall_pipe = start.elapsed();
    assert_eq!(frames.len(), sync_frames, "same frame count");
    let first_pipe = stats.first_frame().expect("first frame measured");
    let first_sync = first_sync.expect("sync first frame");
    println!(
        "sw vp9 phase: path=sync frames={sync_frames} wall_ms={:.3} first_ms={:.3}",
        wall_sync.as_secs_f64() * 1000.0,
        first_sync.as_secs_f64() * 1000.0
    );
    println!(
        "sw vp9 phase: path=pipe frames={} wall_ms={:.3} first_ms={:.3} max_inflight={} overlap={} submit_ms={:.3} engine_ms={:.3} copy_ms={:.3}",
        frames.len(),
        wall_pipe.as_secs_f64() * 1000.0,
        first_pipe.as_secs_f64() * 1000.0,
        stats.max_inflight(),
        stats.overlap_observed(),
        stats.submit_total().as_secs_f64() * 1000.0,
        stats.engine_total().as_secs_f64() * 1000.0,
        stats.copy_total().as_secs_f64() * 1000.0
    );
    assert!(
        wall_pipe <= wall_sync.mul_f64(1.5),
        "per-frame latency parity: pipe {wall_pipe:?} vs sync {wall_sync:?}"
    );
    assert!(
        first_pipe <= first_sync.mul_f64(2.0),
        "first-frame parity: pipe {first_pipe:?} vs sync {first_sync:?}"
    );
}

/// Same geometry, format, and plane bytes at `index` of `codec`.
fn assert_same_frame(left: &CpuFrame, right: &CpuFrame, index: usize, codec: &str) {
    assert_eq!(left.size(), right.size(), "{codec} frame {index} size");
    assert_eq!(
        left.format(),
        right.format(),
        "{codec} frame {index} format"
    );
    assert_eq!(
        left.planes().len(),
        right.planes().len(),
        "{codec} frame {index} plane count"
    );
    for (plane, (l, r)) in left.planes().iter().zip(right.planes().iter()).enumerate() {
        assert_eq!(
            l.stride(),
            r.stride(),
            "{codec} frame {index} plane {plane}"
        );
        assert_eq!(l.data(), r.data(), "{codec} frame {index} plane {plane}");
    }
}

/// Same frames in order.
fn assert_same_frames(frames: &[CpuFrame], expected: &[CpuFrame], codec: &str) {
    assert_eq!(frames.len(), expected.len(), "{codec} frame count");
    for (index, (frame, want)) in frames.iter().zip(expected.iter()).enumerate() {
        assert_same_frame(frame, want, index, codec);
    }
}
