//! VA-API async delivery: loopback frames plus trailing receive.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "linux")]

use rust_video_caps::delivery::{DecoderPump, DropPolicy, ReceiveFrames};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth, Packets};
use rust_video_vaapi::{Av1Decoder, H264Decoder, H265Decoder, VaApiDevice, VaApiError, VaApiFrame};

/// Open device, loud skip without hardware.
fn device(name: &str) -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("SKIP vaapi {name} async loopback: {error}");
            None
        }
    }
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

/// Sent packets return as owned frames; blocking receive drains the tail.
fn loopback(
    decoder: impl Decoder<Frame = VaApiFrame, Error = VaApiError>,
    packets: &[Packet],
    name: &str,
) -> Vec<VaApiFrame> {
    let mut pump = DecoderPump::open(decoder, Drive::channel(32, DropPolicy::DropOldest));
    let (mut frames, _) = Drive::send_all(&mut pump, packets, drain);
    frames.extend(Drive::finish_all(&mut pump));
    assert!(pump.is_finished());
    assert!(!frames.is_empty(), "{name}: loopback decoded no frames");
    let generations: Vec<u64> = frames
        .iter()
        .map(|frame| frame.identity().generation())
        .collect();
    let total = generations.len();
    assert_eq!(
        generations,
        (0..total as u64).collect::<Vec<_>>(),
        "{name}: generations run 0..{total}"
    );
    assert_eq!(pump.dropped(), 0, "{name}: roomy queue drops nothing");
    assert!(pump.receive().expect("drained").is_none());
    println!("vaapi {name} async loopback: {total} frames owned back");
    frames
}

/// AV1 packets return as owned frames; blocking receive drains the tail.
#[test]
fn av1_send_poll_loopback_returns_own_frames() {
    let Some(device) = device("av1") else {
        return;
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = Packets::from_raws(&[bytes]);
    loopback(Av1Decoder::open(&device), &packets, "av1");
}

/// H.264 packets return as owned frames; blocking receive drains the tail.
#[test]
fn h264_send_poll_loopback_returns_own_frames() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device("h264") else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets();
    let packets = Packets::from_raws(&raws);
    let frames = loopback(H264Decoder::open(&device), &packets, "h264");
    assert_eq!(frames.len(), 1, "synth vector holds one picture");
    assert_eq!(frames[0].nv12().len(), FRAME_BYTES, "tight NV12 bytes");
}

/// H.265 packets return as owned frames; blocking receive drains the tail.
#[test]
fn h265_send_poll_loopback_returns_own_frames() {
    let Some(device) = device("h265") else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = Packets::from_raws(&raws);
    loopback(H265Decoder::open(&device), &packets, "h265");
}
