//! Uniform corpus legs over the shared harness bytes: decode, parity, sustained.
//!
//! Lab-gated: needs the ffmpeg system libraries behind the `ffmpeg` feature.
//! Bars are provisional until the lab confirms quiet medians. VP8 has no
//! inter-picture vector, so its sustained leg stays out.

#![cfg(feature = "ffmpeg")]

use std::time::{Duration, Instant};

use rust_video_caps::delivery::{DropPolicy, PollFrames, ReceiveFrames};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, decode_dir, AnnexB, Drive, Ivf, Packets, Parity};
use rust_video_sw::{
    Av1Decoder, H264Decoder, H265Decoder, PipelineConfig, PipelinedDecoder, PipelinedPump,
    RawSource, SplitDecoder, SwError, Vp8Decoder, Vp9Decoder,
};

/// Pictures in each shared vector.
const PICTURES: usize = 15;

/// Provisional sustained bars: tier realtime floor, lab confirms.
const SUSTAINED_H264_BAR: f64 = 60.0;
const SUSTAINED_H265_BAR: f64 = 60.0;
const SUSTAINED_AV1_BAR: f64 = 60.0;
const SUSTAINED_VP9_BAR: f64 = 60.0;

/// Ready frames moved into `frames`, waiting for one when empty.
fn drain<D>(pump: &mut PipelinedPump<D>, frames: &mut Vec<CpuFrame>)
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
fn pipelined<D>(
    inner: D,
    pace: Duration,
    packets: &[Packet],
) -> (Vec<CpuFrame>, rust_video_sw::PhaseStats)
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
    let stats = pump.stats();
    (frames, stats)
}

/// Sync and pipelined runs agree frame for frame over `packets`.
fn parity_case<D>(open: impl Fn() -> D, packets: &[Packet], codec: &str)
where
    D: Decoder<Frame = CpuFrame> + RawSource<Frame = CpuFrame, Error = SwError>,
    <D as Decoder>::Error: std::fmt::Debug,
{
    let expected = Drive::decode_all(&mut open(), packets);
    assert!(!expected.is_empty(), "{codec} vector decodes");
    let (frames, stats) = pipelined(open(), Duration::ZERO, packets);
    Parity::assert_cpu_frames(codec, &frames, &expected);
    println!(
        "sw {codec} uniform: frames={} max_inflight={} overlap={}",
        frames.len(),
        stats.max_inflight(),
        stats.overlap_observed()
    );
}

/// Pipelined fps of one pass of `packets` against `bar`.
fn sustained_case<D>(open: impl Fn() -> D, packets: &[Packet], codec: &str, bar: f64)
where
    D: Decoder<Frame = CpuFrame> + RawSource<Frame = CpuFrame, Error = SwError>,
    <D as Decoder>::Error: std::fmt::Debug,
{
    let start = Instant::now();
    let (frames, _) = pipelined(open(), Duration::ZERO, packets);
    let wall = start.elapsed().as_secs_f64();
    let total = u32::try_from(frames.len()).expect("frame count fits");
    let measured = f64::from(total) / wall;
    println!(
        "sw {codec} sustained overlap 1080p: {measured:.1}fps over {} frames, bar {bar:.0}fps",
        frames.len()
    );
    assert_eq!(
        frames.len(),
        packets.len(),
        "every picture yields its frame"
    );
    assert!(
        measured >= bar,
        "{codec}: below sustained bar: {measured:.1}fps < {bar:.0}fps"
    );
}

/// H.264 probe, skipping loud when the backend declines.
fn h264_or_skip() -> Option<impl Fn() -> H264Decoder> {
    match H264Decoder::open(4) {
        Ok(_) => Some(|| H264Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw h264 uniform skipped: {error}");
            None
        }
    }
}

/// H.265 probe, skipping loud when the backend declines.
fn h265_or_skip() -> Option<impl Fn() -> H265Decoder> {
    match H265Decoder::open(4) {
        Ok(_) => Some(|| H265Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw h265 uniform skipped: {error}");
            None
        }
    }
}

/// VP8 probe, skipping loud when the backend declines.
fn vp8_or_skip() -> Option<impl Fn() -> Vp8Decoder> {
    match Vp8Decoder::open(4) {
        Ok(_) => Some(|| Vp8Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw vp8 uniform skipped: {error}");
            None
        }
    }
}

