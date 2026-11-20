//! Encode config validity: grammar parse, roundtrip bounds, reflection.
//!
//! Skips without hardware; fails when the contract breaks.

use bytes::Bytes;
use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::{Decline, EncodeConfig, Gop, Level, Profile, RateControl};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder, KeyframeRequest};
use rust_video_harness::{Drive, Synth};
use rust_video_vulkan::{
    CodecOp, H264Decoder, H264Encoder, H265Decoder, H265Encoder, SkipReason, VulkanDevice,
    VulkanError, VulkanFrame,
};

/// Validity extent in luma samples.
const WIDTH: u32 = 320;
/// Validity extent in luma samples.
const HEIGHT: u32 = 240;
/// Coded-picture staging cap in bytes.
const BITSTREAM_CAP: usize = 16 << 20;

/// Grammar error behind ingest in tests.
#[derive(Debug)]
struct TestError;

impl rust_video_h264::H264Error for TestError {
    fn parse(kind: rust_video_h264::ParseReason) -> Self {
        let _ = kind;
        Self
    }

    fn unsupported(kind: rust_video_h264::UnsupportedReason) -> Self {
        let _ = kind;
        Self
    }
}

impl rust_video_h265::H265Error for TestError {
    fn parse(kind: rust_video_h265::ParseReason) -> Self {
        let _ = kind;
        Self
    }

    fn unsupported(kind: rust_video_h265::UnsupportedReason) -> Self {
        let _ = kind;
        Self
    }
}

/// Device for `op`, declining loudly when the tier skips.
fn device_or_decline(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open_or_skip(op) {
        Ok(device) => Some(device),
        Err(VulkanError::BackendAbsent(reason @ SkipReason::OutOfDeviceMemory { .. })) => {
            println!("declined:OOM {op:?} on this box: {reason}");
            None
        }
        Err(VulkanError::BackendAbsent(reason)) => {
            println!("declined: {op:?} on this box: {reason}");
            None
        }
        Err(error) if error.is_out_of_device_memory() => {
            println!("declined:OOM {op:?} on this box: {error}");
            None
        }
        Err(error) => panic!("negotiation failed: {error}"),
    }
}

/// Unwrap `result`, declining loud with budget numbers on VRAM exhaustion.
///
/// Panics on other errors.
fn unwrap_or_oom<T>(
    result: Result<T, VulkanError>,
    context: &str,
    device: &VulkanDevice,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if error.is_out_of_device_memory() => {
            let (free_mb, total_mb) = device.memory_budget_mb();
            println!("declined:OOM {context}: {error} (free {free_mb}MB of {total_mb}MB)");
            None
        }
        Err(error) => panic!("{context}: {error}"),
    }
}

/// Legacy H.264 config: auto rate over the defaults.
fn legacy_h264() -> EncodeConfig {
    EncodeConfig {
        rate_control: RateControl::Auto,
        ..Default::default()
    }
}

/// Legacy H.265 config: auto rate over the codec defaults.
fn legacy_h265() -> EncodeConfig {
    EncodeConfig {
        rate_control: RateControl::Auto,
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..Default::default()
    }
}

/// H.264 config under `rate_control` with a `keyint` IDR cadence.
fn h264_config(rate_control: RateControl, keyint: u32) -> EncodeConfig {
    EncodeConfig {
        rate_control,
        gop: Gop {
            keyint,
            min_keyint: keyint,
            b_frames: 0,
            refs: 1,
        },
        ..Default::default()
    }
}

/// H.265 config under `rate_control` with a `keyint` IDR cadence.
fn h265_config(rate_control: RateControl, keyint: u32) -> EncodeConfig {
    EncodeConfig {
        rate_control,
        gop: Gop {
            keyint,
            min_keyint: keyint,
            b_frames: 0,
            refs: 1,
        },
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..Default::default()
    }
}

/// `count` gradient frames at the validity extent.
fn frames(count: u32) -> Vec<CpuFrame> {
    let size = PictureSize::new(WIDTH, HEIGHT).expect("nonzero extent");
    (0..count)
        .map(|index| Synth::gradient(size, index))
        .collect()
}

