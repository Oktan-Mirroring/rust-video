//! Async delivery: loopback frames plus trailing receive per codec.
//!
//! Skips without a backend; fails when the contract breaks.

#![cfg(feature = "ffmpeg")]

use std::fs;

use rust_video_caps::delivery::{DropPolicy, PollFrames, ReceiveFrames, SendPackets};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth, Ivf, Packets, Parity};
use rust_video_sw::{Av1Decoder, DecoderPump, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder};

/// Frames polled from `pump` until the queue stands empty.
fn polled<D>(pump: &mut DecoderPump<D>) -> Vec<CpuFrame>
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut frames = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    frames
}

/// Frames blocking-drained from `pump` through end of stream.
fn drained<D>(pump: &mut DecoderPump<D>) -> Vec<CpuFrame>
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut frames = Vec::new();
    while let Some(frame) = pump.receive().expect("drain") {
        frames.push(frame);
    }
    frames
}

/// Sent packets return as owned frames, then end of stream.
fn check_loopback<D>(open: impl Fn() -> D, packets: &[Packet], codec: &str)
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let expected = Drive::decode_all(&mut open(), packets);
    assert!(!expected.is_empty(), "{codec} vector decodes");
    let mut pump = DecoderPump::open(
        open(),
        Drive::channel(expected.len(), DropPolicy::DropOldest),
    );
    for packet in packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = polled(&mut pump);
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    frames.extend(drained(&mut pump));
    Parity::assert_cpu_frames(codec, &frames, &expected);
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert!(pump.try_receive().expect("drained").is_none());
    assert!(pump.receive().expect("drained").is_none());
}

/// Blocking receive returns the trailing frames, then end of stream.
fn check_trailing<D>(open: impl Fn() -> D, packets: &[Packet], codec: &str)
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let expected = Drive::decode_all(&mut open(), packets);
    assert!(!expected.is_empty(), "{codec} vector decodes");
    let mut pump = DecoderPump::open(
        open(),
        Drive::channel(expected.len(), DropPolicy::DropOldest),
    );
    for packet in packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    pump.finish().expect("finish drains");
    let frames = drained(&mut pump);
    Parity::assert_cpu_frames(codec, &frames, &expected);
    let trailing = frames.last().expect("trailing frame");
    let want = expected.last().expect("oracle tail");
    Parity::assert_cpu_frames(
        codec,
        std::slice::from_ref(trailing),
        std::slice::from_ref(want),
    );
    assert!(pump.receive().expect("drained").is_none());
}

/// H.264 probe, skipping loud when the backend declines.
fn h264_or_skip() -> Option<impl Fn() -> H264Decoder> {
    match H264Decoder::open(4) {
        Ok(_) => Some(|| H264Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw h264 delivery skipped: {error}");
            None
        }
    }
}

/// HEVC probe, skipping loud when the backend declines.
fn h265_or_skip() -> Option<impl Fn() -> H265Decoder> {
    match H265Decoder::open(4) {
        Ok(_) => Some(|| H265Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw h265 delivery skipped: {error}");
            None
        }
    }
}

/// VP8 probe, skipping loud when the backend declines.
fn vp8_or_skip() -> Option<impl Fn() -> Vp8Decoder> {
    match Vp8Decoder::open(4) {
        Ok(_) => Some(|| Vp8Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw vp8 delivery skipped: {error}");
            None
        }
    }
}

/// VP9 probe, skipping loud when the backend declines.
fn vp9_or_skip() -> Option<impl Fn() -> Vp9Decoder> {
    match Vp9Decoder::open(4) {
        Ok(_) => Some(|| Vp9Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw vp9 delivery skipped: {error}");
            None
        }
    }
}

/// AV1 probe, skipping loud when the backend declines.
fn av1_or_skip() -> Option<impl Fn() -> Av1Decoder> {
    match Av1Decoder::open(4) {
        Ok(_) => Some(|| Av1Decoder::open(4).expect("decode opens")),
        Err(error) => {
            println!("sw av1 delivery skipped: {error}");
            None
        }
    }
}

/// H.264 loopback: synth packets return as owned frames.
#[test]
fn h264_send_poll_loopback_returns_own_frames() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    check_loopback(open, &Packets::from_raws(&raws), "h264");
}

/// H.264 trailing frames arrive over blocking receive.
#[test]
fn h264_blocking_receive_gets_trailing_frame() {
    let Some(open) = h264_or_skip() else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    check_trailing(open, &Packets::from_raws(&raws), "h264");
}

/// HEVC loopback: bench pictures return as owned frames.
#[test]
fn h265_send_poll_loopback_returns_own_frames() {
    let Some(open) = h265_or_skip() else {
        return;
    };
    let bytes = fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("h265 vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    check_loopback(open, &Packets::from_raws(&raws), "h265");
}

/// HEVC trailing frames arrive over blocking receive.
#[test]
fn h265_blocking_receive_gets_trailing_frame() {
    let Some(open) = h265_or_skip() else {
        return;
    };
    let bytes = fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("h265 vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    check_trailing(open, &Packets::from_raws(&raws), "h265");
}

/// VP8 loopback: bench pictures return as owned frames.
#[test]
fn vp8_send_poll_loopback_returns_own_frames() {
    let Some(open) = vp8_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    check_loopback(open, &Packets::from_raws(&raws), "vp8");
}

/// VP8 trailing frames arrive over blocking receive.
#[test]
fn vp8_blocking_receive_gets_trailing_frame() {
    let Some(open) = vp8_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    check_trailing(open, &Packets::from_raws(&raws), "vp8");
}

/// VP9 loopback: bench pictures return as owned frames.
#[test]
fn vp9_send_poll_loopback_returns_own_frames() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    check_loopback(open, &Packets::from_raws(&raws), "vp9");
}

/// VP9 trailing frames arrive over blocking receive.
#[test]
fn vp9_blocking_receive_gets_trailing_frame() {
    let Some(open) = vp9_or_skip() else {
        return;
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    check_trailing(open, &Packets::from_raws(&raws), "vp9");
}

/// AV1 loopback: bench pictures return as owned frames.
#[test]
fn av1_send_poll_loopback_returns_own_frames() {
    let Some(open) = av1_or_skip() else {
        return;
    };
    let bytes = fs::read(bench_dir().join("av1-1080p.obu")).expect("av1 vector readable");
    let raws = [bytes];
    check_loopback(open, &Packets::from_raws(&raws), "av1");
}

/// AV1 trailing frames arrive over blocking receive.
#[test]
fn av1_blocking_receive_gets_trailing_frame() {
    let Some(open) = av1_or_skip() else {
        return;
    };
    let bytes = fs::read(bench_dir().join("av1-1080p.obu")).expect("av1 vector readable");
    let raws = [bytes];
    check_trailing(open, &Packets::from_raws(&raws), "av1");
}
