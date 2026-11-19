//! Overlapped decode: byte parity, observed depth, stage parity.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "windows")]

use std::time::Duration;

use rust_video_d3d12::{
    Av1Decoder, Codec, D3d12Device, D3d12Error, D3d12Frame, H264Decoder, HevcDecoder,
    OverlappedAv1Pump, OverlappedH264Pump, OverlappedHevcPump,
};

use bytes::Bytes;
use rust_video_caps::delivery::{
    PhaseTiming, PollFrames, ReceiveError, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{decode_dir, AnnexB, Drive, H264Synth, Obu, Packets};

/// Pictures in the shared vector.
const PICTURES: usize = 15;

/// Temporal units in the shared AV1 vector.
const AV1_PACKETS: usize = 16;

/// Warmup pictures ahead of the depth probe.
const WARMUP_PICTURES: usize = 4;

/// Measured pictures behind the depth probe.
const DEPTH_PICTURES: usize = 6;

/// Pictures behind the depth drive, warmup plus measured.
const PROBE_PICTURES: usize = WARMUP_PICTURES + DEPTH_PICTURES;

/// Overlap margin floor in microseconds behind the depth probe.
const OVERLAP_FLOOR_US: u128 = 200;

/// Open device, loud skip without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            None
        }
    }
}

/// Shared H.264 vector, one packet per picture in order.
fn h264_packets() -> Vec<Packet> {
    let path = decode_dir().join("valid_320x240.h264");
    let packets = Packets::from_raws(&AnnexB::picture_packets(&path, AnnexB::is_h264_vcl));
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    packets
}

/// Shared HEVC inter vector, one packet per picture in order.
fn hevc_packets() -> Vec<Packet> {
    let path = decode_dir().join("inter_320x240.hevc");
    let data = std::fs::read(path).expect("vector readable");
    let packets = Packets::from_raws(&AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl));
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    packets
}

/// Shared AV1 vector, one packet per temporal unit in order.
fn av1_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let packets = Packets::from_raws(&Obu::tu_packets(&data));
    assert_eq!(
        packets.len(),
        AV1_PACKETS,
        "vector holds one packet per unit"
    );
    packets
}

/// 1080p sets-plus-IDR packets, one picture each.
fn idr_1080p_packets() -> Vec<Packet> {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert_eq!(raws.len(), 1, "synth holds sets plus one IDR");
    (0..PROBE_PICTURES)
        .map(|_| Packet::new(Bytes::copy_from_slice(&raws[0])))
        .collect()
}

/// Sync drive behind one vector pass.
struct SyncRun {
    /// NV12 bytes per frame in generation order.
    frames: Vec<Vec<u8>>,
    /// Decode wall microseconds per call.
    walls_us: Vec<u64>,
    /// Stream resets.
    resets: u64,
    /// Wall microseconds behind the drive.
    total_us: u64,
}

/// Overlapped drive behind one vector pass.
struct OverlapRun {
    /// NV12 bytes per frame in generation order.
    frames: Vec<Vec<u8>>,
    /// Phase-split timing per frame.
    timings: Vec<PhaseTiming>,
    /// Peak pipelined pictures with incomplete GPU work.
    max_depth: usize,
    /// Stream resets.
    resets: u64,
    /// Wall microseconds behind the drive.
    total_us: u64,
}

