//! Multi-inflight overlap: byte parity, phase timing, depth proof.
//!
//! Skips without hardware; fails when the contract breaks.

use std::time::{Duration, Instant};

use rust_video_caps::delivery::{
    DropPolicy, PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets, SplitDecode,
    SubmitStats, TrySendError,
};
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth, Packets, Parity};
use rust_video_vulkan::{
    Av1Decoder, CodecOp, H264Decoder, H265Decoder, OverlapPump, VulkanDevice, VulkanError,
    VulkanFrame,
};

/// Device for `op`, skipping loud when the tier declines.
fn device_or_skip(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open(op) {
        Ok(Some(device)) => Some(device),
        Ok(None) => {
            println!("vulkan skipped: {op:?} declined on this box");
            None
        }
        Err(error) => {
            println!("vulkan skipped: {op:?} failed: {error}");
            None
        }
    }
}

/// Frames surfacing under backpressure into `frames`, freeing send room.
fn drain<D>(pump: &mut OverlapPump<D>, frames: &mut Vec<VulkanFrame>)
where
    D: SplitDecode<Frame = VulkanFrame, Error = VulkanError>,
{
    let mut surfaced = false;
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
        surfaced = true;
    }
    if !surfaced {
        match pump.receive_timeout(Duration::from_secs(10)) {
            Ok(frame) => frames.extend(frame),
            Err(error) => panic!("completions stall: {error:?}"),
        }
    }
}

/// Assert `overlap` frames match `sync` frames byte for byte.
fn assert_byte_parity(codec: &str, sync: &[VulkanFrame], overlap: &[VulkanFrame]) {
    Parity::assert_bytes(codec, sync, overlap, VulkanFrame::nv12);
    Parity::assert_generations(codec, sync, overlap, |frame| frame.identity().generation());
}

/// Mean stage sums of `timings`: submit, engine-wait, copy, sum.
fn mean_phases(timings: &[PhaseTiming]) -> (f64, f64, f64, f64) {
    let count = f64::from(u32::try_from(timings.len().max(1)).expect("count fits"));
    let mut submit = 0.0;
    let mut wait = 0.0;
    let mut copy = 0.0;
    for timing in timings {
        submit += timing.submit().as_secs_f64();
        wait += timing.engine_wait().as_secs_f64();
        copy += timing.copy().as_secs_f64();
    }
    (
        submit / count,
        wait / count,
        copy / count,
        (submit + wait + copy) / count,
    )
}

/// True when some later submit precedes an earlier collection.
fn interleaved(timings: &[PhaseTiming]) -> bool {
    timings
        .windows(2)
        .any(|pair| pair[1].submitted_at() < pair[0].collected_at())
}

/// Print the sync versus overlapped phase table plus walls and peaks.
fn print_phase_table(
    codec: &str,
    sync: &[PhaseTiming],
    overlap: &[PhaseTiming],
    sync_wall: Duration,
    overlap_wall: Duration,
    sync_peak: usize,
    overlap_peak: usize,
) {
    println!("{codec}: per-frame phases in microseconds, sync vs overlapped");
    println!("frame | sync submit/wait/copy/sum | overlap submit/wait/copy/sum");
    for (index, (left, right)) in sync.iter().zip(overlap.iter()).enumerate() {
        println!(
            "{index:>5} | {:>6.0}/{:>6.0}/{:>6.0}/{:>6.0} | {:>6.0}/{:>6.0}/{:>6.0}/{:>6.0}",
            left.submit().as_secs_f64() * 1e6,
            left.engine_wait().as_secs_f64() * 1e6,
            left.copy().as_secs_f64() * 1e6,
            left.stage_sum().as_secs_f64() * 1e6,
            right.submit().as_secs_f64() * 1e6,
            right.engine_wait().as_secs_f64() * 1e6,
            right.copy().as_secs_f64() * 1e6,
            right.stage_sum().as_secs_f64() * 1e6,
        );
    }
    let (ss, sw, sc, st) = mean_phases(sync);
    let (os, ow, oc, ot) = mean_phases(overlap);
    println!(
        "mean  | {s:>6.0}/{w:>6.0}/{c:>6.0}/{t:>6.0} | {os:>6.0}/{ow:>6.0}/{oc:>6.0}/{ot:>6.0}",
        s = ss * 1e6,
        w = sw * 1e6,
        c = sc * 1e6,
        t = st * 1e6,
        os = os * 1e6,
        ow = ow * 1e6,
        oc = oc * 1e6,
        ot = ot * 1e6,
    );
    println!(
        "{codec}: wall sync {:.1}ms vs overlap {:.1}ms; peak sync {sync_peak} vs overlap {overlap_peak}",
        sync_wall.as_secs_f64() * 1e3,
        overlap_wall.as_secs_f64() * 1e3,
    );
}

