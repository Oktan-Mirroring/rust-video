//! D3D11 uniform legs: shared vectors through overlap, steady
//! state, reset recovery, and oracle parity per codec.
//!
//! H.264, AV1, HEVC, and VP9 run the same shapes over their committed
//! 15-picture vectors; skips without hardware, fails on divergence.

#![cfg(target_os = "windows")]

#[path = "../examples/common/vp9.rs"]
#[allow(dead_code)]
mod vp9;

use std::path::PathBuf;

use rust_video_d3d11::{Av1Decoder, Codec, D3d11Device, H264Decoder, HevcDecoder, Vp9Decoder};
use vp9::frame_packets;

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_harness::{decode_dir, AnnexB, Obu};

/// Pictures in the committed vectors.
const PICTURES: usize = 15;

/// Frames in the committed AV1 and VP9 vectors.
const FRAMES: usize = 15;

/// Open device, loud skip without hardware.
fn device_h264() -> Option<D3d11Device> {
    match D3d11Device::open() {
        Ok(device) => {
            println!("adapter: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("d3d11 skipped: {error}");
            None
        }
    }
}

/// One packet per picture in order, failing loud off the steady path.
fn decode_all_h264(decoder: &mut H264Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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

/// Shared H.264 vector path.
fn vector_h264() -> PathBuf {
    decode_dir().join("valid_320x240.h264")
}

/// Whole-file packet queues submits before the first wait.
#[test]
fn h264_overlap_queues_before_first_wait() {
    let Some(device) = device_h264() else {
        return;
    };
    let raw = std::fs::read(vector_h264()).expect("vector readable");
    let mut decoder = H264Decoder::open(&device);
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
fn h264_steady_state_never_resets() {
    let Some(device) = device_h264() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector_h264(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let total = decode_all_h264(&mut decoder, &raws);
    println!(
        "steady: {total} frames, resets={} max_inflight={}",
        decoder.resets(),
        decoder.max_inflight()
    );
    assert_eq!(total, PICTURES, "steady vector yields every picture");
    assert_eq!(decoder.resets(), 0, "steady vector never resets");
}

/// B-frame pictures decode with no reset.
#[test]
fn h264_bframes_decode_never_resets() {
    let Some(device) = device_h264() else {
        return;
    };
    let raws = AnnexB::picture_packets(
        &decode_dir().join("bframes_320x240.h264"),
        AnnexB::is_h264_vcl,
    );
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let total = decode_all_h264(&mut decoder, &raws);
    println!(
        "bframes: {total} frames, resets={} max_inflight={}",
        decoder.resets(),
        decoder.max_inflight()
    );
    assert_eq!(total, PICTURES, "bframe vector yields every picture");
    assert_eq!(decoder.resets(), 0, "bframe vector never resets");
}

/// Skipped picture fails once, then the stream recovers clean.
#[test]
fn h264_reset_after_failure_recovers() {
    let Some(device) = device_h264() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector_h264(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let first = Packet::new(Bytes::copy_from_slice(&raws[0]));
    decoder.decode(&first).expect("idr opens the stream");
    assert_eq!(decoder.resets(), 0, "open never resets");
    let skipped = Packet::new(Bytes::copy_from_slice(&raws[2]));
    let Err(error) = decoder.decode(&skipped) else {
        panic!("skipped picture fails")
    };
    println!("failure: {error}");
    assert!(
        error.to_string().contains("frame_num gap"),
        "intended trigger, got {error}"
    );
    assert_eq!(decoder.resets(), 1, "failure resets once");
    let total = decode_all_h264(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, PICTURES, "recovery yields every picture");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// Hardware NV12 bytes match the ffmpeg software oracle byte-exact.
#[test]
fn h264_hardware_matches_software_oracle() {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    const FRAME_BYTES: usize = WIDTH * HEIGHT * 3 / 2;
    let Some(device) = device_h264() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector_h264(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
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
    let mut hardware = Vec::with_capacity(PICTURES * FRAME_BYTES);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.nv12().len(),
            FRAME_BYTES,
            "frame {index} holds tight NV12"
        );
        hardware.extend_from_slice(frame.nv12());
    }
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(vector_h264())
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
            offset / FRAME_BYTES,
            offset % FRAME_BYTES,
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

/// Shared AV1 vector path.
fn vector_av1() -> PathBuf {
    decode_dir().join("valid_320x240.obu")
}

/// Open device, loud skip without hardware or the AV1 profile.
fn device_av1() -> Option<D3d11Device> {
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
fn decode_all_av1(decoder: &mut Av1Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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
fn av1_overlap_queues_before_first_wait() {
    let Some(device) = device_av1() else {
        return;
    };
    let raw = std::fs::read(vector_av1()).expect("vector readable");
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
fn av1_steady_state_never_resets() {
    let Some(device) = device_av1() else {
        return;
    };
    let data = std::fs::read(vector_av1()).expect("vector readable");
    let raws = Obu::tu_packets(&data);
    assert!(!raws.is_empty(), "vector holds temporal units");
    let mut decoder = Av1Decoder::open(&device);
    let total = decode_all_av1(&mut decoder, &raws);
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
fn av1_reset_after_failure_recovers() {
    let Some(device) = device_av1() else {
        return;
    };
    let data = std::fs::read(vector_av1()).expect("vector readable");
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
    let total = decode_all_av1(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, FRAMES, "recovery yields every frame");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// Hardware NV12 bytes match the ffmpeg software oracle byte-exact.
#[test]
fn av1_hardware_matches_software_oracle() {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    const FRAME_BYTES: usize = WIDTH * HEIGHT * 3 / 2;
    let Some(device) = device_av1() else {
        return;
    };
    let data = std::fs::read(vector_av1()).expect("vector readable");
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
    parity(&vector_av1(), &frames, FRAME_BYTES);
}

/// Tiled frames match the software oracle byte-exact.
#[test]
fn av1_tiled_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_av1() else {
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
fn av1_film_grain_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_av1() else {
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

/// Shared HEVC vector path.
fn vector_hevc() -> PathBuf {
    decode_dir().join("inter_320x240.hevc")
}

/// Open device, loud skip without hardware or the HEVC profile.
fn device_hevc() -> Option<D3d11Device> {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return None;
        }
    };
    if !device.supports(Codec::H265) {
        println!("d3d11 skipped: no hevc decoder profile");
        return None;
    }
    println!("adapter: {}", device.name());
    Some(device)
}

/// One packet per picture in order, failing loud off the steady path.
fn decode_all_hevc(decoder: &mut HevcDecoder<'_>, raws: &[Vec<u8>]) -> usize {
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
fn hevc_overlap_queues_before_first_wait() {
    let Some(device) = device_hevc() else {
        return;
    };
    let raw = std::fs::read(vector_hevc()).expect("vector readable");
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
fn hevc_steady_state_never_resets() {
    let Some(device) = device_hevc() else {
        return;
    };
    let data = std::fs::read(vector_hevc()).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let total = decode_all_hevc(&mut decoder, &raws);
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
fn hevc_reset_after_failure_recovers() {
    let Some(device) = device_hevc() else {
        return;
    };
    let data = std::fs::read(vector_hevc()).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
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
    let total = decode_all_hevc(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, PICTURES, "recovery yields every picture");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// Hardware NV12 bytes match the ffmpeg software oracle byte-exact.
#[test]
fn hevc_hardware_matches_software_oracle() {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    const FRAME_BYTES: usize = WIDTH * HEIGHT * 3 / 2;
    let Some(device) = device_hevc() else {
        return;
    };
    let data = std::fs::read(vector_hevc()).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
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
    parity(&vector_hevc(), &frames, FRAME_BYTES);
}

/// Multi-slice pictures match the software oracle byte-exact.
#[test]
fn hevc_multislice_matches_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_hevc() else {
        return;
    };
    let path = decode_dir().join("mslice_320x240.hevc");
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
fn hevc_scaling_lists_match_software_oracle() {
    const FRAME_BYTES: usize = 320 * 240 * 3 / 2;
    let Some(device) = device_hevc() else {
        return;
    };
    let path = decode_dir().join("sl_320x240.hevc");
    let data = std::fs::read(&path).expect("vector readable");
    let mut decoder = HevcDecoder::open(&device);
    let packet = Packet::new(Bytes::from(data));
    let mut frames = decoder.decode(&packet).expect("whole-file decode");
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 15, "scaling vector yields every picture");
    parity(&path, &frames, FRAME_BYTES);
}

/// Shared VP9 vector path.
fn vector_vp9() -> PathBuf {
    decode_dir().join("valid_320x240.ivf")
}

/// Members in the overlap superframe.
const OVERLAP: usize = 8;

/// Open device, loud skip without hardware or the VP9 profile.
fn device_vp9() -> Option<D3d11Device> {
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
fn decode_all_vp9(decoder: &mut Vp9Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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
fn vp9_overlap_queues_before_first_wait() {
    let Some(device) = device_vp9() else {
        return;
    };
    let data = std::fs::read(vector_vp9()).expect("vector readable");
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
fn vp9_steady_state_never_resets() {
    let Some(device) = device_vp9() else {
        return;
    };
    let data = std::fs::read(vector_vp9()).expect("vector readable");
    let raws = frame_packets(&data);
    assert!(!raws.is_empty(), "vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
    let total = decode_all_vp9(&mut decoder, &raws);
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
fn vp9_reset_after_failure_recovers() {
    let Some(device) = device_vp9() else {
        return;
    };
    let data = std::fs::read(vector_vp9()).expect("vector readable");
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
    let total = decode_all_vp9(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, FRAMES, "recovery yields every frame");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// Hardware NV12 bytes match the ffmpeg software oracle byte-exact.
#[test]
fn vp9_hardware_matches_software_oracle() {
    const WIDTH: usize = 320;
    const HEIGHT: usize = 240;
    const FRAME_BYTES: usize = WIDTH * HEIGHT * 3 / 2;
    let Some(device) = device_vp9() else {
        return;
    };
    let data = std::fs::read(vector_vp9()).expect("vector readable");
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
    parity(&vector_vp9(), &frames, FRAME_BYTES);
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
