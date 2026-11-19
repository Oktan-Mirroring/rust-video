//! Uniform corpus legs over the shared harness bytes: decode, parity, sustained.
//!
//! Lab-gated: needs Linux plus VA-API hardware; every test skips
//! loud without them. Bars are provisional until the lab confirms
//! quiet medians.

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, decode_dir, AnnexB, Ivf, Packets, Parity};
use rust_video_vaapi::{Av1Decoder, H264Decoder, H265Decoder, VaApiDevice, VaApiFrame, Vp9Decoder};

/// Pictures in each shared vector.
const PICTURES: usize = 15;

/// Provisional sustained bars: tier realtime floor, lab confirms.
const SUSTAINED_H264_BAR: f64 = 60.0;
const SUSTAINED_H265_BAR: f64 = 60.0;
const SUSTAINED_AV1_BAR: f64 = 60.0;
const SUSTAINED_VP9_BAR: f64 = 60.0;

/// Open device, loud skip without hardware.
fn device(name: &str) -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("SKIP vaapi {name} uniform: {error}");
            None
        }
    }
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

/// Pipeline depth behind one decoder.
trait Pipelined: Decoder<Frame = VaApiFrame> {
    /// Submitted-ahead ceiling, clamped to 1..=4.
    fn set_depth(&mut self, depth: usize);
}

impl Pipelined for H264Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }
}

impl Pipelined for H265Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }
}

impl Pipelined for Av1Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }
}

impl Pipelined for Vp9Decoder<'_> {
    fn set_depth(&mut self, depth: usize) {
        self.set_pipeline_depth(depth);
    }
}

/// Frames for one pass of `packets`, plus wall time.
fn run(decoder: &mut impl Pipelined, packets: &[Packet]) -> (Vec<VaApiFrame>, Duration) {
    let start = Instant::now();
    let mut frames = Vec::new();
    for packet in packets {
        frames.extend(decoder.decode(packet).expect("uniform decode"));
    }
    frames.extend(decoder.flush().expect("uniform flush"));
    (frames, start.elapsed())
}

/// Byte-exact frames across both legs.
fn assert_parity(name: &str, sync: &[VaApiFrame], pipelined: &[VaApiFrame]) {
    assert!(!sync.is_empty(), "{name}: legs decoded no frames");
    Parity::assert_bytes(name, sync, pipelined, VaApiFrame::nv12);
    Parity::assert_generations(name, sync, pipelined, |frame| frame.identity().generation());
    println!(
        "vaapi {name} uniform parity: {} frames byte-exact",
        sync.len()
    );
}

/// Depth-one and pipelined bytes match exactly over `packets`.
fn parity_case<D>(mut sync: D, mut pipelined: D, packets: &[Packet], name: &str)
where
    D: Pipelined,
{
    sync.set_depth(1);
    let (sync_frames, _) = run(&mut sync, packets);
    pipelined.set_depth(depth());
    let (pipelined_frames, _) = run(&mut pipelined, packets);
    assert_parity(name, &sync_frames, &pipelined_frames);
}

/// Measured fps of one pipelined pass of `packets` through `decoder`.
fn sustained_fps(decoder: &mut impl Pipelined, packets: &[Packet]) -> (f64, usize) {
    decoder.set_depth(depth());
    let (frames, wall) = run(decoder, packets);
    let count = u32::try_from(frames.len()).expect("frame count fits");
    assert!(count > 0, "sustained run decoded no frames");
    (f64::from(count) / wall.as_secs_f64(), frames.len())
}

/// Shared H.264 pictures decode to every frame; lab-gated.
#[test]
fn h264_shared_decode_yields_every_frame() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let (frames, _) = run(&mut decoder, &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h264 yields every frame");
}

/// Shared H.264 B-frame pictures decode to every frame; lab-gated.
#[test]
fn h264_bframes_decode_yields_every_frame() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("bframes_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let (frames, _) = run(&mut decoder, &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h264 yields every frame");
}

/// Shared H.265 pictures decode to every frame; lab-gated.
#[test]
fn h265_shared_decode_yields_every_frame() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("inter_320x240.hevc"),
        AnnexB::is_hevc_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H265Decoder::open(&device);
    let (frames, _) = run(&mut decoder, &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h265 yields every frame");
}

