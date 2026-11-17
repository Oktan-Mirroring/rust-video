//! VP9 end to end on the D3D11 tier: file vector through decode
//! overlap and reset recovery.
//!
//! Vector `tests/vectors/valid_320x240.ivf` holds 15 ffmpeg
//! testsrc2 frames (320x240, single tile); `examples/common/vp9.rs`
//! loads and splits it.

#![cfg(target_os = "windows")]

#[path = "../examples/common/vp9.rs"]
mod vp9;

use rust_video_d3d11::{Codec, D3d11Device, Vp9Decoder};
use vp9::{frame_packets, vector};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};

/// Frames in the committed vector.
const FRAMES: usize = 15;

/// Members in the overlap superframe.
const OVERLAP: usize = 8;

/// Open device, loud skip without hardware or the VP9 profile.
fn device() -> Option<D3d11Device> {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return None;
        }
    };
    if !device.supports(Codec::Vp9) {
        println!("d3d11 skipped: no vp9 decoder profile");
        return None;
    }
    println!("adapter: {}", device.name());
    Some(device)
}

/// One packet per frame in order, failing loud off the steady path.
fn decode_all(decoder: &mut Vp9Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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

/// Superframe packet over `frames`, two size bytes per member.
fn superframe(frames: &[Vec<u8>]) -> Vec<u8> {
    assert!(
        !frames.is_empty() && frames.len() <= 8,
        "superframe holds 1..=8 members"
    );
    let mut packet = Vec::new();
    for frame in frames {
        assert!(frame.len() <= 0xFFFF, "member fits two size bytes");
        packet.extend_from_slice(frame);
    }
    let marker = 0xC0 | (1 << 3) | u8::try_from(frames.len() - 1).expect("count fits");
    packet.push(marker);
    for frame in frames {
        packet.extend_from_slice(
            &u16::try_from(frame.len())
                .expect("member fits")
                .to_le_bytes(),
        );
    }
    packet.push(marker);
    packet
}

/// Superframe packet queues submits before the first wait.
#[test]
fn overlap_queues_before_first_wait() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = frame_packets(&data);
    let mut decoder = Vp9Decoder::open(&device);
    let packet = Packet::new(Bytes::from(superframe(&raws[..OVERLAP])));
    let mut frames = decoder.decode(&packet).expect("superframe decode");
    frames.extend(decoder.flush().expect("flush drains"));
    let inflight = decoder.max_inflight();
    println!(
        "overlap: {} frames from one packet, max_inflight={inflight} resets={}",
        frames.len(),
        decoder.resets()
    );
    assert_eq!(frames.len(), OVERLAP, "superframe yields every member");
    assert!(
        inflight > 1,
        "overlap needs inflight above 1, got {inflight}"
    );
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// Ordered frames decode with no reset.
#[test]
fn steady_state_never_resets() {
    let Some(device) = device() else {
        return;
    };
    let data = std::fs::read(vector()).expect("vector readable");
    let raws = frame_packets(&data);
    assert!(!raws.is_empty(), "vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
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
    let raws = frame_packets(&data);
    assert!(raws.len() > 1, "vector holds an inter frame");
    let mut decoder = Vp9Decoder::open(&device);
    let skipped = Packet::new(Bytes::copy_from_slice(&raws[1]));
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
    let raws = frame_packets(&data);
    assert!(!raws.is_empty(), "vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
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
