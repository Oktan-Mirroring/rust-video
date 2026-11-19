//! Uniform corpus legs over the shared harness bytes: decode, parity, sustained.
//!
//! Lab-gated: needs an Android target plus `RUST_VIDEO_MEDIACODEC_DEVICE=1`;
//! anything else skips loudly. Host builds compile this file to nothing.
//! Bars are provisional until the lab confirms quiet medians. VP8 has no
//! inter-picture vector, so its sustained leg stays out.

#![cfg(target_os = "android")]

use std::time::{Duration, Instant};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, decode_dir, AnnexB, Drive, Ivf};
use rust_video_mediacodec::{
    FormatSpec, MediaCodecDecoder, MediaCodecError, MediaCodecSurface, SubmitOutcome,
};

/// Pictures in each shared vector.
const PICTURES: usize = 15;

/// Provisional sustained bars: tier realtime floor, lab confirms.
const SUSTAINED_H264_BAR: f64 = 60.0;
const SUSTAINED_H265_BAR: f64 = 60.0;
const SUSTAINED_AV1_BAR: f64 = 60.0;
const SUSTAINED_VP9_BAR: f64 = 60.0;

/// True when the device gate env requests on-device cases.
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("SKIP mediacodec uniform: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} uniform on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
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

/// Packets over `raws`, stamped 40ms apart for completion matching.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .enumerate()
        .map(|(n, raw)| {
            Packet::new(Bytes::copy_from_slice(raw)).with_pts(Duration::from_micros(
                u64::try_from(n).expect("small index") * 40_000,
            ))
        })
        .collect()
}

/// Plane bytes of `surfaces`, appended to `bytes`.
fn push_bytes(bytes: &mut Vec<u8>, surfaces: &[MediaCodecSurface]) {
    for surface in surfaces {
        for plane in surface.frame().planes() {
            bytes.extend_from_slice(plane.data());
        }
    }
}

/// Every available output polled into `bytes`; drained surface count.
fn drain_bytes(decoder: &mut MediaCodecDecoder, bytes: &mut Vec<u8>) -> usize {
    let mut count = 0;
    loop {
        let batch = decoder.poll().expect("poll drains");
        if batch.is_empty() {
            return count;
        }
        count += batch.len();
        push_bytes(bytes, &batch);
    }
}

/// Phase table plus overlap verdict over one run.
fn print_ledger(name: &str, decoder: &MediaCodecDecoder) {
    let ledger = decoder.ledger();
    println!(
        "mediacodec {name} uniform: in-flight {}/{} overlap {}",
        ledger.max_in_flight(),
        ledger.depth(),
        ledger.overlap_observed()
    );
}

/// Every timing matches its submission stamp to its completion stamp.
fn check_matched(name: &str, decoder: &MediaCodecDecoder) {
    for timing in decoder.ledger().timings() {
        assert_eq!(
            timing.submitted_pts_us(),
            u64::try_from(timing.completed_pts_us()).expect("device pts fit"),
            "{name}: frame {} completion matches its submission",
            timing.seq()
        );
    }
}

/// Surfaces decoded straight through sync `decode`, plus the flush drain.
fn sync_run(decoder: &mut MediaCodecDecoder, coded: &[Packet]) -> (usize, Vec<u8>) {
    let frames = Drive::decode_all(decoder, coded);
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, &frames);
    (frames.len(), bytes)
}

/// Surfaces decoded through batched pipelined submits, plus the drain.
fn pipelined_run(decoder: &mut MediaCodecDecoder, coded: &[Packet]) -> (usize, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut count = 0;
    for packet in coded {
        loop {
            let (outcome, surfaces) = decoder.submit_nowait(packet).expect("queue attempt");
            count += surfaces.len();
            push_bytes(&mut bytes, &surfaces);
            if outcome == SubmitOutcome::Queued {
                break;
            }
            count += drain_bytes(decoder, &mut bytes);
        }
    }
    count += drain_bytes(decoder, &mut bytes);
    let tail = decoder.flush().expect("pipelined drain");
    count += tail.len();
    push_bytes(&mut bytes, &tail);
    (count, bytes)
}

