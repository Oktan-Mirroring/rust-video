#![cfg(target_os = "windows")]

//! D3D12 end to end: file vectors through decode overlap, GPU
//! transcode, and reset recovery.
//!
//! Vector `tests/vectors/valid_320x240.h264` holds 15 ffmpeg
//! testsrc2 pictures (320x240 constrained baseline, IDR at 0 and
//! 8); `examples/common/annexb.rs` loads and splits it.

#[path = "../examples/common/annexb.rs"]
mod annexb;

use annexb::{picture_packets, vector};
use rust_video_d3d12::{Codec, D3d12Device, H264Decoder, Transcoder};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_caps::transcode::Route;

/// Pictures in the committed vector.
const PICTURES: usize = 15;

/// Open device, loud skip without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => {
            println!("adapter: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("d3d12 skipped: {error}");
            None
        }
    }
}

/// One packet per picture in order, failing loud off the steady path.
fn decode_all(decoder: &mut H264Decoder<'_>, raws: &[Vec<u8>]) -> usize {
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
fn steady_state_never_resets() {
    let Some(device) = device() else {
        return;
    };
    let raws = picture_packets();
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let total = decode_all(&mut decoder, &raws);
    println!(
        "steady: {total} frames, resets={} max_inflight={}",
        decoder.resets(),
        decoder.max_inflight()
    );
    assert_eq!(total, PICTURES, "steady vector yields every picture");
    assert_eq!(decoder.resets(), 0, "steady vector never resets");
}

/// Skipped picture fails once, then the stream recovers clean.
#[test]
fn reset_after_failure_recovers() {
    let Some(device) = device() else {
        return;
    };
    let raws = picture_packets();
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
    let total = decode_all(&mut decoder, &raws);
    println!("recovery: {total} frames, resets={}", decoder.resets());
    assert_eq!(total, PICTURES, "recovery yields every picture");
    assert_eq!(decoder.resets(), 1, "recovery adds no reset");
}

/// GPU transcode output stays on the GPU and decodes under ffmpeg.
///
/// Driver 596.36 corrupts H.264 IDR slices on noise-like input, so the
/// oracle verdict is decodable rather than warning-free; the tier
/// proves the bitstream by its Annex-B IDR prefix instead.
#[test]
fn gpu_transcode_stays_on_gpu() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("d3d12 skipped: no h264 transcode ({error})");
            return;
        }
    };
    let raws = picture_packets();
    assert_eq!(raws.len(), PICTURES, "vector holds one packet per picture");
    let mut coded_packets = 0;
    let mut coded_bytes = 0;
    let mut out = Vec::new();
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let outcome = match transcoder.transcode(&packet) {
            Ok(outcome) => outcome,
            Err(error) => panic!("transcode packet {index}: {error}"),
        };
        assert_eq!(
            outcome.plan().route(),
            Route::Gpu,
            "packet {index} stays on gpu"
        );
        coded_packets += outcome.packets().len();
        for coded in outcome.packets() {
            coded_bytes += coded.data().len();
            out.extend_from_slice(coded.data());
        }
    }
    let outcome = transcoder.flush().expect("flush drains");
    assert_eq!(outcome.plan().route(), Route::Gpu, "flush stays on gpu");
    coded_packets += outcome.packets().len();
    for coded in outcome.packets() {
        coded_bytes += coded.data().len();
        out.extend_from_slice(coded.data());
    }
    assert!(coded_bytes > 0, "transcode emits a bitstream");
    let header = [0, 0, 0, 1, 0x67];
    assert!(out.len() >= header.len());
    assert_eq!(&out[..header.len()], &header, "bitstream opens on SPS");
    let path = std::env::temp_dir().join("rust-video-d3d12-e2e.h264");
    std::fs::write(&path, &out).expect("oracle input writable");
    let warnings = ffmpeg_decodable(&path);
    std::fs::remove_file(&path).ok();
    println!(
        "transcode: {coded_packets} packets, {coded_bytes} bytes, route=gpu, ffmpeg-decodable warnings={warnings}"
    );
}

/// ffmpeg decode over `path`, returning its stderr line count.
///
/// Fails loud without the oracle or when ffmpeg rejects the
/// bitstream; concealment warnings stay informational on the
/// corrupting driver.
fn ffmpeg_decodable(path: &std::path::Path) -> usize {
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
    if stderr.is_empty() {
        0
    } else {
        println!("ffmpeg warnings:\n{stderr}");
        stderr.lines().count()
    }
}
