#![cfg(have_corpus)]
#![cfg(target_os = "linux")]

//! VA-API end-to-end: synth vectors in, hashed frames out.

#[path = "support/mp4.rs"]
mod mp4;

use std::path::{Path, PathBuf};
use std::process::Command;

use bytes::Bytes;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_harness::FrameHash;
use rust_video_vaapi::{H264Decoder, VaApiDevice, VaApiError, VaApiFrame};

/// Corpus root: `$RUST_VIDEO_CORPUS` or the checkout's `corpus/`.
fn corpus() -> PathBuf {
    std::env::var_os("RUST_VIDEO_CORPUS").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../corpus")
                .canonicalize()
                .expect("workspace corpus dir")
        },
        PathBuf::from,
    )
}

/// Scratch dir for ffmpeg oracle output, removed after each oracle run.
fn scratch() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.tmp")
        .join(format!("vaapi-e2e-{}", std::process::id()))
}

/// NAL units of Annex B `bytes`, start codes stripped.
fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut start = None;
    let mut cursor = 0;
    while cursor + 3 <= bytes.len() {
        let long = bytes.get(cursor..cursor + 4) == Some(&[0, 0, 0, 1][..]);
        let short = !long && bytes.get(cursor..cursor + 3) == Some(&[0, 0, 1][..]);
        if long || short {
            if let Some(begin) = start.take() {
                units.push(&bytes[begin..cursor]);
            }
            cursor += if long { 4 } else { 3 };
            start = Some(cursor);
        } else {
            cursor += 1;
        }
    }
    if let Some(begin) = start {
        units.push(&bytes[begin..]);
    }
    units
}

/// Demuxed samples as one Annex B blob.
fn annex_b(stream: &mp4::Stream) -> Vec<u8> {
    let mut out = Vec::new();
    for sample in &stream.samples {
        out.extend_from_slice(sample);
    }
    out
}

/// ffmpeg remux of `path` to Annex B, or `None` without ffmpeg.
fn ffmpeg_remux(path: &Path, scratch: &Path) -> Option<Vec<u8>> {
    std::fs::create_dir_all(scratch).ok()?;
    let out = scratch.join("remux.h264");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-c:v")
        .arg("copy")
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    std::fs::remove_dir_all(scratch).ok();
    Some(bytes)
}

/// ffmpeg software decode of `path` as planar hashes, or `None` without ffmpeg.
fn ffmpeg_hashes(path: &Path, scratch: &Path, width: u32, height: u32) -> Option<Vec<String>> {
    std::fs::create_dir_all(scratch).ok()?;
    let out = scratch.join("oracle.yuv");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:v:0")
        .arg("-vsync")
        .arg("0")
        .arg("-c:v")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-f")
        .arg("rawvideo")
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    std::fs::remove_dir_all(scratch).ok();
    let stride = width as usize * height as usize;
    if stride == 0 || bytes.len() % (stride * 3 / 2) != 0 {
        return None;
    }
    let frame = stride * 3 / 2;
    Some(
        bytes
            .chunks_exact(frame)
            .map(|plane| FrameHash::of(plane).hex())
            .collect(),
    )
}

/// NV12 frame bytes as planar YUV420P.
fn to_planar(nv12: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_len = width * height;
    let (y, uv) = nv12.split_at(y_len);
    let mut planar = Vec::with_capacity(nv12.len());
    planar.extend_from_slice(y);
    planar.extend(uv.iter().step_by(2).copied());
    planar.extend(uv.iter().skip(1).step_by(2).copied());
    planar
}

/// Decode one packet per sample.
fn decode_stream(
    decoder: &mut H264Decoder<'_>,
    stream: &mp4::Stream,
) -> Result<Vec<VaApiFrame>, VaApiError> {
    let mut frames = Vec::new();
    for sample in &stream.samples {
        frames.extend(decoder.decode(&Packet::new(Bytes::copy_from_slice(sample)))?);
    }
    frames.extend(decoder.flush()?);
    Ok(frames)
}