/// Sync and pipelined runs agree byte for byte over `spec` and `raws`.
fn parity_case(name: &str, spec: &FormatSpec, raws: &[Vec<u8>]) {
    let Some(mut sync) = open_or_decline(name, spec) else {
        return;
    };
    let coded = packets(raws);
    let (sync_count, sync_bytes) = sync_run(&mut sync, &coded);
    let Some(mut pipelined) = open_or_decline(name, spec) else {
        return;
    };
    let (pipe_count, pipe_bytes) = pipelined_run(&mut pipelined, &coded);
    print_ledger(name, &pipelined);
    assert!(sync_count > 0, "{name}: sync run decodes");
    assert_eq!(pipe_count, sync_count, "{name}: same surface count");
    assert_eq!(pipe_bytes, sync_bytes, "{name}: byte-exact outputs");
    check_matched(name, &pipelined);
}

/// Pipelined fps of one pass of `raws` through `spec` against `bar`.
fn sustained_case(name: &str, spec: &FormatSpec, raws: &[Vec<u8>], bar: f64) {
    let Some(mut decoder) = open_or_decline(name, spec) else {
        return;
    };
    let coded = packets(raws);
    let start = Instant::now();
    let (count, _) = pipelined_run(&mut decoder, &coded);
    let wall = start.elapsed().as_secs_f64();
    let total = u32::try_from(count).expect("frame count fits");
    let measured = f64::from(total) / wall;
    println!("mediacodec {name} sustained overlap 1080p: {measured:.1}fps over {count} frames, bar {bar:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= bar,
        "{name}: below sustained bar: {measured:.1}fps < {bar:.0}fps"
    );
}

/// Shared H.264 pictures decode to every surface; lab-gated.
#[test]
fn h264_shared_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let spec = with_h264_sets(FormatSpec::h264(320, 240).expect("valid dims"), &raws[0]);
    let Some(mut decoder) = open_or_decline("h264", &spec) else {
        return;
    };
    let (count, bytes) = sync_run(&mut decoder, &packets(&raws));
    assert_eq!(count, PICTURES, "h264 yields every frame");
    assert!(!bytes.is_empty(), "h264 emits plane bytes");
}

/// Shared H.264 B-frame pictures decode to every surface; lab-gated.
#[test]
fn h264_bframes_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let raws = AnnexB::picture_packets(
        &decode_dir().join("bframes_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let spec = with_h264_sets(FormatSpec::h264(320, 240).expect("valid dims"), &raws[0]);
    let Some(mut decoder) = open_or_decline("h264", &spec) else {
        return;
    };
    let (count, bytes) = sync_run(&mut decoder, &packets(&raws));
    assert_eq!(count, PICTURES, "h264 yields every frame");
    assert!(!bytes.is_empty(), "h264 emits plane bytes");
}

/// Shared H.265 pictures decode to every surface; lab-gated.
#[test]
fn h265_shared_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let raws = AnnexB::picture_packets(
        &decode_dir().join("inter_320x240.hevc"),
        AnnexB::is_hevc_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let spec = with_hevc_sets(FormatSpec::h265(320, 240).expect("valid dims"), &raws[0]);
    let Some(mut decoder) = open_or_decline("h265", &spec) else {
        return;
    };
    let (count, bytes) = sync_run(&mut decoder, &packets(&raws));
    assert_eq!(count, PICTURES, "h265 yields every frame");
    assert!(!bytes.is_empty(), "h265 emits plane bytes");
}

/// Shared AV1 frames decode to every surface; lab-gated.
#[test]
fn av1_shared_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let spec = FormatSpec::av1(320, 240).expect("valid dims");
    let Some(mut decoder) = open_or_decline("av1", &spec) else {
        return;
    };
    let (count, plane_bytes) = sync_run(&mut decoder, &packets(&[bytes]));
    assert_eq!(count, PICTURES, "av1 yields every frame");
    assert!(!plane_bytes.is_empty(), "av1 emits plane bytes");
}

