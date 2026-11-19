//! Async delivery: loopback frames plus blocking tail drain.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "windows")]

use std::num::NonZeroUsize;

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d12::{
    Codec, D3d12Device, D3d12Error, D3d12Frame, DecoderPump, H264Decoder, HevcDecoder,
};
use rust_video_harness::{decode_dir, AnnexB};

/// Pictures in each shared vector.
const PICTURES: usize = 15;

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

/// Bound of `capacity` frames under `policy`.
fn config(capacity: usize, policy: DropPolicy) -> ChannelConfig {
    ChannelConfig::new(
        NonZeroUsize::new(capacity).expect("nonzero capacity"),
        policy,
    )
}

/// Shared H.264 vector, one packet per picture in order.
fn h264_packets() -> Vec<Packet> {
    let path = decode_dir().join("valid_320x240.h264");
    let packets: Vec<Packet> = AnnexB::picture_packets(&path, AnnexB::is_h264_vcl)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    packets
}

/// Shared HEVC intra vector, one packet per picture in order.
fn hevc_packets() -> Vec<Packet> {
    let path = decode_dir().join("intra_320x240.hevc");
    let data = std::fs::read(path).expect("vector readable");
    let packets: Vec<Packet> = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl)
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect();
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    packets
}

/// Every frame from `packets` through `pump`, tail via blocking receive.
fn drive<D>(pump: &mut DecoderPump<D>, packets: &[Packet]) -> Vec<D3d12Frame>
where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    for packet in packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = Vec::new();
    while let Some(frame) = pump.try_receive().expect("poll") {
        frames.push(frame);
    }
    pump.finish().expect("finish drains");
    while let Some(frame) = pump.receive().expect("tail") {
        frames.push(frame);
    }
    frames
}

/// Frames hold tight NV12 in send order.
fn assert_own_frames(frames: &[D3d12Frame]) {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    assert_eq!(frames.len(), PICTURES, "every picture completes");
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "frame {index} holds tight NV12"
        );
        assert_eq!(frame.identity().generation(), index as u64);
    }
}

/// Sent H.264 packets return as owned frames, then end of stream.
#[test]
fn h264_loopback_returns_own_frames() {
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    let frames = drive(&mut pump, &packets);
    assert_own_frames(&frames);
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert!(pump.is_finished());
    assert!(pump.receive().expect("drained").is_none());
    assert_eq!(pump.decoder().resets(), 0, "clean vector never resets");
}

/// Blocking receive returns the trailing H.264 frame, then end of stream.
#[test]
fn h264_blocking_receive_drains_tail() {
    let Some(device) = device() else {
        return;
    };
    let packets = h264_packets();
    let mut pump = DecoderPump::open(
        H264Decoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..PICTURES - 1 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..PICTURES as u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}

/// Sent HEVC packets return as owned frames, then end of stream.
#[test]
fn hevc_loopback_returns_own_frames() {
    let Some(device) = device() else {
        return;
    };
    if !device.supports(Codec::Hevc) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let packets = hevc_packets();
    let mut pump = DecoderPump::open(
        HevcDecoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    let frames = drive(&mut pump, &packets);
    assert_own_frames(&frames);
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    assert!(pump.is_finished());
    assert!(pump.receive().expect("drained").is_none());
}

/// Blocking receive returns the trailing HEVC frame, then end of stream.
#[test]
fn hevc_blocking_receive_drains_tail() {
    let Some(device) = device() else {
        return;
    };
    if !device.supports(Codec::Hevc) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let packets = hevc_packets();
    let mut pump = DecoderPump::open(
        HevcDecoder::open(&device),
        config(32, DropPolicy::DropOldest),
    );
    for packet in &packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..PICTURES - 1 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..PICTURES as u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}
