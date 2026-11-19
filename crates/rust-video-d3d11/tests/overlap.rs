//! Multi-inflight overlap: depth, byte parity, phase timing.
//!
//! Skips without hardware; fails when the overlapped path diverges from sync.

#![cfg(target_os = "windows")]

#[path = "../examples/common/av1.rs"]
#[allow(dead_code)]
mod av1;
#[path = "../examples/common/hevc.rs"]
#[allow(dead_code)]
mod hevc;
#[path = "../examples/common/vp9.rs"]
#[allow(dead_code)]
mod vp9;

use std::num::NonZeroUsize;
use std::time::Instant;

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets, TrySendError,
};
use rust_video_caps::packet::Packet;
use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Frame, DecoderPump, H264Decoder, HevcDecoder,
    OverlappedPump, PhaseCapture, SplitDecoder, Vp9Decoder,
};
use rust_video_harness::{decode_dir, AnnexB};

/// Pictures in the committed H.264 vector.
const PICTURES: usize = 15;

/// Tight NV12 bytes of one 320x240 frame.
const FRAME_BYTES: usize = 320 * 240 * 3 / 2;

/// Serialized overlap tests: one test at a time per binary.
///
/// Parallel overlap tests share the decode engines and the CPU; contention
/// inside the timed sections reads as flake in the phase table. Every test
/// holds the lock for its whole body.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Whole-test serial guard, poison-tolerant.
fn measure() -> std::sync::MutexGuard<'static, ()> {
    MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Open device, loud skip without hardware.
fn device() -> Option<D3d11Device> {
    match D3d11Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("d3d11 skipped: {error}");
            None
        }
    }
}

/// Open device with `codec` profile, loud skip without.
fn device_for(codec: Codec) -> Option<D3d11Device> {
    let device = device()?;
    if !device.supports(codec) {
        println!("d3d11 skipped: no {codec} decoder profile");
        return None;
    }
    Some(device)
}

/// Shared H.264 vector path.
fn h264_vector() -> std::path::PathBuf {
    decode_dir().join("valid_320x240.h264")
}

/// Bound of `capacity` frames under `policy`.
fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
    ChannelConfig::new(
        NonZeroUsize::new(capacity).expect("nonzero capacity"),
        policy,
    )
}

/// One packet per H.264 picture in order.
fn h264_packets() -> Vec<Packet> {
    AnnexB::picture_packets(&h264_vector(), AnnexB::is_h264_vcl)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// One packet per HEVC picture in order.
fn hevc_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    hevc::picture_packets(&data)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// One packet per AV1 temporal unit in order.
fn av1_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    av1::tu_packets(&data)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// One packet per VP9 frame in order.
fn vp9_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("valid_320x240.ivf")).expect("vector readable");
    vp9::frame_packets(&data)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// Every packet sent; one blocking receive per refusal into `frames`.
///
/// Returns refusal count.
fn send_all<D: SplitDecoder<Frame = D3d11Frame>>(
    pump: &mut OverlappedPump<D>,
    packets: &[Packet],
    frames: &mut Vec<D3d11Frame>,
) -> u64 {
    let mut fulls = 0;
    for packet in packets {
        let mut packet = packet.clone();
        loop {
            match pump.try_send(packet) {
                Ok(()) => break,
                Err(TrySendError::Full { packet: back }) => {
                    fulls += 1;
                    packet = back;
                    let frame = pump
                        .receive()
                        .expect("in-flight frame completes")
                        .expect("input unfinished");
                    frames.push(frame);
                }
                Err(TrySendError::Failed { error }) => panic!("send failed: {error}"),
            }
        }
    }
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    fulls
}

/// Input finished, every trailing frame into `frames`.
fn drain_all<D: SplitDecoder<Frame = D3d11Frame>>(
    pump: &mut OverlappedPump<D>,
    frames: &mut Vec<D3d11Frame>,
) {
    pump.finish().expect("finish drains");
    while let Some(frame) = pump.receive().expect("drain") {
        frames.push(frame);
    }
}

/// Frames hold tight NV12 in generation order.
fn check_frames(frames: &[D3d11Frame], count: usize, tag: &str) {
    assert_eq!(frames.len(), count, "{tag} yields every frame");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "{tag} frame {index} holds tight NV12"
        );
        assert_eq!(
            frame.identity().generation(),
            index as u64,
            "{tag} frame {index} keeps order"
        );
    }
}

