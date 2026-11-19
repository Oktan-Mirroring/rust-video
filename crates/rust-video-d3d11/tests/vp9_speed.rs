//! VP9 encoder speed tuning: fastest MFT level, valid output.
//!
//! Skips without hardware, without a VP9 encoder MFT, or when the MFT
//! declines the speed property.

#![cfg(target_os = "windows")]

use rust_video_caps::traits::Encoder;
use rust_video_d3d11::{Codec, D3d11Device, D3d11Encoder};
use rust_video_harness::{Drive, Synth};

/// Timed frames per fps observation.
const HOPS: u32 = 30;

/// Warmup frames ahead of each timed run.
const WARMUP: u64 = 4;

/// Fps observation repeats on the contended box.
const REPEATS: u32 = 3;

/// Validity frames behind the ffmpeg decode.
const VALIDITY_FRAMES: u64 = 8;

/// Tuned VP9 encoder plus its first frame bytes.
///
/// The forcing frame configures the media types, which applies the tuning;
/// its bytes head the validity bitstream. Loud skip without hardware, MFT,
/// or property.
fn tuned(width: u32, height: u32) -> Option<(D3d11Encoder, Vec<u8>)> {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vp9 skipped: {error}");
            return None;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, Codec::Vp9) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("vp9 skipped: {error}");
            return None;
        }
    };
    println!("vp9 mft: {}", encoder.mft_name());
    let warmup = Synth::noise(width, height, 999);
    let mut first = Vec::new();
    match encoder.encode(&warmup) {
        Ok(packets) => {
            for packet in packets {
                first.extend_from_slice(packet.data());
            }
        }
        Err(error) => {
            println!("vp9 skipped: configure encode failed ({error})");
            return None;
        }
    }
    if let Some(tuning) = encoder.vp9_tuning() {
        println!("vp9 tuning: quality_vs_speed={}", tuning.quality_vs_speed());
        Some((encoder, first))
    } else {
        println!("vp9 skipped: MFT declines the speed property");
        None
    }
}

/// Encoder tuned before its first frame.
#[test]
fn vp9_tuning_lands_fastest_level() {
    let _serial = Drive::serial();
    let Some((encoder, _)) = tuned(320, 240) else {
        return;
    };
    let tuning = encoder.vp9_tuning().expect("tuning applies");
    assert_eq!(tuning.quality_vs_speed(), 0, "landed level favors speed");
}

/// Tuned VP9 output decodes clean under ffmpeg.
#[test]
fn vp9_tuned_output_decodes_clean() {
    let _serial = Drive::serial();
    let Some((mut encoder, first)) = tuned(320, 240) else {
        return;
    };
    let mut payloads = vec![first];
    for call in 0..VALIDITY_FRAMES {
        let frame = Synth::noise(320, 240, call);
        let mut coded = Vec::new();
        for packet in encoder.encode(&frame).expect("encode runs") {
            coded.extend_from_slice(packet.data());
        }
        assert!(!coded.is_empty(), "call {call} emits a frame");
        payloads.push(coded);
    }
    for packet in encoder.flush().expect("flush runs") {
        assert!(packet.data().is_empty(), "flush holds no frame back");
    }
    let path = std::env::temp_dir().join("rust-video-d3d11-vp9.ivf");
    std::fs::write(&path, ivf(&payloads, 320, 240)).expect("oracle input writable");
    ffmpeg_clean(&path);
    std::fs::remove_file(&path).ok();
    let bytes: usize = payloads.iter().map(Vec::len).sum();
    println!(
        "vp9 validity: {} frames, {bytes} bytes, ffmpeg-clean",
        payloads.len()
    );
}

/// Tuned VP9 calls each return their own stamped packet.
#[test]
fn vp9_tuned_latency_returns_own_frames() {
    let _serial = Drive::serial();
    let Some((mut encoder, _)) = tuned(320, 240) else {
        return;
    };
    let mut step = None;
    let mut first = None;
    for call in 0..4u64 {
        let frame = Synth::noise(320, 240, call);
        let packets = encoder.encode(&frame).expect("encode runs");
        assert!(!packets.is_empty(), "call {call} held its frame back");
        let pts = packets[0].pts().expect("packet stamped");
        if call == 0 {
            first = Some(pts);
        }
        if call == 1 {
            step = Some(
                pts.checked_sub(first.expect("first stamped"))
                    .expect("pts increase"),
            );
        }
        let call = u32::try_from(call).expect("call fits");
        let want = first.expect("first stamped") + step.unwrap_or_default() * call;
        assert!(
            packets.iter().any(|packet| packet.pts() == Some(want)),
            "call {call} missed its own stamp"
        );
    }
    let drained = encoder.flush().expect("flush runs");
    assert!(drained.is_empty(), "flush held {} back", drained.len());
}

/// Contended 1080p fps observations with repeats, reported only.
#[test]
fn vp9_contended_fps_observations() {
    let _serial = Drive::serial();
    let Some((mut encoder, _)) = tuned(1920, 1080) else {
        return;
    };
    let frames: Vec<_> = (0..WARMUP + u64::from(HOPS))
        .map(|seed| Synth::noise(1920, 1080, seed))
        .collect();
    let (warmup, timed) = frames.split_at(usize::try_from(WARMUP).expect("warmup fits"));
    let mut runs = Vec::new();
    for _ in 0..REPEATS {
        for frame in warmup {
            encoder.encode(frame).expect("warmup encode");
        }
        let mut bytes = 0;
        let start = std::time::Instant::now();
        for frame in timed {
            for packet in encoder.encode(frame).expect("timed encode") {
                bytes += packet.data().len();
            }
        }
        for packet in encoder.flush().expect("timed flush") {
            bytes += packet.data().len();
        }
        assert!(bytes > 0);
        runs.push(f64::from(HOPS) / start.elapsed().as_secs_f64());
    }
    let mean = runs.iter().sum::<f64>() / f64::from(REPEATS);
    println!("vp9 contended fps: {runs:?}, mean {mean:.1}");
}

/// IVF file over `payloads` at `width` x `height`, 30 fps.
fn ivf(payloads: &[Vec<u8>], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DKIF");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"VP90");
    out.extend_from_slice(&u16::try_from(width).expect("width fits").to_le_bytes());
    out.extend_from_slice(&u16::try_from(height).expect("height fits").to_le_bytes());
    out.extend_from_slice(&30u32.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(payloads.len())
            .expect("count fits")
            .to_le_bytes(),
    );
    out.extend_from_slice(&0u32.to_le_bytes());
    for (index, payload) in payloads.iter().enumerate() {
        out.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("frame fits")
                .to_le_bytes(),
        );
        out.extend_from_slice(&u64::try_from(index).expect("pts fits").to_le_bytes());
        out.extend_from_slice(payload);
    }
    out
}

/// ffmpeg decode over `path`, failing loud without the oracle.
fn ffmpeg_clean(path: &std::path::Path) {
    let output = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap_or_else(|error| panic!("ffmpeg oracle missing: {error}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ffmpeg rejects the bitstream: {stderr}"
    );
    assert!(output.stderr.is_empty(), "ffmpeg warnings: {stderr}");
}
