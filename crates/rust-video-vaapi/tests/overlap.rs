//! VA-API pipelined readback: parity, overlap, and latency.
//!
//! Skips without hardware; `RUST_VIDEO_VAAPI_DEPTH` overrides the
//! overlapped depth under test. Fails when the contract breaks.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, H264Synth};
use rust_video_vaapi::{
    Av1Decoder, H264Decoder, H265Decoder, PhaseStats, SubmitStats, VaApiDevice, VaApiFrame,
};

/// Repeat passes over single-picture packets.
const PASSES: usize = 8;

/// Open device, loud skip without hardware.
fn device(name: &str) -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("SKIP vaapi {name} overlap: {error}");
            None
        }
    }
}

/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// Overlapped depth under test: env override or three.
fn depth() -> usize {
    match std::env::var("RUST_VIDEO_VAAPI_DEPTH") {
        Ok(text) => text
            .parse::<usize>()
            .unwrap_or_else(|_| panic!("RUST_VIDEO_VAAPI_DEPTH parses: {text}"))
            .clamp(1, 4),
        Err(_) => 3,
    }
}

/// Pipeline knobs behind one decoder.
trait Pipelined: Decoder<Frame = VaApiFrame> {
    /// Submitted-ahead ceiling, clamped to 1..=4.
    fn set_depth(&mut self, depth: usize);

    /// Phase timings of the live context.
    fn phases(&self) -> Option<PhaseStats>;

    /// Submit gauges of the live context.
    fn submits(&self) -> Option<SubmitStats>;
}

impl Pipelined for H264Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }

    fn phases(&self) -> Option<PhaseStats> {
        self.phase_stats()
    }

    fn submits(&self) -> Option<SubmitStats> {
        self.submit_stats()
    }
}

impl Pipelined for H265Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }

    fn phases(&self) -> Option<PhaseStats> {
        self.phase_stats()
    }

    fn submits(&self) -> Option<SubmitStats> {
        self.submit_stats()
    }
}

impl Pipelined for Av1Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }

    fn phases(&self) -> Option<PhaseStats> {
        self.phase_stats()
    }

    fn submits(&self) -> Option<SubmitStats> {
        self.submit_stats()
    }
}

/// Frames for `passes` runs of `packets`, plus wall time.
fn run(
    decoder: &mut impl Pipelined,
    packets: &[Packet],
    passes: usize,
) -> (Vec<VaApiFrame>, Duration) {
    let start = Instant::now();
    let mut frames = Vec::new();
    for _ in 0..passes {
        for packet in packets {
            frames.extend(decoder.decode(packet).expect("overlap decode"));
        }
    }
    frames.extend(decoder.flush().expect("overlap flush"));
    (frames, start.elapsed())
}

/// Byte-exact frames across both legs.
fn assert_parity(name: &str, sync: &[VaApiFrame], pipelined: &[VaApiFrame]) {
    assert_eq!(sync.len(), pipelined.len(), "{name}: frame count");
    assert!(!sync.is_empty(), "{name}: legs decoded no frames");
    for (index, (left, right)) in sync.iter().zip(pipelined.iter()).enumerate() {
        assert_eq!(
            left.identity().generation(),
            right.identity().generation(),
            "{name}: frame {index} generation"
        );
        assert_eq!(left.nv12(), right.nv12(), "{name}: frame {index} bytes");
    }
    println!(
        "vaapi {name} pipeline parity: {} frames byte-exact",
        sync.len()
    );
}

/// Phase table line for one leg.
fn report(name: &str, leg: &str, wall: Duration, phases: &PhaseStats, submits: &SubmitStats) {
    let frames = phases.frames();
    assert!(frames > 0, "{name}: leg decoded no frames");
    println!(
        "vaapi {name} {leg}: wall {:.1}ms frames {} submit {}us wait {}us copy {}us latency {}us polled {}/{} overlapped {} peak {}",
        wall.as_secs_f64() * 1000.0,
        frames,
        phases.submit_us() / frames,
        phases.wait_us() / frames,
        phases.copy_us() / frames,
        phases.latency_us() / frames,
        phases.polled_ready(),
        frames,
        phases.overlapped(),
        submits.peak(),
    );
}

/// Leg stats, failing loud before the backend opens.
fn stats(decoder: &impl Pipelined, name: &str) -> (PhaseStats, SubmitStats) {
    match (decoder.phases(), decoder.submits()) {
        (Some(phases), Some(submits)) => (phases, submits),
        _ => panic!("{name}: backend closed"),
    }
}

/// H.264 depth-one and pipelined bytes match exactly.
#[test]
fn h264_pipeline_matches_sync_bytes() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let packets = packets(&raws);
    let mut sync = H264Decoder::open(&device);
    sync.set_depth(1);
    let (sync_frames, _) = run(&mut sync, &packets, PASSES);
    let mut pipelined = H264Decoder::open(&device);
    pipelined.set_depth(depth());
    let (pipelined_frames, _) = run(&mut pipelined, &packets, PASSES);
    assert_parity("h264", &sync_frames, &pipelined_frames);
}

