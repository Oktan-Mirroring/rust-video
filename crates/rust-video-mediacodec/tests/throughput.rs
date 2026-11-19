//! Device 1080p throughput against the 60 fps bar.
//!
//! Loud decline without the codec; host builds compile this file to nothing.

#![cfg(target_os = "android")]

use rust_video_caps::frame::{CpuFrame, GpuExport};
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth, Ivf, Packets, Synth};
use rust_video_mediacodec::{
    EncodeSpec, FormatSpec, MediaCodecDecoder, MediaCodecEncoder, MediaCodecError,
    MediaCodecSurface,
};

/// Timed iterations per case.
const ITERS: u32 = 30;

/// Realtime bar in frames per second.
const BAR: f64 = 60.0;

/// True when the device gate env requests on-device cases.
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("SKIP mediacodec surface: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Spec carrying the first SPS/PPS pair in `packet`.
fn with_h264_sets(spec: FormatSpec, packet: &[u8]) -> FormatSpec {
    let mut sps = None;
    let mut pps = None;
    for nal in AnnexB::split_nals(packet) {
        match nal.first().map(|header| header & 0x1F) {
            Some(7) if sps.is_none() => sps = Some(nal.to_vec()),
            Some(8) if pps.is_none() => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    spec.with_csd_0(sps.expect("sps in first packet"))
        .with_csd_1(pps.expect("pps in first packet"))
}

/// Spec carrying VPS in `csd-0` and SPS/PPS in `csd-1` from `packet`.
fn with_hevc_sets(spec: FormatSpec, packet: &[u8]) -> FormatSpec {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for nal in AnnexB::split_nals(packet) {
        match nal.first().map(|header| header >> 1) {
            Some(32) if vps.is_none() => vps = Some(nal.to_vec()),
            Some(33) if sps.is_none() => sps = Some(nal.to_vec()),
            Some(34) if pps.is_none() => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    let mut sets = sps.expect("sps in first packet");
    sets.extend_from_slice(&pps.expect("pps in first packet"));
    spec.with_csd_0(vps.expect("vps in first packet"))
        .with_csd_1(sets)
}

/// Four 1080p noise frames over seeds 1..=4.
fn noise_frames() -> Vec<CpuFrame> {
    (1_u64..=4)
        .map(|seed| Synth::noise(1920, 1080, seed))
        .collect()
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_decoder_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} decode on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Encoder for `spec`, or loud decline when the device lacks the codec.
fn open_encoder_or_decline(name: &str, spec: &EncodeSpec) -> Option<MediaCodecEncoder> {
    match MediaCodecEncoder::open(spec) {
        Ok(encoder) => Some(encoder),
        Err(error @ (MediaCodecError::NoEncoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} encode on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
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
    surface.wait().expect("wait completes");
}

/// Warmup plus timed decode of `raws` against the realtime bar.
fn run_decode(name: &str, spec: &FormatSpec, raws: &[Vec<u8>]) {
    let _measure = Drive::serial();
    let Some(mut decoder) = open_decoder_or_decline(name, spec) else {
        return;
    };
    let coded = Packets::from_raws(raws);
    let warmup = Drive::decode_all(&mut decoder, &coded).len();
    assert!(warmup > 0, "{name}: warmup decodes");
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in &coded {
            decoder.decode(packet).expect("timed decode");
        }
    }
    decoder.flush().expect("timed drain");
    let fps = f64::from(ITERS) / start.elapsed().as_secs_f64();
    println!("mediacodec {name} decode 1080p: {fps:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        fps >= BAR,
        "{name}: below realtime bar: {fps:.1}fps < {BAR:.0}fps"
    );
}

/// Warmup plus timed encode of `frames` against the realtime bar.
fn run_encode(name: &str, spec: &EncodeSpec, frames: &[CpuFrame]) {
    let _measure = Drive::serial();
    let Some(mut encoder) = open_encoder_or_decline(name, spec) else {
        return;
    };
    let mut warmup = 0;
    for frame in frames {
        warmup += encoder.encode(frame).expect("warmup encode").len();
    }
    warmup += encoder.flush().expect("warmup drain").len();
    assert!(warmup > 0, "{name}: warmup encodes");
    let timed = usize::try_from(ITERS).expect("iters fit");
    let start = std::time::Instant::now();
    for frame in frames.iter().cycle().take(timed) {
        encoder.encode(frame).expect("timed encode");
    }
    encoder.flush().expect("timed drain");
    let fps = f64::from(ITERS) / start.elapsed().as_secs_f64();
    println!("mediacodec {name} encode 1080p: {fps:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        fps >= BAR,
        "{name}: below realtime bar: {fps:.1}fps < {BAR:.0}fps"
    );
}

/// Warmup plus timed surface-path decode of `raws` against the realtime bar.
///
/// Decodes like `run_decode`, then walks the surface contract per picture:
/// extent, format, export, wait. Surfaces drop each pass, so slot pressure
/// stays at one pass of pictures.
fn run_surface_decode(name: &str, spec: &FormatSpec, raws: &[Vec<u8>]) {
    if !device_gate() {
        return;
    }
    let _measure = Drive::serial();
    let Some(mut decoder) = open_decoder_or_decline(name, spec) else {
        return;
    };
    let coded = Packets::from_raws(raws);
    let mut warmup = 0;
    for packet in &coded {
        for surface in decoder.decode(packet).expect("warmup decode") {
            check_surface(&surface);
            warmup += 1;
        }
    }
    for surface in decoder.flush().expect("warmup drain") {
        check_surface(&surface);
        warmup += 1;
    }
    assert!(warmup > 0, "{name}: warmup decodes");
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in &coded {
            for surface in decoder.decode(packet).expect("timed decode") {
                check_surface(&surface);
            }
        }
    }
    for surface in decoder.flush().expect("timed drain") {
        check_surface(&surface);
    }
    let fps = f64::from(ITERS) / start.elapsed().as_secs_f64();
    println!("mediacodec {name} surface 1080p: {fps:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        fps >= BAR,
        "{name}: below realtime bar: {fps:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert!(!raws.is_empty(), "synth yields a packet");
    let spec = with_h264_sets(FormatSpec::h264(1920, 1080).expect("valid dims"), &raws[0]);
    run_decode("h264", &spec, &raws);
}

/// 1080p H.265 decode measured against the realtime bar.
#[test]
fn decode_h265_1080p_measures_realtime() {
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    assert!(!raws.is_empty(), "vector groups into pictures");
    let spec = with_hevc_sets(FormatSpec::h265(1920, 1080).expect("valid dims"), &raws[0]);
    run_decode("h265", &spec, &raws);
}

/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let obu = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let spec = FormatSpec::av1(1920, 1080).expect("valid dims");
    run_decode("av1", &spec, &[obu]);
}

/// 1080p VP9 decode measured against the realtime bar.
#[test]
fn decode_vp9_1080p_measures_realtime() {
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp9(1920, 1080).expect("valid dims");
    run_decode("vp9", &spec, &raws);
}

/// 1080p VP8 decode measured against the realtime bar.
#[test]
fn decode_vp8_1080p_measures_realtime() {
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp8(1920, 1080).expect("valid dims");
    run_decode("vp8", &spec, &raws);
}

/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn encode_h264_1080p_measures_realtime() {
    let spec = EncodeSpec::h264(1920, 1080, 8_000_000, 60, 1).expect("valid spec");
    run_encode("h264", &spec, &noise_frames());
}

/// 1080p H.265 encode measured against the realtime bar.
#[test]
fn encode_h265_1080p_measures_realtime() {
    let spec = EncodeSpec::h265(1920, 1080, 8_000_000, 60, 1).expect("valid spec");
    run_encode("h265", &spec, &noise_frames());
}

/// 1080p AV1 encode measured against the realtime bar.
#[test]
fn encode_av1_1080p_measures_realtime() {
    let spec = EncodeSpec::av1(1920, 1080, 8_000_000, 60, 1).expect("valid spec");
    run_encode("av1", &spec, &noise_frames());
}

/// 1080p H.264 surface path measured against the realtime bar.
#[test]
fn decode_h264_1080p_surface_measures_realtime() {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    assert!(!raws.is_empty(), "synth yields a packet");
    let spec = with_h264_sets(FormatSpec::h264(1920, 1080).expect("valid dims"), &raws[0]);
    run_surface_decode("h264", &spec, &raws);
}

/// 1080p H.265 surface path measured against the realtime bar.
#[test]
fn decode_h265_1080p_surface_measures_realtime() {
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    assert!(!raws.is_empty(), "vector groups into pictures");
    let spec = with_hevc_sets(FormatSpec::h265(1920, 1080).expect("valid dims"), &raws[0]);
    run_surface_decode("h265", &spec, &raws);
}

/// 1080p AV1 surface path measured against the realtime bar.
#[test]
fn decode_av1_1080p_surface_measures_realtime() {
    let obu = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let spec = FormatSpec::av1(1920, 1080).expect("valid dims");
    run_surface_decode("av1", &spec, &[obu]);
}

/// 1080p VP9 surface path measured against the realtime bar.
#[test]
fn decode_vp9_1080p_surface_measures_realtime() {
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp9(1920, 1080).expect("valid dims");
    run_surface_decode("vp9", &spec, &raws);
}

/// 1080p VP8 surface path measured against the realtime bar.
#[test]
fn decode_vp8_1080p_surface_measures_realtime() {
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp8(1920, 1080).expect("valid dims");
    run_surface_decode("vp8", &spec, &raws);
}