/// True when `RUST_VIDEO_ALLOW_SKIP=1` permits skipping.
fn allow_skip() -> bool {
    std::env::var("RUST_VIDEO_ALLOW_SKIP").is_ok_and(|value| value == "1")
}

/// Skip over missing `what`.
///
/// # Panics
///
/// Panics unless `RUST_VIDEO_ALLOW_SKIP=1` is set.
fn skip(what: &str) {
    if allow_skip() {
        eprintln!("skipped: missing {what}");
    } else {
        panic!("missing {what}; rerun with RUST_VIDEO_ALLOW_SKIP=1 to skip");
    }
}

/// Device, or `None` when the tier declines the machine and skips are allowed.
fn device() -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => {
            eprintln!("va-api device: {}", device.name());
            Some(device)
        }
        Err(VaApiError::Skipped(reason)) => {
            skip(&format!("va-api tier ({reason})"));
            None
        }
        Err(error) => panic!("unexpected va-api failure: {error}"),
    }
}

#[test]
fn demux_matches_ffmpeg_remux() {
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = mp4::demux(&path).expect("demux synth high");
    assert_eq!(stream.samples.len(), 60);
    let first = nal_units(&stream.samples[0]);
    assert!(first.iter().any(|nal| nal.first() == Some(&0x67)));
    let Some(remux) = ffmpeg_remux(&path, &scratch()) else {
        skip("ffmpeg remux oracle (install ffmpeg)");
        return;
    };
    let blob = annex_b(&stream);
    let ours = nal_units(&blob);
    let theirs = nal_units(&remux);
    assert_eq!(ours.len(), theirs.len(), "nal unit count");
    for (index, (left, right)) in ours.iter().zip(theirs.iter()).enumerate() {
        assert_eq!(left, right, "nal unit {index}");
    }
}

#[test]
fn vaapi_decodes_synth_high() {
    let Some(device) = device() else {
        return;
    };
    let path = corpus().join("synth/h264/h264-high.mp4");
    let stream = mp4::demux(&path).expect("demux synth high");
    let mut decoder = H264Decoder::open(&device);
    let frames = decode_stream(&mut decoder, &stream).expect("va-api decode");
    assert_eq!(frames.len(), stream.samples.len());
    let mut hashes = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        let size = frame.identity().size();
        assert_eq!((size.width(), size.height()), (1920, 1080), "frame {index}");
        assert_eq!(frame.identity().format(), PixelFormat::Nv12);
        let nv12 = frame.nv12();
        assert_eq!(nv12.len(), 1920 * 1080 * 3 / 2, "frame {index}");
        hashes.push(FrameHash::of(&to_planar(nv12, 1920, 1080)).hex());
    }
    let Some(expected) = ffmpeg_hashes(&path, &scratch(), 1920, 1080) else {
        skip("ffmpeg decode oracle (install ffmpeg)");
        return;
    };
    assert_eq!(hashes.len(), expected.len(), "frame count");
    for (index, (left, right)) in hashes.iter().zip(expected.iter()).enumerate() {
        assert_eq!(left, right, "frame {index}");
    }
}

#[test]
fn vaapi_declines_high_chroma() {
    let Some(device) = device() else {
        return;
    };
    for name in ["h264-high422.mp4", "h264-high444.mp4"] {
        let path = corpus().join("synth/h264").join(name);
        let stream = mp4::demux(&path).expect("demux chroma vector");
        let mut decoder = H264Decoder::open(&device);
        match decode_stream(&mut decoder, &stream) {
            Err(VaApiError::Unsupported(_)) => {}
            Err(error) => panic!("{name}: expected unsupported chroma, got {error}"),
            Ok(_) => panic!("{name}: expected unsupported chroma, decoded"),
        }
    }
}
