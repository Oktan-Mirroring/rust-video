//! Async delivery: loopback frames plus trailing receive per codec.
//!
//! Skips without hardware; fails when the contract breaks.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use bytes::Bytes;
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets,
};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, H264Synth};
use rust_video_vulkan::{
    Av1Decoder, CodecOp, DecoderPump, H264Decoder, H265Decoder, ParseError, VulkanDevice,
    VulkanError, VulkanFrame,
};

// Parallel VkDevice teardown deadlocks in driver join; guard serializes it.
static SERIAL: Mutex<()> = Mutex::new(());

/// Device for `op`, skipping loud when the tier declines.
fn device_or_skip(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open(op) {
        Ok(Some(device)) => Some(device),
        Ok(None) => {
            println!("vulkan skipped: {op:?} declined on this box");
            None
        }
        Err(error) => {
            println!("vulkan skipped: {op:?} failed: {error}");
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

/// Packets over `raws`, built once outside the pump.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

/// True when `error` is the tier declining the vector's PTL profile.
fn profile_gap(error: &VulkanError) -> bool {
    matches!(
        error,
        VulkanError::Parse(ParseError::OutOfRange {
            syntax: "ptl profile",
        })
    )
}

/// Generations of every frame polled from `pump`, oldest first.
fn polled_generations(
    pump: &mut DecoderPump<impl Decoder<Frame = VulkanFrame, Error = VulkanError>>,
) -> Result<Vec<u64>, VulkanError> {
    let mut generations = Vec::new();
    while let Some(frame) = pump.try_receive()? {
        assert_eq!(frame.nv12().len(), tight_nv12(&frame));
        generations.push(frame.identity().generation());
    }
    Ok(generations)
}

/// Tight NV12 byte count of `frame` from its visible size.
fn tight_nv12(frame: &VulkanFrame) -> usize {
    let size = frame.size();
    let width = usize::try_from(size.width()).expect("width fits");
    let height = usize::try_from(size.height()).expect("height fits");
    width * height * 3 / 2
}

/// Sent packets return as owned frames, then end of stream.
fn check_loopback(
    pump: &mut DecoderPump<impl Decoder<Frame = VulkanFrame, Error = VulkanError>>,
    packets: &[Packet],
    expected: usize,
) {
    for packet in packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let generations = polled_generations(pump).expect("poll");
    assert_eq!(generations.len(), expected, "every picture completes");
    assert_eq!(
        generations,
        (0..expected as u64).collect::<Vec<_>>(),
        "frames arrive in display order"
    );
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    pump.finish().expect("finish drains");
    assert!(pump.is_finished());
    assert!(pump.try_receive().expect("drained").is_none());
    assert!(pump.receive().expect("drained").is_none());
}

/// Blocking receive returns the trailing frame, then end of stream.
fn check_trailing(
    pump: &mut DecoderPump<impl Decoder<Frame = VulkanFrame, Error = VulkanError>>,
    packets: &[Packet],
    expected: usize,
) {
    assert!(expected > 0);
    for packet in packets {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut generations = Vec::new();
    for _ in 0..expected - 1 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..expected as u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}

/// H.264 loopback: two synth packets return two owned frames.
#[test]
fn h264_send_poll_loopback_returns_own_frames() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let Ok(decoder) = H264Decoder::open(&device) else {
        println!("vulkan skipped: h264 decode open failed");
        return;
    };
    let mut pump = DecoderPump::open(decoder, config(8, DropPolicy::DropOldest));
    let raws = H264Synth::annex_b_packets();
    assert_eq!(raws.len(), 2, "synth holds sets plus two pictures");
    check_loopback(&mut pump, &packets(&raws), 2);
}

/// H.264 trailing frame arrives over blocking receive.
#[test]
fn h264_blocking_receive_gets_trailing_frame() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::H264Decode) else {
        return;
    };
    let Ok(decoder) = H264Decoder::open(&device) else {
        println!("vulkan skipped: h264 decode open failed");
        return;
    };
    let mut pump = DecoderPump::open(decoder, config(8, DropPolicy::DropOldest));
    let raws = H264Synth::annex_b_packets();
    check_trailing(&mut pump, &packets(&raws), 2);
}

/// H.265 loopback: bench pictures return as owned frames.
#[test]
fn h265_send_poll_loopback_returns_own_frames() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::H265Decode) else {
        return;
    };
    let Ok(decoder) = H265Decoder::open(&device) else {
        println!("vulkan skipped: h265 decode open failed");
        return;
    };
    let mut pump = DecoderPump::open(decoder, config(32, DropPolicy::DropOldest));
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let expected = raws.len();
    assert!(!raws.is_empty(), "bench vector holds pictures");
    for packet in packets(&raws) {
        match pump.try_send(packet) {
            Ok(()) => {}
            Err(rust_video_caps::delivery::TrySendError::Failed { error })
                if profile_gap(&error) =>
            {
                println!(
                    "vulkan skipped: h265 bench vector declares ptl profile 4, tier parses 1-3"
                );
                return;
            }
            Err(error) => panic!("queue room: {error}"),
        }
    }
    let generations = polled_generations(&mut pump).expect("poll");
    assert_eq!(generations.len(), expected, "every picture completes");
    assert_eq!(
        generations,
        (0..expected as u64).collect::<Vec<_>>(),
        "frames arrive in display order"
    );
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    pump.finish().expect("finish drains");
    assert!(pump.receive().expect("drained").is_none());
}