/// Assert overlapped host phases match sync within `ratio`.
fn assert_phase_parity(codec: &str, sync: &[PhaseTiming], overlap: &[PhaseTiming], ratio: f64) {
    let (ss, _, sc, _) = mean_phases(sync);
    let (os, _, oc, _) = mean_phases(overlap);
    assert!(
        os <= ss * ratio,
        "{codec}: overlapped mean submit {os:.6}s within {ratio}x of sync {ss:.6}s"
    );
    assert!(
        oc <= sc * ratio,
        "{codec}: overlapped mean copy {oc:.6}s within {ratio}x of sync {sc:.6}s"
    );
}

/// H.264 synth sequence repeated `repeats` times: sets plus pictures.
fn h264_stream(repeats: usize) -> Vec<Vec<u8>> {
    let unit = H264Synth::annex_b_packets();
    let mut raws = Vec::with_capacity(unit.len() * repeats);
    for _ in 0..repeats {
        raws.extend(unit.iter().cloned());
    }
    raws
}

/// Sync decode of `raws`: frames, timings, gauges, wall.
fn run_sync_h264(
    device: &VulkanDevice,
    raws: &[Vec<u8>],
) -> (Vec<VulkanFrame>, Vec<PhaseTiming>, SubmitStats, Duration) {
    let mut decoder = H264Decoder::open(device).expect("decode opens");
    let start = Instant::now();
    let frames = Drive::decode_all(&mut decoder, &Packets::from_raws(raws));
    let wall = start.elapsed();
    let timings = decoder.take_timings();
    (frames, timings, decoder.submit_stats(), wall)
}

/// Overlapped decode of `raws`: frames, timings, gauges, wall.
fn run_overlap_h264(
    device: &VulkanDevice,
    raws: &[Vec<u8>],
) -> (Vec<VulkanFrame>, Vec<PhaseTiming>, SubmitStats, Duration) {
    let mut pump = OverlapPump::open(
        H264Decoder::open(device).expect("decode opens"),
        Drive::channel(raws.len() * 2 + 8, DropPolicy::DropOldest),
    );
    let start = Instant::now();
    let (mut frames, _) = Drive::send_all(&mut pump, &Packets::from_raws(raws), drain);
    frames.extend(Drive::finish_all(&mut pump));
    assert!(pump.is_finished());
    let wall = start.elapsed();
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    let timings = pump.decoder_mut().take_timings();
    (frames, timings, pump.decoder().submit_stats(), wall)
}

/// H.264 byte parity plus phase table plus latency parity.
#[test]
fn h264_overlap_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    if H264Decoder::open(&device).is_err() {
        println!("vulkan skipped: h264 decode open failed");
        return;
    }
    let raws = h264_stream(6);
    let (sync_frames, sync_timings, sync_stats, sync_wall) = run_sync_h264(&device, &raws);
    assert!(!sync_frames.is_empty(), "synth emits frames");
    assert_eq!(sync_timings.len(), sync_frames.len());
    let (overlap_frames, overlap_timings, overlap_stats, overlap_wall) =
        run_overlap_h264(&device, &raws);
    assert_byte_parity("h264", &sync_frames, &overlap_frames);
    assert_eq!(overlap_timings.len(), overlap_frames.len());
    print_phase_table(
        "h264",
        &sync_timings,
        &overlap_timings,
        sync_wall,
        overlap_wall,
        sync_stats.peak_inflight(),
        overlap_stats.peak_inflight(),
    );
    assert!(
        overlap_stats.peak_inflight() >= 2,
        "overlap observed: peak {}",
        overlap_stats.peak_inflight()
    );
    assert!(
        interleaved(&overlap_timings),
        "a later submit precedes an earlier collection"
    );
    assert_phase_parity("h264", &sync_timings, &overlap_timings, 3.0);
    assert!(
        overlap_wall <= sync_wall.mul_f32(1.5),
        "overlapped wall no worse than sync within noise"
    );
}