/// VP9 probe, skipping loud when the backend declines.
fn vp9_or_skip() -> Option<impl Fn() -> Vp9Decoder> {
    match Vp9Decoder::open(4) {
        Ok(_) => Some(|| Vp9Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw vp9 uniform skipped: {error}");
            None
        }
    }
}

/// AV1 probe, skipping loud when the backend declines.
fn av1_or_skip() -> Option<impl Fn() -> Av1Decoder> {
    match Av1Decoder::open(4) {
        Ok(_) => Some(|| Av1Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw av1 uniform skipped: {error}");
            None
        }
    }
}

/// Shared H.264 pictures decode to every frame; lab-gated.
#[test]
fn h264_shared_decode_yields_every_frame() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h264 yields every frame");
}

/// Shared H.264 B-frame pictures decode to every frame; lab-gated.
#[test]
fn h264_bframes_decode_yields_every_frame() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("bframes_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h264 yields every frame");
}

/// Shared H.265 pictures decode to every frame; lab-gated.
#[test]
fn h265_shared_decode_yields_every_frame() {
    let Some(open) = h265_or_skip() else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "h265 yields every frame");
}

/// Shared AV1 frames decode to every frame; lab-gated.
#[test]
fn av1_shared_decode_yields_every_frame() {
    let Some(open) = av1_or_skip() else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&[bytes]));
    assert_eq!(frames.len(), PICTURES, "av1 yields every frame");
}

/// Shared VP9 frames decode to every frame; lab-gated.
#[test]
fn vp9_shared_decode_yields_every_frame() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&raws));
    assert_eq!(frames.len(), PICTURES, "vp9 yields every frame");
}

/// Shared VP8 frame decodes to its frame; lab-gated.
#[test]
fn vp8_shared_decode_yields_every_frame() {
    let Some(open) = vp8_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let frames = Drive::decode_all(&mut open(), &Packets::from_raws(&raws));
    assert_eq!(frames.len(), raws.len(), "vp8 yields every frame");
}

/// Pipelined H.264 matches sync bytes on the shared vector; lab-gated.
#[test]
fn h264_shared_pipelined_matches_sync_bytes() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    parity_case(open, &Packets::from_raws(&raws), "h264");
}

/// Pipelined H.265 matches sync bytes on the shared vector; lab-gated.
#[test]
fn h265_shared_pipelined_matches_sync_bytes() {
    let Some(open) = h265_or_skip() else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    parity_case(open, &Packets::from_raws(&raws), "h265");
}

/// Pipelined AV1 matches sync bytes on the shared vector; lab-gated.
#[test]
fn av1_shared_pipelined_matches_sync_bytes() {
    let Some(open) = av1_or_skip() else {
        return;
    };
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    parity_case(open, &Packets::from_raws(&[bytes]), "av1");
}

/// Pipelined VP9 matches sync bytes on the shared vector; lab-gated.
#[test]
fn vp9_shared_pipelined_matches_sync_bytes() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    parity_case(open, &Packets::from_raws(&raws), "vp9");
}

/// Pipelined VP8 matches sync bytes on the shared vector; lab-gated.
#[test]
fn vp8_shared_pipelined_matches_sync_bytes() {
    let Some(open) = vp8_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    parity_case(open, &Packets::from_raws(&raws), "vp8");
}

/// Sustained H.264 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h264-1080p_ip.h264"), AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    sustained_case(open, &Packets::from_raws(&raws), "h264", SUSTAINED_H264_BAR);
}

/// Sustained H.265 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h265_1080p_measures_realtime() {
    let Some(open) = h265_or_skip() else {
        return;
    };
    let bytes = std::fs::read(bench_dir().join("h265-1080p_ip.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    sustained_case(open, &Packets::from_raws(&raws), "h265", SUSTAINED_H265_BAR);
}

/// Sustained AV1 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_av1_1080p_measures_realtime() {
    let Some(open) = av1_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    sustained_case(open, &Packets::from_raws(&raws), "av1", SUSTAINED_AV1_BAR);
}

/// Sustained VP9 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_vp9_1080p_measures_realtime() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    sustained_case(open, &Packets::from_raws(&raws), "vp9", SUSTAINED_VP9_BAR);
}