/// Packets coded from `frames` through `encoder`.
fn encode_run(
    encoder: &mut impl Encoder<Error = VulkanError>,
    frames: &[CpuFrame],
) -> Result<Vec<Packet>, VulkanError> {
    let mut packets = Vec::new();
    for frame in frames {
        packets.extend(encoder.encode(frame)?);
    }
    packets.extend(encoder.flush()?);
    Ok(packets)
}

/// Frames decoded from the Annex-B `stream` through `decoder`.
fn decode_all(
    decoder: &mut impl Decoder<Frame = VulkanFrame, Error = VulkanError>,
    stream: &[u8],
) -> Result<Vec<VulkanFrame>, VulkanError> {
    let mut frames = decoder.decode(&Packet::new(Bytes::copy_from_slice(stream)))?;
    frames.extend(decoder.flush()?);
    Ok(frames)
}

/// Keyframe flags of `packets` in order.
fn marks(packets: &[Packet]) -> Vec<bool> {
    packets.iter().map(Packet::is_keyframe).collect()
}

/// NAL unit types of one Annex-B packet in order.
fn nal_types_h264(packet: &Packet) -> Vec<u8> {
    rust_video_h264::split_nals(packet.data())
        .iter()
        .filter_map(|nal| nal.first())
        .map(|header| rust_video_h264::nal_unit_type(*header))
        .collect()
}

/// NAL unit types of one Annex-B packet in order.
fn nal_types_h265(packet: &Packet) -> Vec<u8> {
    rust_video_h265::split_nals(packet.data())
        .iter()
        .filter_map(|nal| nal.first())
        .map(|header| rust_video_h265::nal_unit_type(*header))
        .collect()
}

/// Auto rate still codes an IDR every 30 pictures for H.264.
#[test]
fn h264_auto_still_idr30() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let opened = H264Encoder::open(&device, WIDTH, HEIGHT, legacy_h264());
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 IDR30 without P frames");
        return;
    }
    let source = frames(66);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 66, "one packet per frame");
    let expected: Vec<bool> = (0..66).map(|index| index % 30 == 0).collect();
    assert_eq!(marks(&packets), expected, "default still IDR30");
}

/// Auto rate still codes an IDR every 30 pictures for H.265.
#[test]
fn h265_auto_still_idr30() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let opened = H265Encoder::open(&device, WIDTH, HEIGHT, legacy_h265());
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h265 IDR30 without P frames");
        return;
    }
    let source = frames(66);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 66, "one packet per frame");
    let expected: Vec<bool> = (0..66).map(|index| index % 30 == 0).collect();
    assert_eq!(marks(&packets), expected, "default still IDR30");
}

/// The keyint knob moves the H.264 IDR cadence.
#[test]
fn h264_keyint_changes_cadence() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let opened = H264Encoder::open(
        &device,
        WIDTH,
        HEIGHT,
        h264_config(RateControl::default(), 20),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 keyint without P frames");
        return;
    }
    let source = frames(66);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 66, "one packet per frame");
    let expected: Vec<bool> = (0..66).map(|index| index % 20 == 0).collect();
    assert_eq!(marks(&packets), expected, "keyint moves the cadence");
}

/// The keyint knob moves the H.265 IDR cadence.
#[test]
fn h265_keyint_changes_cadence() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let opened = H265Encoder::open(
        &device,
        WIDTH,
        HEIGHT,
        h265_config(RateControl::default(), 20),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h265 keyint without P frames");
        return;
    }
    let source = frames(66);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 66, "one packet per frame");
    let expected: Vec<bool> = (0..66).map(|index| index % 20 == 0).collect();
    assert_eq!(marks(&packets), expected, "keyint moves the cadence");
}

/// CQP opens and codes when the driver runs disabled rate control.
#[test]
fn h264_cqp_opens_and_codes() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let config = h264_config(RateControl::Cqp { qp: 23 }, 8);
    let opened = H264Encoder::open(&device, WIDTH, HEIGHT, config);
    if matches!(
        opened,
        Err(VulkanError::Declined(
            Decline::RateControlUnsupported { .. }
        ))
    ) {
        println!("declined: h264 cqp without disabled rate control");
        return;
    }
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 cqp without P frames");
        return;
    }
    let source = frames(16);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 16, "one packet per frame");
    let expected: Vec<bool> = (0..16).map(|index| index % 8 == 0).collect();
    assert_eq!(marks(&packets), expected, "cqp keeps the cadence");
    assert!(
        packets.iter().all(|packet| !packet.data().is_empty()),
        "every packet carries bytes"
    );
}

