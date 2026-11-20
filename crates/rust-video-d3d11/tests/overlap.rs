//! Multi-inflight overlap: depth, byte parity, phase timing.
//!
//! Skips without hardware; fails when the overlapped path diverges from sync.

#![cfg(target_os = "windows")]

#[path = "../examples/common/vp9.rs"]
#[allow(dead_code)]
mod vp9;

use std::time::Instant;

use rust_video_caps::delivery::{DecoderPump, DropPolicy, ReceiveFrames};
use rust_video_caps::packet::Packet;
use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Frame, H264Decoder, HevcDecoder, OverlappedPump,
    Vp9Decoder,
};
use rust_video_harness::{decode_dir, AnnexB, Drive, Obu, Packets, Parity};

/// Pictures in the committed H.264 vector.
const PICTURES: usize = 15;

/// Tight NV12 bytes of one 320x240 frame.
const FRAME_BYTES: usize = 320 * 240 * 3 / 2;

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

/// One packet per H.264 picture in order.
fn h264_packets() -> Vec<Packet> {
    Packets::from_raws(&AnnexB::picture_packets(
        &h264_vector(),
        AnnexB::is_h264_vcl,
    ))
}

/// One packet per HEVC picture in order.
fn hevc_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    Packets::from_raws(&AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl))
}

/// One packet per AV1 temporal unit in order.
fn av1_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    Packets::from_raws(&Obu::tu_packets(&data))
}

/// One packet per VP9 frame in order.
fn vp9_packets() -> Vec<Packet> {
    let data = std::fs::read(decode_dir().join("valid_320x240.ivf")).expect("vector readable");
    Packets::from_raws(&vp9::frame_packets(&data))
}

/// One in-flight frame into `frames`, freeing send room.
fn drain<P>(pump: &mut P, frames: &mut Vec<P::Frame>)
where
    P: ReceiveFrames,
    P::Error: std::fmt::Debug,
{
    frames.push(
        pump.receive()
            .expect("in-flight frame completes")
            .expect("input unfinished"),
    );
}

/// Frames hold tight NV12 in generation order.
fn check_frames(frames: &[D3d11Frame], count: usize, tag: &str) {
    Parity::assert_ordered(tag, frames, count, FRAME_BYTES, D3d11Frame::nv12, |frame| {
        frame.identity().generation()
    });
}

/// Back-to-back sends overlap on hardware.
#[test]
fn overlap_depth_above_one() {
    let _serial = Drive::serial();
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
    let (mut frames, fulls) = Drive::send_all(&mut pump, &packets, drain);
    let depth = pump.max_depth();
    frames.extend(Drive::finish_all(&mut pump));
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
    let _serial = Drive::serial();
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
    let (mut frames, fulls) = Drive::send_all(&mut pump, &packets, drain);
    let depth = pump.max_depth();
    frames.extend(Drive::finish_all(&mut pump));
    println!(
        "bound: max_depth={depth} fulls={fulls} frames={}",
        frames.len()
    );
    check_frames(&frames, PICTURES, "bound");
    assert!(
        depth <= OverlappedPump::<H264Decoder>::MAX_INFLIGHT + 1,
        "bound holds depth near {}, got {depth}",
        OverlappedPump::<H264Decoder>::MAX_INFLIGHT
    );
    assert!(fulls > 0, "rapid sends hit the bound");
}