/// H.264 pipelined reads overlap engine runs.
#[test]
fn h264_pipeline_overlaps_copy_with_engine() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let packets = packets(&raws);
    let mut decoder = H264Decoder::open(&device);
    decoder.set_depth(depth());
    let (frames, wall) = run(&mut decoder, &packets, PASSES);
    assert!(frames.len() >= 3, "h264: overlap needs three frames");
    let (phases, submits) = stats(&decoder, "h264");
    report("h264", "pipelined", wall, &phases, &submits);
    assert!(submits.peak() > 1, "h264: submits ahead {}", submits.peak());
    assert!(
        phases.overlapped() > 0,
        "h264: no copy overlapped an engine run"
    );
}

/// H.264 pipelined per-frame latency matches sync.
#[test]
fn h264_pipeline_keeps_per_frame_latency() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let packets = packets(&raws);
    let mut sync = H264Decoder::open(&device);
    sync.set_depth(1);
    let (sync_frames, sync_wall) = run(&mut sync, &packets, PASSES);
    let (sync_phases, sync_submits) = stats(&sync, "h264");
    report("h264", "sync", sync_wall, &sync_phases, &sync_submits);
    let mut pipelined = H264Decoder::open(&device);
    pipelined.set_depth(depth());
    let (pipelined_frames, pipelined_wall) = run(&mut pipelined, &packets, PASSES);
    let (pipelined_phases, pipelined_submits) = stats(&pipelined, "h264");
    report(
        "h264",
        "pipelined",
        pipelined_wall,
        &pipelined_phases,
        &pipelined_submits,
    );
    assert_parity("h264", &sync_frames, &pipelined_frames);
    let sync_latency = sync_phases.latency_us() / sync_phases.frames().max(1);
    let pipelined_latency = pipelined_phases.latency_us() / pipelined_phases.frames().max(1);
    assert!(sync_latency > 0, "h264: sync latency unmeasured");
    assert!(
        pipelined_latency * 2 <= sync_latency * 3,
        "h264: latency {pipelined_latency}us vs sync {sync_latency}us"
    );
    assert!(
        pipelined_wall <= sync_wall * 2,
        "h264: wall regressed past twice sync"
    );
}

/// H.265 depth-one and pipelined bytes match exactly.
#[test]
fn h265_pipeline_matches_sync_bytes() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = packets(&raws);
    let mut sync = H265Decoder::open(&device);
    sync.set_depth(1);
    let (sync_frames, _) = run(&mut sync, &packets, 1);
    let mut pipelined = H265Decoder::open(&device);
    pipelined.set_depth(depth());
    let (pipelined_frames, _) = run(&mut pipelined, &packets, 1);
    assert_parity("h265", &sync_frames, &pipelined_frames);
}

/// H.265 pipelined reads overlap engine runs.
#[test]
fn h265_pipeline_overlaps_copy_with_engine() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = packets(&raws);
    let mut decoder = H265Decoder::open(&device);
    decoder.set_depth(depth());
    let (frames, wall) = run(&mut decoder, &packets, 1);
    assert!(frames.len() >= 3, "h265: overlap needs three frames");
    let (phases, submits) = stats(&decoder, "h265");
    report("h265", "pipelined", wall, &phases, &submits);
    assert!(submits.peak() > 1, "h265: submits ahead {}", submits.peak());
    assert!(
        phases.overlapped() > 0,
        "h265: no copy overlapped an engine run"
    );
}

/// AV1 depth-one and pipelined bytes match exactly.
#[test]
fn av1_pipeline_matches_sync_bytes() {
    let Some(device) = device("av1") else {
        return;
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = packets(&[bytes]);
    let mut sync = Av1Decoder::open(&device);
    sync.set_depth(1);
    let (sync_frames, _) = run(&mut sync, &packets, PASSES);
    let mut pipelined = Av1Decoder::open(&device);
    pipelined.set_depth(depth());
    let (pipelined_frames, _) = run(&mut pipelined, &packets, PASSES);
    assert_parity("av1", &sync_frames, &pipelined_frames);
}

/// AV1 pipelined reads overlap engine runs.
#[test]
fn av1_pipeline_overlaps_copy_with_engine() {
    let Some(device) = device("av1") else {
        return;
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = packets(&[bytes]);
    let mut decoder = Av1Decoder::open(&device);
    decoder.set_depth(depth());
    let (frames, wall) = run(&mut decoder, &packets, PASSES);
    assert!(frames.len() >= 3, "av1: overlap needs three frames");
    let (phases, submits) = stats(&decoder, "av1");
    report("av1", "pipelined", wall, &phases, &submits);
    assert!(submits.peak() > 1, "av1: submits ahead {}", submits.peak());
    assert!(
        phases.overlapped() > 0,
        "av1: no copy overlapped an engine run"
    );
}