/// H.264 in-flight depth exceeds one on hardware.
#[test]
fn h264_overlap_depth_exceeds_one() {
    let _guard = Drive::serial();
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let Ok(decoder) = H264Decoder::open(&device) else {
        println!("vulkan skipped: h264 decode open failed");
        return;
    };
    let mut pump = OverlapPump::open(decoder, Drive::channel(64, DropPolicy::DropOldest));
    let mut frames = Vec::new();
    let mut peak = 0;
    for packet in Packets::from_raws(&h264_stream(6)) {
        let mut pending = packet;
        loop {
            match pump.try_send(pending) {
                Ok(()) => break,
                Err(TrySendError::Full { packet }) => {
                    pending = packet;
                    drain(&mut pump, &mut frames);
                }
                Err(TrySendError::Failed { error }) => panic!("submit runs: {error}"),
            }
        }
        peak = peak.max(pump.decoder().inflight_len());
    }
    frames.extend(Drive::finish_all(&mut pump));
    assert!(pump.is_finished());
    peak = peak.max(pump.decoder().submit_stats().peak_inflight());
    println!("h264: observed in-flight depth {peak}");
    assert!(peak >= 2, "depth observed above one: {peak}");
    assert!(!frames.is_empty(), "stream emits frames");
}

/// H.264 blocking receive, timeout, and finish contract.
#[test]
fn h264_overlap_receive_paths() {
    let _guard = Drive::serial();
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let Ok(decoder) = H264Decoder::open(&device) else {
        println!("vulkan skipped: h264 decode open failed");
        return;
    };
    let mut pump = OverlapPump::open(decoder, Drive::channel(16, DropPolicy::DropOldest));
    let Err(err) = pump.receive_timeout(Duration::from_millis(10)) else {
        panic!("empty open pump times out")
    };
    assert!(
        matches!(err, ReceiveError::Timeout),
        "empty open pump times out"
    );
    let sent = Packets::from_raws(&H264Synth::annex_b_packets());
    for packet in &sent {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    while let Some(frame) = pump.receive().expect("blocking drain") {
        generations.push(frame.identity().generation());
        if generations.len() == sent.len() {
            break;
        }
    }
    assert_eq!(generations.len(), sent.len(), "every picture arrives");
    pump.finish().expect("finish drains");
    pump.finish().expect("second finish holds");
    assert!(pump.receive().expect("drained").is_none());
    let err = pump.try_send(sent[0].clone()).unwrap_err();
    assert!(
        matches!(err, TrySendError::Full { .. }),
        "full after finish"
    );
}

/// True when `error` is the tier declining the vector's PTL profile.
fn is_profile_gap(error: &rust_video_vulkan::VulkanError) -> bool {
    matches!(
        error,
        rust_video_vulkan::VulkanError::Parse(rust_video_vulkan::ParseError::OutOfRange {
            syntax: "ptl profile",
        })
    )
}

/// H.265 byte parity plus phase table plus latency parity.
#[test]
fn h265_overlap_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device_or_skip(CodecOp::H265Decode) else {
        return;
    };
    let Ok(sync_decoder) = H265Decoder::open(&device) else {
        println!("vulkan skipped: h265 decode open failed");
        return;
    };
    let unit = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    assert!(!unit.is_empty(), "bench vector holds pictures");
    let mut raws = Vec::with_capacity(unit.len() * 8);
    for _ in 0..8 {
        raws.extend(unit.iter().cloned());
    }
    let mut decoder = sync_decoder;
    let start = Instant::now();
    let mut sync_frames = Vec::new();
    for packet in &Packets::from_raws(&raws) {
        match decoder.decode(packet) {
            Ok(frames) => sync_frames.extend(frames),
            Err(error) if is_profile_gap(&error) => {
                println!(
                    "vulkan skipped: h265 bench vector declares ptl profile 4, tier parses 1-3"
                );
                return;
            }
            Err(error) => panic!("decode runs: {error}"),
        }
    }
    sync_frames.extend(decoder.flush().expect("flush drains"));
    let sync_wall = start.elapsed();
    let sync_timings = decoder.take_timings();
    let sync_stats = decoder.submit_stats();
    assert!(!sync_frames.is_empty(), "bench vector emits frames");

    let mut pump = OverlapPump::open(
        H265Decoder::open(&device).expect("decode opens"),
        Drive::channel(raws.len() + 16, DropPolicy::DropOldest),
    );
    let start = Instant::now();
    let (mut overlap_frames, _) = Drive::send_all(&mut pump, &Packets::from_raws(&raws), drain);
    overlap_frames.extend(Drive::finish_all(&mut pump));
    assert!(pump.is_finished());
    let overlap_wall = start.elapsed();
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    let overlap_timings = pump.decoder_mut().take_timings();
    let overlap_stats = pump.decoder().submit_stats();

    assert_byte_parity("h265", &sync_frames, &overlap_frames);
    print_phase_table(
        "h265",
        &sync_timings,
        &overlap_timings,
        sync_wall,
        overlap_wall,
        sync_stats.peak_inflight(),
        overlap_stats.peak_inflight(),
    );
    assert!(
        overlap_stats.peak_inflight() >= 2,
        "overlap observed: peak {}",
        overlap_stats.peak_inflight()
    );
    assert!(
        interleaved(&overlap_timings),
        "a later submit precedes an earlier collection"
    );
    assert_phase_parity("h265", &sync_timings, &overlap_timings, 3.0);
}