/// Shared AV1 frames decode to every frame; lab-gated.
#[test]
fn av1_shared_decode_yields_every_frame() {
    let Some(device) = device("av1") else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let mut decoder = Av1Decoder::open(&device);
    let (frames, _) = run(&mut decoder, &Packets::from_raws(&[bytes]));
    assert_eq!(frames.len(), PICTURES, "av1 yields every frame");
}

/// Shared VP9 frames decode to every frame; lab-gated.
#[test]
fn vp9_shared_decode_yields_every_frame() {
    let Some(device) = device("vp9") else {
        return;
    };
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    let mut decoder = Vp9Decoder::open(&device);
    let (frames, _) = run(&mut decoder, &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "vp9 yields every frame");
}

/// Shared H.264 bytes match across depth one and pipelined; lab-gated.
#[test]
fn h264_shared_pipeline_matches_sync_bytes() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    parity_case(
        H264Decoder::open(&device),
        H264Decoder::open(&device),
        &Packets::from_raws(&raws),
        "h264",
    );
}

/// Shared H.265 bytes match across depth one and pipelined; lab-gated.
#[test]
fn h265_shared_pipeline_matches_sync_bytes() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("inter_320x240.hevc"),
        AnnexB::is_hevc_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    parity_case(
        H265Decoder::open(&device),
        H265Decoder::open(&device),
        &Packets::from_raws(&raws),
        "h265",
    );
}

/// Shared AV1 bytes match across depth one and pipelined; lab-gated.
#[test]
fn av1_shared_pipeline_matches_sync_bytes() {
    let Some(device) = device("av1") else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    parity_case(
        Av1Decoder::open(&device),
        Av1Decoder::open(&device),
        &Packets::from_raws(&[bytes]),
        "av1",
    );
}

/// Shared VP9 bytes match across depth one and pipelined; lab-gated.
#[test]
fn vp9_shared_pipeline_matches_sync_bytes() {
    let Some(device) = device("vp9") else {
        return;
    };
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    parity_case(
        Vp9Decoder::open(&device),
        Vp9Decoder::open(&device),
        &Packets::from_raws(&raws),
        "vp9",
    );
}

/// Sustained H.264 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    let Some(device) = device("h264") else {
        return;
    };
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h264-1080p_ip.h264"), AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut decoder = H264Decoder::open(&device);
    let (measured, count) = sustained_fps(&mut decoder, &Packets::from_raws(&raws));
    println!(
        "vaapi h264 sustained overlap 1080p: {measured:.1}fps over {count} frames, bar {SUSTAINED_H264_BAR:.0}fps"
    );
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_H264_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_H264_BAR:.0}fps"
    );
}

/// Sustained H.265 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h265_1080p_measures_realtime() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h265-1080p_ip.h265"), AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut decoder = H265Decoder::open(&device);
    let (measured, count) = sustained_fps(&mut decoder, &Packets::from_raws(&raws));
    println!(
        "vaapi h265 sustained overlap 1080p: {measured:.1}fps over {count} frames, bar {SUSTAINED_H265_BAR:.0}fps"
    );
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_H265_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_H265_BAR:.0}fps"
    );
}

/// Sustained AV1 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_av1_1080p_measures_realtime() {
    let Some(device) = device("av1") else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let mut decoder = Av1Decoder::open(&device);
    let (measured, count) = sustained_fps(&mut decoder, &Packets::from_raws(&raws));
    println!(
        "vaapi av1 sustained overlap 1080p: {measured:.1}fps over {count} frames, bar {SUSTAINED_AV1_BAR:.0}fps"
    );
    assert_eq!(count, raws.len(), "every frame yields its frame");
    assert!(
        measured >= SUSTAINED_AV1_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_AV1_BAR:.0}fps"
    );
}

/// Sustained VP9 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_vp9_1080p_measures_realtime() {
    let Some(device) = device("vp9") else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
    let (measured, count) = sustained_fps(&mut decoder, &Packets::from_raws(&raws));
    println!(
        "vaapi vp9 sustained overlap 1080p: {measured:.1}fps over {count} frames, bar {SUSTAINED_VP9_BAR:.0}fps"
    );
    assert_eq!(count, raws.len(), "every frame yields its frame");
    assert!(
        measured >= SUSTAINED_VP9_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_VP9_BAR:.0}fps"
    );
}