/// H.265 trailing frame arrives over blocking receive.
#[test]
fn h265_blocking_receive_gets_trailing_frame() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::H265Decode) else {
        return;
    };
    let Ok(decoder) = H265Decoder::open(&device) else {
        println!("vulkan skipped: h265 decode open failed");
        return;
    };
    let mut pump = DecoderPump::open(decoder, config(32, DropPolicy::DropOldest));
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let expected = raws.len();
    assert!(!raws.is_empty(), "bench vector holds pictures");
    for packet in packets(&raws) {
        match pump.try_send(packet) {
            Ok(()) => {}
            Err(rust_video_caps::delivery::TrySendError::Failed { error })
                if profile_gap(&error) =>
            {
                println!(
                    "vulkan skipped: h265 bench vector declares ptl profile 4, tier parses 1-3"
                );
                return;
            }
            Err(error) => panic!("queue room: {error}"),
        }
    }
    let mut generations = Vec::new();
    for _ in 0..expected - 1 {
        let frame = pump.try_receive().expect("poll").expect("frame queued");
        generations.push(frame.identity().generation());
    }
    pump.finish().expect("finish drains");
    let trailing = pump.receive().expect("trailing").expect("trailing queued");
    generations.push(trailing.identity().generation());
    assert_eq!(generations, (0..expected as u64).collect::<Vec<_>>());
    assert!(pump.receive().expect("drained").is_none());
}

/// AV1 loopback: bench pictures return as owned frames.
#[test]
fn av1_send_poll_loopback_returns_own_frames() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::Av1Decode) else {
        return;
    };
    let Ok(decoder) = Av1Decoder::open(&device) else {
        println!("vulkan skipped: av1 decode open failed");
        return;
    };
    let mut pump = DecoderPump::open(decoder, config(32, DropPolicy::DropOldest));
    let raws = vec![std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable")];
    let sent = packets(&raws);
    for packet in &sent {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let generations = polled_generations(&mut pump).expect("poll");
    assert!(!generations.is_empty(), "bench vector emits frames");
    let expected = generations.len();
    assert_eq!(
        generations,
        (0..expected as u64).collect::<Vec<_>>(),
        "frames arrive in display order"
    );
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    pump.finish().expect("finish drains");
    assert!(pump.receive().expect("drained").is_none());
}

/// AV1 trailing frame arrives over blocking receive.
#[test]
fn av1_blocking_receive_gets_trailing_frame() {
    let _guard = SERIAL.lock().expect("pump lock");
    let Some(device) = device_or_skip(CodecOp::Av1Decode) else {
        return;
    };
    if Av1Decoder::open(&device).is_err() {
        println!("vulkan skipped: av1 decode open failed");
        return;
    }
    let raws = vec![std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable")];
    let sent = packets(&raws);
    let expected = {
        let mut probe = DecoderPump::open(
            Av1Decoder::open(&device).expect("decode opens"),
            config(32, DropPolicy::DropOldest),
        );
        for packet in &sent {
            probe.try_send(packet.clone()).expect("queue room");
        }
        polled_generations(&mut probe).expect("poll").len()
    };
    assert!(expected > 0, "bench vector emits frames");
    let mut pump = DecoderPump::open(
        Av1Decoder::open(&device).expect("decode opens"),
        config(32, DropPolicy::DropOldest),
    );
    check_trailing(&mut pump, &sent, expected);
}
