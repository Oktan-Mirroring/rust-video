//! Pipelined overlap: byte parity, depth, phase timing over libavcodec.
//!
//! Lab-gated: needs the ffmpeg system libraries behind the `ffmpeg` feature.

#![cfg(feature = "ffmpeg")]

use std::time::{Duration, Instant};

use rust_video_caps::delivery::{DropPolicy, PollFrames, ReceiveFrames, SplitDecode};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, Drive, H264Synth, Ivf, Packets, Parity};
use rust_video_sw::{
    H264Decoder, PhaseStats, PipelineConfig, PipelinedDecoder, PipelinedPump, RawSource, SwError,
    Vp9Decoder,
};

/// Ready frames moved into `frames`, waiting for one when empty.
fn drain<D>(pump: &mut PipelinedPump<D>, frames: &mut Vec<CpuFrame>)
where
    D: SplitDecode<Frame = CpuFrame>,
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
    let mut pump = PipelinedPump::open(
        decoder,
        Drive::channel(packets.len() + 8, DropPolicy::DropOldest),
    );
    let (mut frames, _) = Drive::send_all(&mut pump, packets, drain);
    frames.extend(Drive::finish_all(&mut pump));
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert_eq!(
        pump.stats().submitted(),
        u64::try_from(packets.len()).expect("packet count fits")
    );
    let stats = pump.decoder().phase_stats();
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
    let one = Packets::from_raws(&raws);
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
    let expected = Drive::decode_all(&mut open(), &packets);
    assert!(!expected.is_empty(), "h264 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::ZERO, &packets);
    Parity::assert_cpu_frames("h264", &frames, &expected);
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
    let packets = Packets::from_raws(&raws);
    let expected = Drive::decode_all(&mut open(), &packets);
    assert!(!expected.is_empty(), "vp9 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::ZERO, &packets);
    Parity::assert_cpu_frames("vp9", &frames, &expected);
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
    let expected = Drive::decode_all(&mut open(), &packets);
    assert!(!expected.is_empty(), "h264 vector decodes");
    let (frames, stats) = pipelined(open(), Duration::from_millis(20), &packets);
    Parity::assert_cpu_frames("h264", &frames, &expected);
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
    let packets = Packets::from_raws(&raws);
    let warm = {
        let mut decoder = open();
        Drive::decode_all(&mut decoder, &packets).len()
    };
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