/// Same H.264 bytes through sync and overlapped decode.
#[test]
fn byte_parity_sync_vs_overlapped() {
    let _serial = Drive::serial();
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
        Drive::channel(32, DropPolicy::DropOldest),
    );
    let (mut sync_frames, _) = Drive::send_all(&mut sync, &packets, drain);
    sync_frames.extend(Drive::finish_all(&mut sync));
    let mut overlapped = OverlappedPump::open(H264Decoder::open(&device));
    let (mut overlapped_frames, _) = Drive::send_all(&mut overlapped, &packets, drain);
    overlapped_frames.extend(Drive::finish_all(&mut overlapped));
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    Parity::assert_bytes("parity", &sync_frames, &overlapped_frames, D3d11Frame::nv12);
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same HEVC bytes through sync and overlapped decode.
#[test]
fn hevc_byte_parity_sync_vs_overlapped() {
    let _serial = Drive::serial();
    let Some(device) = device_for(Codec::H265) else {
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
        Drive::channel(32, DropPolicy::DropOldest),
    );
    let (mut sync_frames, _) = Drive::send_all(&mut sync, &packets, drain);
    sync_frames.extend(Drive::finish_all(&mut sync));
    let mut overlapped = OverlappedPump::open(HevcDecoder::open(&device));
    let (mut overlapped_frames, _) = Drive::send_all(&mut overlapped, &packets, drain);
    overlapped_frames.extend(Drive::finish_all(&mut overlapped));
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    Parity::assert_bytes("parity", &sync_frames, &overlapped_frames, D3d11Frame::nv12);
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same AV1 bytes through sync and overlapped decode.
#[test]
fn av1_byte_parity_sync_vs_overlapped() {
    let _serial = Drive::serial();
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let packets = av1_packets();
    assert_eq!(packets.len(), 16, "vector holds one packet per unit");
    let mut sync = DecoderPump::open(
        Av1Decoder::open(&device),
        Drive::channel(32, DropPolicy::DropOldest),
    );
    let (mut sync_frames, _) = Drive::send_all(&mut sync, &packets, drain);
    sync_frames.extend(Drive::finish_all(&mut sync));
    let mut overlapped = OverlappedPump::open(Av1Decoder::open(&device));
    let (mut overlapped_frames, _) = Drive::send_all(&mut overlapped, &packets, drain);
    overlapped_frames.extend(Drive::finish_all(&mut overlapped));
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    Parity::assert_bytes("parity", &sync_frames, &overlapped_frames, D3d11Frame::nv12);
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Same VP9 bytes through sync and overlapped decode.
#[test]
fn vp9_byte_parity_sync_vs_overlapped() {
    let _serial = Drive::serial();
    let Some(device) = device_for(Codec::Vp9) else {
        return;
    };
    let packets = vp9_packets();
    assert_eq!(packets.len(), PICTURES, "vector holds one packet per frame");
    let mut sync = DecoderPump::open(
        Vp9Decoder::open(&device),
        Drive::channel(32, DropPolicy::DropOldest),
    );
    let (mut sync_frames, _) = Drive::send_all(&mut sync, &packets, drain);
    sync_frames.extend(Drive::finish_all(&mut sync));
    let mut overlapped = OverlappedPump::open(Vp9Decoder::open(&device));
    let (mut overlapped_frames, _) = Drive::send_all(&mut overlapped, &packets, drain);
    overlapped_frames.extend(Drive::finish_all(&mut overlapped));
    check_frames(&sync_frames, PICTURES, "sync");
    check_frames(&overlapped_frames, PICTURES, "overlapped");
    Parity::assert_bytes("parity", &sync_frames, &overlapped_frames, D3d11Frame::nv12);
    println!("parity: {PICTURES} frames, {FRAME_BYTES} bytes each, 0 mismatches");
}

/// Phase-split latency parity plus observed submit/completion overlap.
#[test]
fn phase_timing_parity_and_overlap() {
    use rust_video_caps::traits::Decoder;

    let _serial = Drive::serial();
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
    let sync_start = Instant::now();
    let mut sync_count = 0;
    for packet in &packets {
        sync_count += sync_decoder.decode(packet).expect("sync decode").len();
    }
    sync_count += sync_decoder.flush().expect("sync flush").len();
    let sync_wall = sync_start.elapsed();
    assert_eq!(sync_count, PICTURES, "sync yields every frame");
    let sync_timings = sync_decoder.timings().to_vec();

    let mut overlapped = OverlappedPump::open(H264Decoder::open(&device));
    let overlapped_start = Instant::now();
    let (mut frames, fulls) = Drive::send_all(&mut overlapped, &packets, drain);
    frames.extend(Drive::finish_all(&mut overlapped));
    let overlapped_wall = overlapped_start.elapsed();
    check_frames(&frames, PICTURES, "overlapped");
    let overlapped_timings = overlapped.timings().to_vec();

    assert_eq!(
        sync_timings.len(),
        PICTURES,
        "sync timings cover every frame"
    );
    assert_eq!(
        overlapped_timings.len(),
        PICTURES,
        "overlapped timings cover every frame"
    );
    println!("phase table (us): seq | sync submit/wait/copy/lat | ovlp submit/wait/copy/lat");
    let mut sync_lat = 0u64;
    let mut overlapped_lat = 0u64;
    for (sync, ovlp) in sync_timings.iter().zip(overlapped_timings.iter()) {
        assert_eq!(sync.seq(), ovlp.seq(), "timings join on seq");
        sync_lat += sync.latency_us();
        overlapped_lat += ovlp.latency_us();
        println!(
            "{:>3} | {:>5}/{:>5}/{:>5}/{:>5} | {:>5}/{:>5}/{:>5}/{:>5}",
            sync.seq(),
            sync.submit_us(),
            sync.engine_wait_us(),
            sync.copy_us(),
            sync.latency_us(),
            ovlp.submit_us(),
            ovlp.engine_wait_us(),
            ovlp.copy_us(),
            ovlp.latency_us(),
        );
    }
    let mut events = 0;
    for pair in overlapped_timings.windows(2) {
        let prev_ready = pair[0].submitted_at() + pair[0].submit() + pair[0].engine_wait();
        if pair[1].submitted_at() < prev_ready {
            events += 1;
        }
    }
    let sync_mean = sync_lat / PICTURES as u64;
    let overlapped_mean = overlapped_lat / PICTURES as u64;
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