/// Shared VP9 frames decode to every surface; lab-gated.
#[test]
fn vp9_shared_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    let spec = FormatSpec::vp9(320, 240).expect("valid dims");
    let Some(mut decoder) = open_or_decline("vp9", &spec) else {
        return;
    };
    let (count, bytes) = sync_run(&mut decoder, &packets(&raws));
    assert_eq!(count, PICTURES, "vp9 yields every frame");
    assert!(!bytes.is_empty(), "vp9 emits plane bytes");
}

/// Shared VP8 frame decodes to its surface; lab-gated.
#[test]
fn vp8_shared_decode_yields_every_frame() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp8(1920, 1080).expect("valid dims");
    let Some(mut decoder) = open_or_decline("vp8", &spec) else {
        return;
    };
    let (count, bytes) = sync_run(&mut decoder, &packets(&raws));
    assert_eq!(count, raws.len(), "vp8 yields every frame");
    assert!(!bytes.is_empty(), "vp8 emits plane bytes");
}

/// Same shared H.264 bytes through sync and pipelined decode; lab-gated.
#[test]
fn h264_shared_pipelined_matches_sync_bytes() {
    if !device_gate() {
        return;
    }
    let raws = AnnexB::picture_packets(
        &decode_dir().join("valid_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let spec = with_h264_sets(FormatSpec::h264(320, 240).expect("valid dims"), &raws[0]);
    parity_case("h264", &spec, &raws);
}

/// Same shared H.265 bytes through sync and pipelined decode; lab-gated.
#[test]
fn h265_shared_pipelined_matches_sync_bytes() {
    if !device_gate() {
        return;
    }
    let raws = AnnexB::picture_packets(
        &decode_dir().join("inter_320x240.hevc"),
        AnnexB::is_hevc_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let spec = with_hevc_sets(FormatSpec::h265(320, 240).expect("valid dims"), &raws[0]);
    parity_case("h265", &spec, &raws);
}

/// Same shared AV1 bytes through sync and pipelined decode; lab-gated.
#[test]
fn av1_shared_pipelined_matches_sync_bytes() {
    if !device_gate() {
        return;
    }
    let bytes = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let spec = FormatSpec::av1(320, 240).expect("valid dims");
    parity_case("av1", &spec, &[bytes]);
}

/// Same shared VP9 bytes through sync and pipelined decode; lab-gated.
#[test]
fn vp9_shared_pipelined_matches_sync_bytes() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&decode_dir().join("valid_320x240.ivf")).frames;
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per frame");
    let spec = FormatSpec::vp9(320, 240).expect("valid dims");
    parity_case("vp9", &spec, &raws);
}

/// Same shared VP8 bytes through sync and pipelined decode; lab-gated.
#[test]
fn vp8_shared_pipelined_matches_sync_bytes() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "vector holds frames");
    let spec = FormatSpec::vp8(1920, 1080).expect("valid dims");
    parity_case("vp8", &spec, &raws);
}

/// Sustained H.264 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    if !device_gate() {
        return;
    }
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h264-1080p_ip.h264"), AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let spec = with_h264_sets(FormatSpec::h264(1920, 1080).expect("valid dims"), &raws[0]);
    sustained_case("h264", &spec, &raws, SUSTAINED_H264_BAR);
}

/// Sustained H.265 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_h265_1080p_measures_realtime() {
    if !device_gate() {
        return;
    }
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h265-1080p_ip.h265"), AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let spec = with_hevc_sets(FormatSpec::h265(1920, 1080).expect("valid dims"), &raws[0]);
    sustained_case("h265", &spec, &raws, SUSTAINED_H265_BAR);
}

/// Sustained AV1 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_av1_1080p_measures_realtime() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let spec = FormatSpec::av1(1920, 1080).expect("valid dims");
    sustained_case("av1", &spec, &raws, SUSTAINED_AV1_BAR);
}

/// Sustained VP9 overlap throughput against its bar; lab-gated.
#[test]
fn sustained_vp9_1080p_measures_realtime() {
    if !device_gate() {
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp9-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let spec = FormatSpec::vp9(1920, 1080).expect("valid dims");
    sustained_case("vp9", &spec, &raws, SUSTAINED_VP9_BAR);
}
