//! Pipelined multi-inflight decode: batch submits, drain-all polls, parity.
//!
//! Device cases need an Android target plus `RUST_VIDEO_MEDIACODEC_DEVICE=1`;
//! anything else skips loudly. Host builds compile this file to nothing.

#![cfg(target_os = "android")]

use std::time::Duration;

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, Drive};
use rust_video_mediacodec::{
    FormatSpec, MediaCodecDecoder, MediaCodecError, MediaCodecSurface, PipelineLedger,
};

/// True when the device gate env requests on-device cases.
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("SKIP mediacodec overlap: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} overlap on this device: {error}");
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
        "mediacodec {name} pipeline: in-flight {}/{} overlap {}",
        ledger.max_in_flight(),
        ledger.depth(),
        ledger.overlap_observed()
    );
    for timing in ledger.timings() {
        println!(
            "mediacodec {name} frame {} submit {}us engine {}us copy {}us total {}us",
            timing.seq(),
            timing.submit_us(),
            timing.engine_wait_us(),
            timing.copy_us(),
            timing.total_us()
        );
    }
}

/// Every timing matches its submission stamp to its completion stamp.
fn check_matched(name: &str, decoder: &MediaCodecDecoder) {
    for timing in decoder.ledger().timings() {
        assert_eq!(
            timing.submitted_pts_us().expect("stamped"),
            u64::try_from(timing.completed_pts_us().expect("stamped")).expect("device pts fit"),
            "{name}: frame {} completion matches its submission",
            timing.seq()
        );
    }
}

/// H.264 spec plus stamped synth packets.
fn h264_case() -> (FormatSpec, Vec<Packet>) {
    let raws =
        AnnexB::picture_packets(&bench_dir().join("h264-1080p_ip.h264"), AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "vector groups into pictures");
    let spec = with_h264_sets(FormatSpec::h264(1920, 1080).expect("valid dims"), &raws[0]);
    (spec, packets(&raws))
}

/// Surface count plus plane bytes for `coded` through the pipelined path.
fn pipelined_bytes(decoder: &mut MediaCodecDecoder, coded: &[Packet]) -> (usize, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut count = 0;
    for packet in coded {
        let surfaces = decoder
            .submit_wait(packet)
            .expect("queue within drain budget");
        count += surfaces.len();
        push_bytes(&mut bytes, &surfaces);
    }
    count += drain_bytes(decoder, &mut bytes);
    let tail = decoder.flush().expect("pipelined drain");
    count += tail.len();
    push_bytes(&mut bytes, &tail);
    (count, bytes)
}

/// Batched submits observe more than one frame in flight on hardware.
#[test]
fn pipelined_decode_overlaps_inputs_on_device() {
    if !device_gate() {
        return;
    }
    let _measure = Drive::serial();
    let (spec, coded) = h264_case();
    let Some(mut decoder) = open_or_decline("h264", &spec) else {
        return;
    };
    let (count, _bytes) = pipelined_bytes(&mut decoder, &coded);
    print_ledger("h264-overlap", &decoder);
    assert!(count > 0, "h264: pipelined run decodes");
    assert!(
        decoder.ledger().overlap_observed(),
        "h264: depth observed above one"
    );
    assert!(
        decoder.ledger().max_in_flight() <= PipelineLedger::DEFAULT_DEPTH,
        "h264: in flight stays under the bound"
    );
    check_matched("h264", &decoder);
}

/// Same vectors through sync decode and the pipelined path agree byte for byte.
#[test]
fn pipelined_bytes_match_sync_decode_on_device() {
    if !device_gate() {
        return;
    }
    let _measure = Drive::serial();
    let (spec, coded) = h264_case();
    let Some(mut sync) = open_or_decline("h264", &spec) else {
        return;
    };
    let mut sync_count = 0;
    let mut sync_bytes = Vec::new();
    for packet in &coded {
        let batch = sync.decode(packet).expect("sync decode");
        sync_count += batch.len();
        push_bytes(&mut sync_bytes, &batch);
    }
    let tail = sync.flush().expect("sync drain");
    sync_count += tail.len();
    push_bytes(&mut sync_bytes, &tail);

    let Some(mut pipelined) = open_or_decline("h264", &spec) else {
        return;
    };
    let (pipe_count, pipe_bytes) = pipelined_bytes(&mut pipelined, &coded);

    print_ledger("h264-sync", &sync);
    print_ledger("h264-pipelined", &pipelined);
    assert!(sync_count > 0, "h264: sync run decodes");
    assert_eq!(pipe_count, sync_count, "h264: same surface count");
    assert_eq!(pipe_bytes, sync_bytes, "h264: byte-exact outputs");
    check_matched("h264", &pipelined);
}