/// Whole microseconds behind `elapsed`, saturating.
fn micros(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// Median of `values`, zero when empty.
fn median(mut values: Vec<u64>) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

/// True when `value` sits within a factor of two of `baseline`.
fn within_two_x(value: u64, baseline: u64) -> bool {
    if baseline == 0 {
        return value == 0;
    }
    u128::from(value) * 2 >= u128::from(baseline) && u128::from(baseline) * 2 >= u128::from(value)
}

/// Sync frames plus per-call walls for `packets` through a fresh decoder.
fn drive_sync(device: &D3d12Device, packets: &[Packet]) -> SyncRun {
    let mut decoder = H264Decoder::open(device);
    let mut frames = Vec::new();
    let mut walls_us = Vec::new();
    let start = std::time::Instant::now();
    for (index, packet) in packets.iter().enumerate() {
        let call = std::time::Instant::now();
        frames.extend(
            decoder
                .decode(packet)
                .unwrap_or_else(|error| panic!("sync packet {index}: {error}")),
        );
        walls_us.push(micros(call.elapsed()));
    }
    frames.extend(decoder.flush().expect("sync flush drains"));
    let total_us = micros(start.elapsed());
    let resets = decoder.resets();
    let frames = frames
        .into_iter()
        .map(|frame| frame.nv12().to_vec())
        .collect();
    SyncRun {
        frames,
        walls_us,
        resets,
        total_us,
    }
}

/// One in-flight frame into `frames`, freeing send room within five seconds.
fn drain<P>(pump: &mut P, frames: &mut Vec<P::Frame>)
where
    P: ReceiveFrames,
    P::Error: std::error::Error,
{
    frames.push(
        pump.receive_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("bound drain: {error}"))
            .expect("bound drain completes"),
    );
}

/// Overlapped frames plus timings for `packets` through a fresh bound pump.
fn drive_overlapped(device: &D3d12Device, bound: usize, packets: &[Packet]) -> OverlapRun {
    let mut pump = OverlappedH264Pump::open(device, bound);
    assert_eq!(pump.bound(), bound);
    let start = std::time::Instant::now();
    let (mut frames, _) = Drive::send_all(&mut pump, packets, drain);
    frames.extend(Drive::finish_all(&mut pump));
    let total_us = micros(start.elapsed());
    let (max_depth, resets) = (pump.max_depth(), pump.resets());
    let timings = pump.timings().to_vec();
    let frames = frames
        .into_iter()
        .map(|frame| frame.nv12().to_vec())
        .collect();
    OverlapRun {
        frames,
        timings,
        max_depth,
        resets,
        total_us,
    }
}

/// Sync NV12 bytes for `packets` through `decoder`.
fn drive_sync_with(
    decoder: &mut impl Decoder<Frame = D3d12Frame, Error = D3d12Error>,
    packets: &[Packet],
) -> Vec<Vec<u8>> {
    Drive::decode_all(decoder, packets)
        .into_iter()
        .map(|frame| frame.nv12().to_vec())
        .collect()
}

/// Overlapped NV12 bytes for `packets` through `pump`.
fn drive_pump_with(
    pump: &mut (impl SendPackets<Error = D3d12Error>
              + PollFrames<Frame = D3d12Frame, Error = D3d12Error>
              + ReceiveFrames<Frame = D3d12Frame, Error = D3d12Error>),
    packets: &[Packet],
) -> Vec<Vec<u8>> {
    let (mut frames, _) = Drive::send_all(pump, packets, drain);
    frames.extend(Drive::finish_all(pump));
    frames
        .into_iter()
        .map(|frame| frame.nv12().to_vec())
        .collect()
}

/// Phase-split table behind `name` over `timings`.
fn print_phase_table(name: &str, timings: &[PhaseTiming]) {
    println!(
        "{name}: {} frames, gen submit_us engine_us copy_us latency_us",
        timings.len()
    );
    for timing in timings {
        println!(
            "{name} frame {}: {} {} {} {}",
            timing.seq(),
            timing.submit_us(),
            timing.engine_wait_us(),
            timing.copy_us(),
            timing.latency_us()
        );
    }
    let (mut submit, mut engine, mut copy, mut latency) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for timing in timings {
        submit.push(timing.submit_us());
        engine.push(timing.engine_wait_us());
        copy.push(timing.copy_us());
        latency.push(timing.latency_us());
    }
    println!(
        "{name} median: {} {} {} {}",
        median(submit),
        median(engine),
        median(copy),
        median(latency)
    );
}

