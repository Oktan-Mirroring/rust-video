//! AV1 end to end on the D3D11 tier: file vector through decode
//! overlap and reset recovery.
//!
//! Shared vector `valid_320x240.obu` holds 15 ffmpeg
//! testsrc2 frames (320x240, reordered with show-existing
//! display); the harness `Obu` splitter groups it.
//! `tiled_320x240.obu` and `grain_320x240.obu` cover tiled
//! frames and film grain through parity.

#![cfg(target_os = "windows")]

use std::path::PathBuf;

use rust_video_d3d11::{Av1Decoder, Codec, D3d11Device};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_harness::{decode_dir, Obu};

/// Shared vector path.
fn vector() -> PathBuf {
    decode_dir().join("valid_320x240.obu")
}

/// Frames in the committed vector.
const FRAMES: usize = 15;

/// Open device, loud skip without hardware or the AV1 profile.
fn device() -> Option<D3d11Device> {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return None;
        }
    };
    if !device.supports(Codec::Av1) {
        println!("d3d11 skipped: no av1 decoder profile");
        return None;
    }
    println!("adapter: {}", device.name());
    Some(device)
}

/// One packet per temporal unit in order, failing loud off the steady path.
fn decode_all(decoder: &mut Av1Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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
    let mut decoder = Av1Decoder::open(&device);
    let packet = Packet::new(Bytes::from(raw));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    let inflight = decoder.max_inflight();
    println!(
        "overlap: {} frames from one packet, max_inflight={inflight} resets={}",
        frames.len(),
        decoder.resets()
    );
    assert_eq!(frames.len(), FRAMES, "whole file yields every frame");
    assert!(
        inflight > 1,
        "overlap needs inflight above 1, got {inflight}"
    );
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// Ordered units decode with no reset.
#[test]
fn steady_state_never_resets() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = Obu::tu_packets(&data);
    assert!(!raws.is_empty(), "vector holds temporal units");
    let mut decoder = Av1Decoder::open(&device);
    let total = decode_all(&mut decoder, &raws);
    println!(
        "steady: {total} frames, resets={} max_inflight={}",
        decoder.resets(),
        decoder.max_inflight()
    );
    assert_eq!(total, FRAMES, "steady vector yields every frame");
    assert_eq!(decoder.resets(), 0, "steady vector never resets");
}

/// Skipped key fails once, then the stream recovers clean.
#[test]
fn reset_after_failure_recovers() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = Obu::tu_packets(&data);
    let obus = Obu::split_obus(&data);
    let mut decoder = Av1Decoder::open(&device);
    for (unit_type, obu) in obus.iter().take(2) {
        assert_ne!(*unit_type, 6, "prefix holds no frame");
        let packet = Packet::new(Bytes::copy_from_slice(obu));
        decoder.decode(&packet).expect("sequence opens the stream");
    }
    assert_eq!(decoder.resets(), 0, "open never resets");
    let inter = obus
        .iter()
        .filter(|(unit_type, _)| *unit_type == 6)
        .nth(1)
        .expect("vector holds an inter frame")
        .1
        .clone();
    let skipped = Packet::new(Bytes::from(inter));
    let Err(error) = decoder.decode(&skipped) else {
        panic!("skipped key fails")
    };
    println!("failure: {error}");
    assert!(
        error.to_string().contains("names empty slot"),
        "intended trigger, got {error}"
    );
    assert_eq!(decoder.resets(), 1, "failure resets once");
    let total = decode_all(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, FRAMES, "recovery yields every frame");
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
    let raws = Obu::tu_packets(&data);
    assert!(!raws.is_empty(), "vector holds temporal units");
    let mut decoder = Av1Decoder::open(&device);
    let mut frames = Vec::new();
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        match decoder.decode(&packet) {
            Ok(out) => frames.extend(out),
            Err(error) => panic!("steady packet {index}: {error}"),
        }
    }
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), FRAMES, "steady vector yields every frame");
    parity(&vector(), &frames, FRAME_BYTES);
}

/// Tiled frames match the software oracle byte-exact.
#[test]
fn tiled_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let path = decode_dir().join("tiled_320x240.obu");
    let data = std::fs::read(&path).expect("vector readable");
    let mut decoder = Av1Decoder::open(&device);
    let packet = Packet::new(Bytes::from(data));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 12, "tiled vector yields every frame");
    parity(&path, &frames, FRAME_BYTES);
}

/// Film grain frames match the software oracle byte-exact.
#[test]
fn film_grain_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device() else {
        return;
    };
    let path = decode_dir().join("grain_320x240.obu");
    let data = std::fs::read(&path).expect("vector readable");
    let mut decoder = Av1Decoder::open(&device);
    let packet = Packet::new(Bytes::from(data));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 15, "grain vector yields every frame");
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