/// H.264 output parses under the shared grammar.
#[test]
fn h264_output_parses() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let opened = H264Encoder::open(
        &device,
        WIDTH,
        HEIGHT,
        h264_config(RateControl::default(), 4),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 parse without P frames");
        return;
    }
    let source = frames(10);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 10, "one packet per frame");
    assert_eq!(nal_types_h264(&packets[0]), vec![7, 8, 5]);
    assert_eq!(nal_types_h264(&packets[4]), vec![7, 8, 5]);
    for packet in packets.iter().filter(|packet| !packet.is_keyframe()) {
        assert_eq!(nal_types_h264(packet), vec![1]);
    }
    let mut ingest = rust_video_h264::Ingest::<TestError, rust_video_h264::PermissiveGate>::new();
    let mut pictures = 0;
    let mut intra = Vec::new();
    for packet in &packets {
        let completed = ingest
            .ingest_packet(packet.data())
            .expect("grammar parses output");
        for picture in &completed {
            intra.push(picture.intra());
        }
        pictures += completed.len();
    }
    pictures += usize::from(ingest.finish().expect("grammar finishes").is_some());
    assert_eq!(pictures, 10, "every packet completes its picture");
    assert_eq!(intra, marks(&packets), "intra matches keyframe flags");
    let sps = ingest.sps(0).expect("sequence set stored");
    assert_eq!(sps.visible_width(), WIDTH);
    assert_eq!(sps.visible_height(), HEIGHT);
}

/// H.265 output parses under the shared grammar.
#[test]
fn h265_output_parses() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let opened = H265Encoder::open(
        &device,
        WIDTH,
        HEIGHT,
        h265_config(RateControl::default(), 4),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h265 parse without P frames");
        return;
    }
    let source = frames(10);
    let Some(packets) = unwrap_or_oom(encode_run(&mut encoder, &source), "encode runs", &device)
    else {
        return;
    };
    assert_eq!(packets.len(), 10, "one packet per frame");
    for packet in &packets {
        let types = nal_types_h265(packet);
        if packet.is_keyframe() {
            assert_eq!(types[..3], [32, 33, 34]);
            assert!(
                rust_video_h265::is_idr(types[3]),
                "keyframe carries an IDR slice"
            );
        } else {
            assert!(
                types.first().is_some_and(|first| *first < 32),
                "P packet carries no parameter sets"
            );
        }
    }
    let mut ingest = rust_video_h265::Ingest::<TestError, rust_video_h265::PermissiveGate>::new();
    let mut pictures = 0;
    for packet in &packets {
        pictures += ingest
            .ingest_packet(packet.data())
            .expect("grammar parses output")
            .len();
    }
    pictures += usize::from(ingest.finish().expect("grammar finishes").is_some());
    assert_eq!(pictures, 10, "every packet completes its picture");
    let sps = ingest.sps(0).expect("sequence set stored");
    assert_eq!(sps.visible_width(), WIDTH);
    assert_eq!(sps.visible_height(), HEIGHT);
}

