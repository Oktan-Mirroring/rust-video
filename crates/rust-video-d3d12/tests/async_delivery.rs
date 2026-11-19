//! Async delivery: loopback frames plus blocking tail drain.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "windows")]

use rust_video_caps::delivery::{DecoderPump, DropPolicy, PollFrames, ReceiveFrames, SendPackets};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d12::{Codec, D3d12Device, D3d12Error, D3d12Frame, H264Decoder, HevcDecoder};
use rust_video_harness::{decode_dir, AnnexB, Drive, Packets, Parity};

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

/// Shared HEVC intra vector, one packet per picture in order.
fn hevc_packets() -> Vec<Packet> {
    let path = decode_dir().join("intra_320x240.hevc");
    let data = std::fs::read(path).expect("vector readable");
    let packets = Packets::from_raws(&AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl));
    assert_eq!(
        packets.len(),
        PICTURES,
        "vector holds one packet per picture"
    );
    packets
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

/// Every frame from `packets` through `pump`, tail via blocking receive.
fn drive<D>(pump: &mut DecoderPump<D>, packets: &[Packet]) -> Vec<D3d12Frame>
where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    let (mut frames, _) = Drive::send_all(pump, packets, drain);
    frames.extend(Drive::finish_all(pump));
    frames
}

/// Frames hold tight NV12 in send order.
fn assert_own_frames(frames: &[D3d12Frame]) {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    Parity::assert_ordered(
        "d3d12",
        frames,
        PICTURES,
        FRAME_BYTES,
        D3d12Frame::nv12,
        |frame| frame.identity().generation(),
    );
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
        Drive::channel(32, DropPolicy::DropOldest),
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
        Drive::channel(32, DropPolicy::DropOldest),
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
        Drive::channel(32, DropPolicy::DropOldest),
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
        Drive::channel(32, DropPolicy::DropOldest),
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