/// Same vector bytes through sync and overlapped decode match exactly.
#[test]
fn overlap_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    assert_eq!(OverlappedH264Pump::open(&device, 0).bound(), 1);
    assert_eq!(
        OverlappedH264Pump::open(&device, 99).bound(),
        OverlappedH264Pump::MAX_INFLIGHT
    );
    let packets = h264_packets();
    let sync = drive_sync(&device, &packets);
    let one = drive_overlapped(&device, 1, &packets);
    let four = drive_overlapped(&device, OverlappedH264Pump::MAX_INFLIGHT, &packets);
    println!(
        "parity: {} frames byte-exact across sync/bound-1/bound-4, depths {} / {}",
        PICTURES, one.max_depth, four.max_depth
    );
    print_phase_table("bound-1", &one.timings);
    print_phase_table("bound-4", &four.timings);
    assert_eq!(sync.frames.len(), PICTURES, "sync yields every picture");
    assert_eq!(one.frames.len(), PICTURES, "bound-1 yields every picture");
    assert_eq!(four.frames.len(), PICTURES, "bound-4 yields every picture");
    assert_eq!(one.timings.len(), PICTURES);
    assert_eq!(four.timings.len(), PICTURES);
    for index in 0..PICTURES {
        assert_eq!(one.timings[index].seq(), index as u64, "bound-1 order");
        assert_eq!(four.timings[index].seq(), index as u64, "bound-4 order");
        assert_eq!(
            one.frames[index], sync.frames[index],
            "bound-1 frame {index} bytes"
        );
        assert_eq!(
            four.frames[index], sync.frames[index],
            "bound-4 frame {index} bytes"
        );
    }
    assert_eq!(sync.resets, 0, "clean vector never resets");
    assert_eq!(one.resets, 0);
    assert_eq!(four.resets, 0);
}

/// 1080p back-to-back sends hold two pictures in flight with overlapping windows.
#[test]
fn overlap_observes_depth_above_one() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    let packets = idr_1080p_packets();
    let sync = drive_sync(&device, &packets);
    let run = drive_overlapped(&device, OverlappedH264Pump::MAX_INFLIGHT, &packets);
    println!(
        "depth: max_depth={} over {PROBE_PICTURES} pictures",
        run.max_depth
    );
    print_phase_table("1080p bound-4", &run.timings);
    assert_eq!(sync.frames.len(), PROBE_PICTURES, "sync yields every IDR");
    assert_eq!(run.frames.len(), PROBE_PICTURES, "overlap yields every IDR");
    for index in 0..PROBE_PICTURES {
        assert_eq!(
            run.frames[index], sync.frames[index],
            "1080p frame {index} bytes"
        );
    }
    assert_eq!(run.resets, 0);
    assert!(
        run.max_depth > 1,
        "overlap needs depth above 1, got {}",
        run.max_depth
    );
    let mut overlaps = 0u32;
    let mut best_us = 0u128;
    for pair in run.timings.windows(2) {
        let (first, second) = (&pair[0], &pair[1]);
        if second.submitted_at() < first.collected_at() {
            overlaps += 1;
            best_us = best_us.max(
                first
                    .collected_at()
                    .duration_since(second.submitted_at())
                    .as_micros(),
            );
        }
    }
    println!("overlap: {overlaps} pairs overlap, best margin {best_us}us");
    assert!(overlaps > 0, "copy/engine windows never overlap");
    assert!(
        best_us > OVERLAP_FLOOR_US,
        "overlap margin {best_us}us under floor {OVERLAP_FLOOR_US}us"
    );
}