/// H.264 output stays bounded and decodes back to every frame.
#[test]
fn h264_roundtrip_bounds() {
    let _guard = Drive::serial();
    let Some(encode_device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let opened = H264Encoder::open(
        &encode_device,
        WIDTH,
        HEIGHT,
        h264_config(RateControl::default(), 4),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 roundtrip without P frames");
        return;
    }
    let source = frames(10);
    let Some(packets) = unwrap_or_oom(
        encode_run(&mut encoder, &source),
        "encode runs",
        &encode_device,
    ) else {
        return;
    };
    assert_eq!(packets.len(), 10, "one packet per frame");
    for packet in &packets {
        assert!(
            !packet.data().is_empty() && packet.data().len() <= BITSTREAM_CAP,
            "packet within bounds"
        );
        let types = nal_types_h264(packet);
        if packet.is_keyframe() {
            assert_eq!(types[0], 7, "headers ride keyframes only");
        } else {
            assert_eq!(types[0], 1, "P packets carry slices only");
        }
    }
    let mut stream = Vec::new();
    for packet in &packets {
        stream.extend_from_slice(packet.data());
    }
    let Some(decode_device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(
        H264Decoder::open(&decode_device),
        "engine starts",
        &decode_device,
    ) else {
        return;
    };
    let Some(back) = unwrap_or_oom(
        decode_all(&mut decoder, &stream),
        "decode runs",
        &decode_device,
    ) else {
        return;
    };
    assert_eq!(back.len(), 10, "roundtrip loses no frame");
    for frame in &back {
        assert_eq!(
            (frame.size().width(), frame.size().height()),
            (WIDTH, HEIGHT)
        );
    }
}

/// H.265 output stays bounded and decodes back to every frame.
#[test]
fn h265_roundtrip_bounds() {
    let _guard = Drive::serial();
    let Some(encode_device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let opened = H265Encoder::open(
        &encode_device,
        WIDTH,
        HEIGHT,
        h265_config(RateControl::default(), 4),
    );
    let Some(mut encoder) = unwrap_or_oom(opened, "encoder opens", &encode_device) else {
        return;
    };
    if !encoder.supports_p_frames() {
        println!("declined: h265 roundtrip without P frames");
        return;
    }
    let source = frames(10);
    let Some(packets) = unwrap_or_oom(
        encode_run(&mut encoder, &source),
        "encode runs",
        &encode_device,
    ) else {
        return;
    };
    assert_eq!(packets.len(), 10, "one packet per frame");
    for packet in &packets {
        assert!(
            !packet.data().is_empty() && packet.data().len() <= BITSTREAM_CAP,
            "packet within bounds"
        );
        let types = nal_types_h265(packet);
        if packet.is_keyframe() {
            assert_eq!(types[0], 32, "headers ride keyframes only");
        } else {
            assert!(
                types.first().is_some_and(|first| *first < 32),
                "P packets carry slices only"
            );
        }
    }
    let mut stream = Vec::new();
    for packet in &packets {
        stream.extend_from_slice(packet.data());
    }
    let Some(decode_device) = device_or_decline(CodecOp::H265Decode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(
        H265Decoder::open(&decode_device),
        "engine starts",
        &decode_device,
    ) else {
        return;
    };
    let Some(back) = unwrap_or_oom(
        decode_all(&mut decoder, &stream),
        "decode runs",
        &decode_device,
    ) else {
        return;
    };
    assert_eq!(back.len(), 10, "roundtrip loses no frame");
    for frame in &back {
        assert_eq!(
            (frame.size().width(), frame.size().height()),
            (WIDTH, HEIGHT)
        );
    }
}

/// Config gaps decline typed through `open`.
#[test]
fn open_declines_typed() {
    let _guard = Drive::serial();
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let crf = h264_config(RateControl::Crf { value: 23 }, 30);
    assert!(
        matches!(
            H264Encoder::open(&device, WIDTH, HEIGHT, crf),
            Err(VulkanError::Declined(
                Decline::RateControlUnsupported { .. }
            ))
        ),
        "crf declines typed"
    );
    let bframes = EncodeConfig {
        gop: Gop {
            keyint: 30,
            min_keyint: 30,
            b_frames: 2,
            refs: 1,
        },
        ..h264_config(RateControl::default(), 30)
    };
    assert!(
        matches!(
            H264Encoder::open(&device, WIDTH, HEIGHT, bframes),
            Err(VulkanError::Declined(Decline::BFramesUnsupported { .. }))
        ),
        "b-frames decline typed"
    );
    let foreign = h265_config(RateControl::Auto, 30);
    assert!(
        matches!(
            H264Encoder::open(&device, WIDTH, HEIGHT, foreign),
            Err(VulkanError::Declined(Decline::ProfileUnsupported { .. }))
        ),
        "foreign profile declines typed"
    );
}

/// Forced keyframe codes the next picture as IDR.
#[test]
fn keyframe_request_forces_idr() {
    let _guard = Drive::serial();
    keyframe_case_h264();
    keyframe_case_h265();
}

/// Forced H.264 keyframe codes the next picture as IDR.
fn keyframe_case_h264() {
    let Some(device) = device_or_decline(CodecOp::H264Encode) else {
        return;
    };
    let config = h264_config(
        RateControl::Cbr {
            bitrate: 2_000_000,
            maxrate: 2_000_000,
            bufsize: 2_000_000,
        },
        60,
    );
    let mut encoder = match H264Encoder::open(&device, WIDTH, HEIGHT, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("cbr skipped: {error}");
            return;
        }
    };
    if !encoder.supports_p_frames() {
        println!("declined: h264 keyframe without P frames");
        return;
    }
    let source = frames(6);
    let mut packets = Vec::new();
    for frame in &source[..4] {
        let Some(coded) = unwrap_or_oom(encoder.encode(frame), "encode runs", &device) else {
            return;
        };
        packets.extend(coded);
    }
    encoder.request_keyframe().expect("keyframe forces");
    for frame in &source[4..] {
        let Some(coded) = unwrap_or_oom(encoder.encode(frame), "encode runs", &device) else {
            return;
        };
        packets.extend(coded);
    }
    packets.extend(encoder.flush().expect("flush runs"));
    assert_eq!(packets.len(), 6, "one packet per frame");
    assert_eq!(nal_types_h264(&packets[4]), vec![7, 8, 5]);
    let mut ingest = rust_video_h264::Ingest::<TestError, rust_video_h264::PermissiveGate>::new();
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(ingest.ingest_packet(packet.data()).expect("grammar parses"));
    }
    if let Some(trailing) = ingest.finish().expect("grammar finishes") {
        pictures.push(trailing);
    }
    assert_eq!(pictures.len(), 6, "six frames parse to six pictures");
    assert!(
        pictures[1..4].iter().any(|picture| !picture.idr),
        "p-frames flow before the request"
    );
    assert!(pictures[4].idr, "first picture after the request is IDR");
    println!("keyframe: p-flow then forced IDR at picture 5");
}

/// Forced H.265 keyframe codes the next picture as IDR.
fn keyframe_case_h265() {
    let Some(device) = device_or_decline(CodecOp::H265Encode) else {
        return;
    };
    let config = h265_config(
        RateControl::Cbr {
            bitrate: 2_000_000,
            maxrate: 2_000_000,
            bufsize: 2_000_000,
        },
        60,
    );
    let mut encoder = match H265Encoder::open(&device, WIDTH, HEIGHT, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("cbr skipped: {error}");
            return;
        }
    };
    if !encoder.supports_p_frames() {
        println!("declined: h265 keyframe without P frames");
        return;
    }
    let source = frames(6);
    let mut packets = Vec::new();
    for frame in &source[..4] {
        let Some(coded) = unwrap_or_oom(encoder.encode(frame), "encode runs", &device) else {
            return;
        };
        packets.extend(coded);
    }
    encoder.request_keyframe().expect("keyframe forces");
    for frame in &source[4..] {
        let Some(coded) = unwrap_or_oom(encoder.encode(frame), "encode runs", &device) else {
            return;
        };
        packets.extend(coded);
    }
    packets.extend(encoder.flush().expect("flush runs"));
    assert_eq!(packets.len(), 6, "one packet per frame");
    let types = nal_types_h265(&packets[4]);
    assert_eq!(types[..3], [32, 33, 34]);
    assert!(rust_video_h265::is_idr(types[3]));
    let mut ingest = rust_video_h265::Ingest::<TestError, rust_video_h265::PermissiveGate>::new();
    let mut pictures = Vec::new();
    for packet in &packets {
        pictures.extend(ingest.ingest_packet(packet.data()).expect("grammar parses"));
    }
    if let Some(trailing) = ingest.finish().expect("grammar finishes") {
        pictures.push(trailing);
    }
    assert_eq!(pictures.len(), 6, "six frames parse to six pictures");
    assert!(
        pictures[1..4].iter().any(|picture| !picture.idr),
        "p-frames flow before the request"
    );
    assert!(pictures[4].idr, "first picture after the request is IDR");
    println!("keyframe: p-flow then forced IDR at picture 5");
}
