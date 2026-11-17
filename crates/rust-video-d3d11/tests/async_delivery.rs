//! Async delivery: loopback frames, trailing receive, drop policies.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "windows")]

#[path = "../examples/common/av1.rs"]
#[allow(dead_code)]
mod av1;
#[path = "../examples/common/hevc.rs"]
mod hevc;

use std::num::NonZeroUsize;

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets,
};
use rust_video_caps::packet::Packet;
use rust_video_d3d11::{Av1Decoder, Codec, D3d11Device, DecoderPump, H264Decoder, HevcDecoder};
use rust_video_harness::AnnexB;

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

/// Committed H.264 vector path.
fn h264_vector() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/valid_320x240.h264")
}

/// Bound of `capacity` frames under `policy`.
fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
    ChannelConfig::new(
        NonZeroUsize::new(capacity).expect("nonzero capacity"),
        policy,
    )
}

/// One packet per picture in order.
fn picture_packets() -> Vec<Packet> {
    AnnexB::picture_packets(&h264_vector(), AnnexB::is_h264_vcl)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// One packet per HEVC picture in order.
fn hevc_packets() -> Vec<Packet> {
    let data = std::fs::read(hevc::vector()).expect("vector readable");
    hevc::picture_packets(&data)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// One packet per AV1 temporal unit in order.
fn av1_packets() -> Vec<Packet> {
    let data = std::fs::read(av1::vector()).expect("vector readable");
    av1::tu_packets(&data)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// Sent packets return as owned frames, then end of stream.
#[test]
fn send_poll_loopback_returns_own_frames() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let packets = picture_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    assert_eq!(frames.len(), 15, "every picture completes");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "frame {index} holds tight NV12"
        );
        assert_eq!(frame.identity().generation(), index as u64);
    }
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    assert!(pump.try_receive().expect("drained").is_none());
    assert!(pump.receive().expect("drained").is_none());
    assert_eq!(pump.decoder().resets(), 0, "clean vector never resets");
}

/// Blocking receive returns the trailing frame, then end of stream.
#[test]
fn blocking_receive_gets_trailing_frame() {
    let Some(device) = device() else {
        return;
    };
    let packets = picture_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..14 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..15u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}

/// Slow consumer under drop-oldest keeps the latest frames.
#[test]
fn slow_consumer_drop_oldest_keeps_latest() {
    let Some(device) = device() else {
        return;
    };
    let packets = picture_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(2, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("policy absorbs");
    }
    assert_eq!(pump.dropped(), 13, "slow consumer drops all but two");
    pump.finish().expect("finish drains");
    let mut generations = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        generations.push(frame.identity().generation());
    }
    assert_eq!(generations, [13, 14]);
    assert!(pump.receive().expect("drained").is_none());
}

/// Slow consumer under drop-incoming keeps the first frames.
#[test]
fn slow_consumer_drop_incoming_keeps_first() {
    let Some(device) = device() else {
        return;
    };
    let packets = picture_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(2, DropPolicy::DropIncoming),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("policy absorbs");
    }
    assert_eq!(pump.dropped(), 13, "slow consumer drops all but two");
    pump.finish().expect("finish drains");
    let mut generations = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        generations.push(frame.identity().generation());
    }
    assert_eq!(generations, [0, 1]);
    assert!(pump.receive().expect("drained").is_none());
}

/// Callback drain collects every queued frame oldest-first.
#[test]
fn callback_drain_collects_queued_frames() {
    let Some(device) = device() else {
        return;
    };
    let packets = picture_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    let drained = pump
        .channel()
        .drain_with(|frame| generations.push(frame.identity().generation()));
    assert_eq!(drained, 15);
    assert_eq!(generations, (0..15u64).collect::<Vec<_>>());
    assert!(pump.is_empty());
    pump.finish().expect("finish drains");
    assert!(pump.receive().expect("drained").is_none());
}

/// Sent HEVC packets return as owned frames, then end of stream.
#[test]
fn hevc_send_poll_loopback_returns_own_frames() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_for(Codec::Hevc) else {
        return;
    };
    let packets = hevc_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        HevcDecoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    while let Some(frame) = pump.receive().expect("drained") {
        frames.push(frame);
    }
    assert_eq!(frames.len(), 15, "every picture completes");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "frame {index} holds tight NV12"
        );
        assert_eq!(frame.identity().generation(), index as u64);
    }
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert!(pump.receive().expect("drained").is_none());
    assert_eq!(pump.decoder().resets(), 0, "clean vector never resets");
}

/// Blocking receive returns the trailing HEVC frames, then end of stream.
#[test]
fn hevc_blocking_receive_gets_trailing_frames() {
    let Some(device) = device_for(Codec::Hevc) else {
        return;
    };
    let packets = hevc_packets();
    assert_eq!(packets.len(), 15, "vector holds one packet per picture");
    let mut pump = DecoderPump::open(
        HevcDecoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..12 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    while let Some(frame) = pump.receive().expect("trailing") {
        generations.push(frame.identity().generation());
    }
    assert_eq!(generations, (0..15u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}

/// Sent AV1 units return as owned frames, then end of stream.
#[test]
fn av1_send_poll_loopback_returns_own_frames() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let packets = av1_packets();
    assert_eq!(packets.len(), 16, "vector holds one packet per unit");
    let mut pump = DecoderPump::open(
        Av1Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    while let Some(frame) = pump.receive().expect("drained") {
        frames.push(frame);
    }
    assert_eq!(frames.len(), 15, "every frame completes");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "frame {index} holds tight NV12"
        );
        assert_eq!(frame.identity().generation(), index as u64);
    }
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert!(pump.receive().expect("drained").is_none());
    assert_eq!(pump.decoder().resets(), 0, "clean vector never resets");
}

/// Blocking receive returns the trailing AV1 frame, then end of stream.
#[test]
fn av1_blocking_receive_gets_trailing_frame() {
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let packets = av1_packets();
    assert_eq!(packets.len(), 16, "vector holds one packet per unit");
    let mut pump = DecoderPump::open(
        Av1Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..14 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..15u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}