/// Bound-1 stage latency matches the sync path per frame.
#[test]
fn overlap_keeps_stage_parity() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    drive_sync(&device, &packets);
    let sync = drive_sync(&device, &packets);
    let one = drive_overlapped(&device, 1, &packets);
    let four = drive_overlapped(&device, OverlappedH264Pump::MAX_INFLIGHT, &packets);
    println!(
        "parity: sync median {}us, bound-1 median {}us, totals sync {}us bound-1 {}us bound-4 {}us",
        median(sync.walls_us.clone()),
        median(one.timings.iter().map(PhaseTiming::latency_us).collect()),
        sync.total_us,
        one.total_us,
        four.total_us
    );
    assert_eq!(sync.walls_us.len(), PICTURES);
    let sync_median = median(sync.walls_us.clone());
    let one_median = median(one.timings.iter().map(PhaseTiming::latency_us).collect());
    assert!(
        within_two_x(one_median, sync_median),
        "bound-1 median {one_median}us against sync median {sync_median}us"
    );
    assert!(
        four.total_us * 2 <= sync.total_us * 3,
        "pipelining never much slower: {}us vs {}us",
        four.total_us,
        sync.total_us
    );
}

/// Blocking receive ends the stream; idle receive times out.
#[test]
fn overlap_receive_endpoints() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    if !device.supports(Codec::H264) {
        println!("SKIP: no h264 decoder profile");
        return;
    }
    let mut pump = OverlappedH264Pump::open(&device, 1);
    assert!(pump.is_empty());
    assert!(!pump.is_finished());
    let lapsed = pump.receive_timeout(Duration::from_millis(50));
    assert!(
        matches!(lapsed, Err(ReceiveError::Timeout)),
        "idle receive lapses"
    );
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    assert!(pump.receive().expect("drained").is_none());
    assert!(pump.try_receive().expect("polled").is_none());
    let packets = h264_packets();
    let Err(TrySendError::Full { packet }) = pump.try_send(packets[0].clone()) else {
        panic!("send after finish refuses");
    };
    assert_eq!(packet.data(), packets[0].data());
}

/// Same HEVC inter bytes through sync and overlapped decode match exactly.
#[test]
fn overlap_hevc_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    if !device.supports(Codec::Hevc) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    assert_eq!(OverlappedHevcPump::open(&device, 0).bound(), 1);
    assert_eq!(
        OverlappedHevcPump::open(&device, 99).bound(),
        OverlappedHevcPump::MAX_INFLIGHT
    );
    let packets = hevc_packets();
    let mut decoder = HevcDecoder::open(&device);
    let sync = drive_sync_with(&mut decoder, &packets);
    let mut pump = OverlappedHevcPump::open(&device, OverlappedHevcPump::MAX_INFLIGHT);
    let overlapped = drive_pump_with(&mut pump, &packets);
    println!(
        "parity: {} frames byte-exact across sync/overlap, depth {}",
        PICTURES,
        pump.max_depth()
    );
    assert_eq!(sync.len(), PICTURES, "sync yields every picture");
    assert_eq!(overlapped.len(), PICTURES, "overlap yields every picture");
    assert_eq!(overlapped, sync, "overlap bytes match sync");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
    assert_eq!(pump.resets(), 0);
}

/// Same AV1 bytes through sync and overlapped decode match exactly.
#[test]
fn overlap_av1_matches_sync_bytes() {
    let _guard = Drive::serial();
    let Some(device) = device() else {
        return;
    };
    if !device.supports(Codec::Av1) {
        println!("SKIP: no av1 decoder profile");
        return;
    }
    assert_eq!(OverlappedAv1Pump::open(&device, 0).bound(), 1);
    assert_eq!(
        OverlappedAv1Pump::open(&device, 99).bound(),
        OverlappedAv1Pump::MAX_INFLIGHT
    );
    let packets = av1_packets();
    let mut decoder = Av1Decoder::open(&device);
    let sync = drive_sync_with(&mut decoder, &packets);
    let mut pump = OverlappedAv1Pump::open(&device, OverlappedAv1Pump::MAX_INFLIGHT);
    let overlapped = drive_pump_with(&mut pump, &packets);
    println!(
        "parity: {} frames byte-exact across sync/overlap, depth {}",
        PICTURES,
        pump.max_depth()
    );
    assert_eq!(sync.len(), PICTURES, "sync yields every frame");
    assert_eq!(overlapped.len(), PICTURES, "overlap yields every frame");
    assert_eq!(overlapped, sync, "overlap bytes match sync");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
    assert_eq!(pump.resets(), 0);
}