/// AV1 byte parity plus phase table plus latency parity.
#[test]
fn av1_overlap_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device_or_skip(CodecOp::Av1Decode) else {
        return;
    };
    let Ok(sync_decoder) = Av1Decoder::open(&device) else {
        println!("vulkan skipped: av1 decode open failed");
        return;
    };
    let unit = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let raws = vec![unit; 4];
    let mut decoder = sync_decoder;
    let start = Instant::now();
    let sync_frames = Drive::decode_all(&mut decoder, &Packets::from_raws(&raws));
    let sync_wall = start.elapsed();
    let sync_timings = decoder.take_timings();
    let sync_stats = decoder.submit_stats();
    assert!(!sync_frames.is_empty(), "bench vector emits frames");

    let mut pump = OverlapPump::open(
        Av1Decoder::open(&device).expect("decode opens"),
        Drive::channel(sync_frames.len() + 16, DropPolicy::DropOldest),
    );
    let start = Instant::now();
    let (mut overlap_frames, _) = Drive::send_all(&mut pump, &Packets::from_raws(&raws), drain);
    overlap_frames.extend(Drive::finish_all(&mut pump));
    assert!(pump.is_finished());
    let overlap_wall = start.elapsed();
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    let overlap_timings = pump.decoder_mut().take_timings();
    let overlap_stats = pump.decoder().submit_stats();

    assert_byte_parity("av1", &sync_frames, &overlap_frames);
    print_phase_table(
        "av1",
        &sync_timings,
        &overlap_timings,
        sync_wall,
        overlap_wall,
        sync_stats.peak_inflight(),
        overlap_stats.peak_inflight(),
    );
    assert!(
        overlap_stats.peak_inflight() >= 2,
        "overlap observed: peak {}",
        overlap_stats.peak_inflight()
    );
    assert!(
        interleaved(&overlap_timings),
        "a later submit precedes an earlier collection"
    );
    assert_phase_parity("av1", &sync_timings, &overlap_timings, 3.0);
}
