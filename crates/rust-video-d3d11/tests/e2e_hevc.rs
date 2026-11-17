//! HEVC end to end on the D3D11 tier: file vector through decode
//! overlap and reset recovery.
//!
//! Vector `tests/vectors/valid_320x240.hevc` holds 15 ffmpeg
//! testsrc2 pictures (320x240, IDR at 0 and CRA at 8 with B
//! reordering); `examples/common/hevc.rs` loads and splits it.
//! `tests/vectors/mslice_320x240.hevc` and
//! `tests/vectors/sl_320x240.hevc` cover multi-slice pictures
//! and scaling lists through parity.

#![cfg(target_os = "windows")]

#[path = "../examples/common/hevc.rs"]
mod hevc;

use hevc::{picture_packets, vector};
use rust_video_d3d11::{Codec, D3d11Device, HevcDecoder};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};

/// Pictures in the committed vector.
const PICTURES: usize = 15;

/// Open device, loud skip without hardware or the HEVC profile.
fn device() -> Option<D3d11Device> {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return None;
        }
    };
    if !device.supports(Codec::Hevc) {
        println!("d3d11 skipped: no hevc decoder profile");
        return None;
    }
    println!("adapter: {}", device.name());
    Some(device)
}

/// One packet per picture in order, failing loud off the steady path.
fn decode_all(decoder: &mut HevcDecoder<'_>, raws: &[Vec<u8>]) -> usize {
    let mut total = 0;
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        match decoder.decode(&packet) {
            Ok(frames) => total += frames.len(),
            Err(error) => panic!("steady packet {index}: {error}"),
        }
    }
    total += decoder.flush().expect("flush drains").len();
    total
}

/// Whole-file packet queues submits before the first wait.
#[test]
fn overlap_queues_before_first_wait() {
    let Some(device) = device() else {
        return;
    };
    let raw = std::fs::read(vector()).expect("vector readable");
    let mut decoder = HevcDecoder::open(&device);
    let packet = Packet::new(Bytes::from(raw));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    let inflight = decoder.max_inflight();
    println!(
        "overlap: {} frames from one packet, max_inflight={inflight} resets={}",
        frames.len(),
        decoder.resets()
    );
    assert_eq!(frames.len(), PICTURES, "whole file yields every picture");
    assert!(
        inflight > 1,
        "overlap needs inflight above 1, got {inflight}"
    );
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// Ordered pictures decode with no reset.
#[test]
fn steady_state_never_resets() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = picture_packets(&data);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let total = decode_all(&mut decoder, &raws);
    println!(
        "steady: {total} frames, resets={} max_inflight={}",
        decoder.resets(),
        decoder.max_inflight()
    );
    assert_eq!(total, PICTURES, "steady vector yields every picture");
    assert_eq!(decoder.resets(), 0, "steady vector never resets");
}

/// Skipped references fail once, then the stream recovers clean.
#[test]
fn reset_after_failure_recovers() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = picture_packets(&data);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let first = Packet::new(Bytes::copy_from_slice(&raws[0]));
    decoder.decode(&first).expect("idr opens the stream");
    assert_eq!(decoder.resets(), 0, "open never resets");
    let skipped = Packet::new(Bytes::copy_from_slice(&raws[2]));
    let Err(error) = decoder.decode(&skipped) else {
        panic!("skipped references fail")
    };
    println!("failure: {error}");
    assert!(
        error.to_string().contains("reference poc"),
        "intended trigger, got {error}"
    );
    assert_eq!(decoder.resets(), 1, "failure resets once");
    let total = decode_all(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, PICTURES, "recovery yields every picture");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// Hardware NV12 bytes match the ffmpeg software oracle byte-exact.
#[test]
fn hardware_matches_software_oracle() {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    const FRAME_BYTES: usize = WIDTH * HEIGHT * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = picture_packets(&data);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let mut frames = Vec::new();
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        match decoder.decode(&packet) {
            Ok(out) => frames.extend(out),
            Err(error) => panic!("steady packet {index}: {error}"),
        }
    }
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), PICTURES, "steady vector yields every picture");
    parity(&vector(), &frames, FRAME_BYTES);
}

/// Multi-slice pictures match the software oracle byte-exact.
#[test]
fn multislice_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors/mslice_320x240.hevc");
    let data = std::fs::read(&path).expect("vector readable");
    let mut decoder = HevcDecoder::open(&device);
    let packet = Packet::new(Bytes::from(data));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 12, "multislice vector yields every picture");
    parity(&path, &frames, FRAME_BYTES);
}

/// Scaling lists match the software oracle byte-exact.
#[test]
fn scaling_lists_match_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/sl_320x240.hevc");
    let data = std::fs::read(&path).expect("vector readable");
    let mut decoder = HevcDecoder::open(&device);
    let packet = Packet::new(Bytes::from(data));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 15, "scaling vector yields every picture");
    parity(&path, &frames, FRAME_BYTES);
}

/// Decoded `frames` match the ffmpeg oracle for `path` byte-exact.
fn parity(path: &std::path::Path, frames: &[rust_video_d3d11::D3d11Frame], frame_bytes: usize) {
    let mut hardware = Vec::with_capacity(frames.len() * frame_bytes);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            frame_bytes,
            "frame {index} holds tight NV12"
        );
        hardware.extend_from_slice(frame.nv12());
    }
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "nv12", "-"])
        .output()
        .unwrap_or_else(|error| panic!("ffmpeg oracle missing: {error}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "ffmpeg oracle failed: {stderr}");
    let software = output.stdout;
    assert_eq!(
        software.len(),
        hardware.len(),
        "oracle byte count matches hardware"
    );
    if hardware != software {
        let offset = hardware
            .iter()
            .zip(software.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        panic!(
            "parity mismatch at frame {} offset {} (global {offset}): hw={} sw={}",
            offset / frame_bytes,
            offset % frame_bytes,
            hardware[offset],
            software[offset]
        );
    }
    println!(
        "parity: {} frames, {} bytes compared, 0 mismatches",
        frames.len(),
        hardware.len()
    );
}
