//! Pump loopback over the five shipped codecs: send, poll, drain.
//!
//! Device cases need an Android target plus `RUST_VIDEO_MEDIACODEC_DEVICE=1`;
//! anything else skips loudly. Host builds compile this file to nothing.

#![cfg(target_os = "android")]

use rust_video_caps::delivery::{DecoderPump, DropPolicy, PollFrames, ReceiveFrames, SendPackets};
use rust_video_caps::frame::GpuExport;
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth, Ivf, Packets};
use rust_video_mediacodec::{FormatSpec, MediaCodecDecoder, MediaCodecError, MediaCodecSurface};

/// True when the device gate env requests on-device cases.
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("SKIP mediacodec delivery: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} delivery on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Spec carrying the first SPS/PPS pair in `packet`.
fn with_h264_sets(spec: FormatSpec, packet: &[u8]) -> FormatSpec {
    let (sps, pps) = AnnexB::avc_csd(packet).expect("sps/pps in first packet");
    spec.with_csd_0(sps).with_csd_1(pps)
}

/// Spec carrying VPS in `csd-0` and SPS/PPS in `csd-1` from `packet`.
fn with_hevc_sets(spec: FormatSpec, packet: &[u8]) -> FormatSpec {
    let (vps, sets) = AnnexB::hevc_csd(packet).expect("vps/sps/pps in first packet");
    spec.with_csd_0(vps).with_csd_1(sets)
}

/// Canonical metadata invariants over one surface.
fn check_surface(surface: &MediaCodecSurface) {
    assert_eq!(surface.slice(), 0);
    assert_eq!(surface.origin(), (0, 0));
    assert_eq!(surface.size(), surface.frame().size());
    assert_eq!(surface.format(), surface.frame().format());
    assert!(surface.coded().width() >= surface.size().width());
    assert!(surface.coded().height() >= surface.size().height());
    assert!(matches!(
        surface.export(),
        GpuExport::Opaque { name: "mediacodec" }
    ));
    surface.try_wait().expect("wait completes");
}

/// Sent packets return as owned surfaces, then blocking receive drains the tail.
fn run_loopback(name: &str, spec: &FormatSpec, raws: &[Vec<u8>]) {
    if !device_gate() {
        return;
    }
    let Some(decoder) = open_or_decline(name, spec) else {
        return;
    };
    let coded = Packets::from_raws(raws);
    assert!(!coded.is_empty(), "{name}: vector holds a packet");
    let mut pump = DecoderPump::open(decoder, Drive::channel(32, DropPolicy::DropOldest));
    for packet in &coded {
        pump.try_send(packet.clone()).expect("queue room");
    }
    let mut frames = 0;
    while let Some(surface) = pump.try_receive().expect("poll") {
        check_surface(&surface);
        frames += 1;
    }
    assert!(frames > 0, "{name}: loopback returns own frames");
    assert_eq!(pump.dropped(), 0, "{name}: roomy queue drops nothing");
    pump.finish().expect("finish drains");
    assert!(pump.is_finished(), "{name}: finished after finish");
    let mut tail = 0;
    while pump.receive().expect("drain").is_some() {
        tail += 1;
    }
    println!("mediacodec {name} delivery loopback: {frames} polled, {tail} trailing");
}

/// H.264 loopback over synth packets with real parameter sets.
#[test]
fn h264_loopback_returns_own_frames() {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert!(!raws.is_empty(), "synth yields a packet");
    let spec = with_h264_sets(FormatSpec::h264(1920, 1080).expect("valid dims"), &raws[0]);
    run_loopback("h264", &spec, &raws);
}

/// H.265 loopback over the shared bench vector.
#[test]
fn h265_loopback_returns_own_frames() {
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    assert!(!raws.is_empty(), "vector groups into pictures");
    let spec = with_hevc_sets(FormatSpec::h265(1920, 1080).expect("valid dims"), &raws[0]);
    run_loopback("h265", &spec, &raws);
}

/// AV1 loopback over the shared bench vector.
#[test]
fn av1_loopback_returns_own_frames() {
    let obu = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let spec = FormatSpec::av1(1920, 1080).expect("valid dims");
    run_loopback("av1", &spec, &[obu]);
}

/// VP9 loopback over the shared bench vector.
#[test]
fn vp9_loopback_returns_own_frames() {
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp9(1920, 1080).expect("valid dims");
    run_loopback("vp9", &spec, &raws);
}

/// VP8 loopback over the shared bench vector.
#[test]
fn vp8_loopback_returns_own_frames() {
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp8(1920, 1080).expect("valid dims");
    run_loopback("vp8", &spec, &raws);
}