/// Back-to-back sends overlap on hardware.
#[test]
fn overlap_depth_above_one() {
    let _serial = measure();
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    let mut pump = OverlappedPump::open(H264Decoder::open(&device));
    let mut frames = Vec::new();
    let fulls = send_all(&mut pump, &packets, &mut frames);
    let depth = pump.max_depth();
    drain_all(&mut pump, &mut frames);
    println!(
        "overlap: max_depth={depth} fulls={fulls} frames={}",
        frames.len()
    );
    check_frames(&frames, PICTURES, "overlap");
    assert!(depth > 1, "overlap needs depth above 1, got {depth}");
    assert_eq!(pump.decoder().resets(), 0, "clean vector never resets");
}

/// In-flight bound refuses instead of growing without limit.
#[test]
fn send_bound_refuses_past_depth() {
    let _serial = measure();
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    let mut pump = OverlappedPump::open(H264Decoder::open(&device));
    let mut frames = Vec::new();
    let fulls = send_all(&mut pump, &packets, &mut frames);
    let depth = pump.max_depth();
    drain_all(&mut pump, &mut frames);
    println!(
        "bound: max_depth={depth} fulls={fulls} frames={}",
        frames.len()
    );
    check_frames(&frames, PICTURES, "bound");
    assert!(
        depth <= OverlappedPump::<H264Decoder>::DEPTH + 1,
        "bound holds depth near {}, got {depth}",
        OverlappedPump::<H264Decoder>::DEPTH
    );
    assert!(fulls > 0, "rapid sends hit the bound");
}

/// Same H.264 bytes through sync and overlapped decode.
#[test]
fn byte_parity_sync_vs_overlapped() {
    let _serial = measure();
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    let mut sync = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        sync.try_send(packet.clone()).expect("queue room");
    }
    let mut sync_frames = Vec::new();
    while let Some(frame) = sync.try_receive().expect("poll") {
        sync_frames.push(frame);
    }
    sync.finish().expect("finish drains");
    while let Some(frame) = sync.receive().expect("drained") {
        sync_frames.push(frame);
    }
    let mut overlapped = OverlappedPump::open(H264Decoder::open(&device));
    let mut overlapped_frames = Vec::new();
    send_all(&mut overlapped, &packets, &mut overlapped_frames);
    drain_all(&mut overlapped, &mut overlapped_frames);
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    for (index, (left, right)) in sync_frames.iter().zip(overlapped_frames.iter()).enumerate() {
        assert_eq!(
            left.nv12(),
            right.nv12(),
            "frame {index} bytes match across paths"
        );
    }
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same HEVC bytes through sync and overlapped decode.
#[test]
fn hevc_byte_parity_sync_vs_overlapped() {
    let _serial = measure();
    let Some(device) = device_for(Codec::Hevc) else {
        return;
    };
    let packets = hevc_packets();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    let mut sync = DecoderPump::open(
        HevcDecoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        sync.try_send(packet.clone()).expect("queue room");
    }
    let mut sync_frames = Vec::new();
    while let Some(frame) = sync.try_receive().expect("poll") {
        sync_frames.push(frame);
    }
    sync.finish().expect("finish drains");
    while let Some(frame) = sync.receive().expect("drained") {
        sync_frames.push(frame);
    }
    let mut overlapped = OverlappedPump::open(HevcDecoder::open(&device));
    let mut overlapped_frames = Vec::new();
    send_all(&mut overlapped, &packets, &mut overlapped_frames);
    drain_all(&mut overlapped, &mut overlapped_frames);
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    for (index, (left, right)) in sync_frames.iter().zip(overlapped_frames.iter()).enumerate() {
        assert_eq!(
            left.nv12(),
            right.nv12(),
            "frame {index} bytes match across paths"
        );
    }
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same AV1 bytes through sync and overlapped decode.
#[test]
fn av1_byte_parity_sync_vs_overlapped() {
    let _serial = measure();
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let packets = av1_packets();
    assert_eq!(packets.len(), 16, "vector holds one packet per unit");
    let mut sync = DecoderPump::open(
        Av1Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        sync.try_send(packet.clone()).expect("queue room");
    }
    let mut sync_frames = Vec::new();
    while let Some(frame) = sync.try_receive().expect("poll") {
        sync_frames.push(frame);
    }
    sync.finish().expect("finish drains");
    while let Some(frame) = sync.receive().expect("drained") {
        sync_frames.push(frame);
    }
    let mut overlapped = OverlappedPump::open(Av1Decoder::open(&device));
    let mut overlapped_frames = Vec::new();
    send_all(&mut overlapped, &packets, &mut overlapped_frames);
    drain_all(&mut overlapped, &mut overlapped_frames);
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    for (index, (left, right)) in sync_frames.iter().zip(overlapped_frames.iter()).enumerate() {
        assert_eq!(
            left.nv12(),
            right.nv12(),
            "frame {index} bytes match across paths"
        );
    }
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same VP9 bytes through sync and overlapped decode.
#[test]
fn vp9_byte_parity_sync_vs_overlapped() {
    let _serial = measure();
    let Some(device) = device_for(Codec::Vp9) else {
        return;
    };
    let packets = vp9_packets();
    assert_eq!(packets.len(), PICTURES, "vector holds one packet per frame");
    let mut sync = DecoderPump::open(
        Vp9Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        sync.try_send(packet.clone()).expect("queue room");
    }
    let mut sync_frames = Vec::new();
    while let Some(frame) = sync.try_receive().expect("poll") {
        sync_frames.push(frame);
    }
    sync.finish().expect("finish drains");
    while let Some(frame) = sync.receive().expect("drained") {
        sync_frames.push(frame);
    }
    let mut overlapped = OverlappedPump::open(Vp9Decoder::open(&device));
    let mut overlapped_frames = Vec::new();
    send_all(&mut overlapped, &packets, &mut overlapped_frames);
    drain_all(&mut overlapped, &mut overlapped_frames);
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    for (index, (left, right)) in sync_frames.iter().zip(overlapped_frames.iter()).enumerate() {
        assert_eq!(
            left.nv12(),
            right.nv12(),
            "frame {index} bytes match across paths"
        );
    }
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Phase-split latency parity plus observed submit/completion overlap.
#[test]
fn phase_timing_parity_and_overlap() {
    use rust_video_caps::traits::Decoder;

    let _serial = measure();
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    let mut sync_decoder = H264Decoder::open(&device);
    sync_decoder.capture_phases(true);
    let sync_start = Instant::now();
    let mut sync_count = 0;
    for packet in &packets {
        sync_count += sync_decoder.decode(packet).expect("sync decode").len();
    }
    sync_count += sync_decoder.flush().expect("sync flush").len();
    let sync_wall = sync_start.elapsed();
    assert_eq!(sync_count, PICTURES, "sync yields every frame");
    let sync_phases = sync_decoder.phases().to_vec();

    let mut overlapped = OverlappedPump::open(H264Decoder::open(&device));
    overlapped.capture_phases(true);
    let overlapped_start = Instant::now();
    let mut frames = Vec::new();
    let fulls = send_all(&mut overlapped, &packets, &mut frames);
    drain_all(&mut overlapped, &mut frames);
    let overlapped_wall = overlapped_start.elapsed();
    check_frames(&frames, PICTURES, "overlapped");
    let overlapped_phases = overlapped.phases().to_vec();

    assert_eq!(sync_phases.len(), PICTURES, "sync phases cover every frame");
    assert_eq!(
        overlapped_phases.len(),
        PICTURES,
        "overlapped phases cover every frame"
    );
    println!("phase table (us): gen | sync submit/wait/read/lat | ovlp submit/wait/read/lat");
    let mut sync_lat = 0u128;
    let mut overlapped_lat = 0u128;
    for (sync_phase, overlapped_phase) in sync_phases.iter().zip(overlapped_phases.iter()) {
        assert_eq!(
            sync_phase.generation(),
            overlapped_phase.generation(),
            "phases join on generation"
        );
        sync_lat += sync_phase.latency().as_micros();
        overlapped_lat += overlapped_phase.latency().as_micros();
        println!(
            "{:>3} | {:>5}/{:>5}/{:>5}/{:>5} | {:>5}/{:>5}/{:>5}/{:>5}",
            sync_phase.generation(),
            sync_phase.submit().as_micros(),
            sync_phase.engine_wait().as_micros(),
            sync_phase.readback().as_micros(),
            sync_phase.latency().as_micros(),
            overlapped_phase.submit().as_micros(),
            overlapped_phase.engine_wait().as_micros(),
            overlapped_phase.readback().as_micros(),
            overlapped_phase.latency().as_micros(),
        );
    }
    let mut events = 0;
    for pair in overlapped_phases.windows(2) {
        if pair[1].submit_start() < pair[0].signaled_at() {
            events += 1;
        }
    }
    let sync_mean = sync_lat / PICTURES as u128;
    let overlapped_mean = overlapped_lat / PICTURES as u128;
    println!(
        "phases: sync mean {sync_mean}us, overlapped mean {overlapped_mean}us, \
         overlap events {events}, fulls {fulls}, walls {}us vs {}us",
        sync_wall.as_micros(),
        overlapped_wall.as_micros(),
    );
    assert!(events > 0, "overlap submits before prior completion");
    assert!(
        overlapped_mean <= sync_mean * 2,
        "latency parity: overlapped mean {overlapped_mean}us vs sync mean {sync_mean}us"
    );
    assert!(
        overlapped_wall.as_micros() <= sync_wall.as_micros() * 2,
        "wall parity: overlapped {}us vs sync {}us",
        overlapped_wall.as_micros(),
        sync_wall.as_micros(),
    );
}
