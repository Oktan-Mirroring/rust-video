//! D3D11 end to end: adapter probe plus GPU transcode.
//!
//! Shared vector `valid_320x240.h264` holds 15 ffmpeg
//! testsrc2 pictures (320x240 constrained baseline, IDR at 0 and
//! 8); harness `AnnexB` splits it per picture.

#![cfg(target_os = "windows")]

use std::path::PathBuf;

use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, H264Decoder, HevcDecoder,
    Mpeg2Decoder, Transcoder, Vc1Decoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_harness::{decode_dir, AnnexB, Synth};

/// Pictures in the committed vector.
const PICTURES: usize = 15;

/// Open device, loud skip without hardware.
fn device() -> Option<D3d11Device> {
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

/// Shared vector path.
fn vector() -> PathBuf {
    decode_dir().join("valid_320x240.h264")
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

/// Adapter matrix plus shell and encoder contracts, skipped without hardware.
#[test]
fn probe_reports_or_skips() {
    let start = std::time::Instant::now();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    println!(
        "adapter: {} (feature level {:#x}, open in {:?})",
        device.name(),
        device.feature_level(),
        start.elapsed()
    );
    for codec in Codec::ALL {
        println!("supports {codec}: {}", device.supports(*codec));
    }
    for profile in device.profiles() {
        println!("profile: {} ({})", profile.name(), profile.codec());
    }
    assert!(device.supports(Codec::H264));
    assert_ne!(device.profiles(), []);

    let packet = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x41, 0xC5, 0x00]));
    let mut h264 = H264Decoder::open(&device);
    assert!(h264.decode(&packet).is_err());
    assert!(h264.flush().expect("flush").is_empty());

    let mut hevc = HevcDecoder::open(&device);
    let hevc_bad = Packet::new(Bytes::from_static(&[0, 0, 0, 1, 0x40, 0x01]));
    assert!(hevc.decode(&hevc_bad).is_err());
    assert!(hevc.flush().expect("flush").is_empty());
    let mut vp9 = Vp9Decoder::open(&device);
    assert!(vp9.decode(&packet).is_err());
    assert!(vp9.flush().expect("flush").is_empty());
    let mut av1 = Av1Decoder::open(&device);
    let av1_bad = Packet::new(Bytes::from_static(&[0xFF]));
    assert!(av1.decode(&av1_bad).is_err());
    assert!(av1.flush().expect("flush").is_empty());
    let mut mpeg2 = Mpeg2Decoder::open(&device);
    assert!(mpeg2.decode(&packet).is_err());
    assert!(mpeg2.flush().expect("flush").is_empty());
    let mut vc1_decoder = Vc1Decoder::open(&device);
    assert!(vc1_decoder.decode(&packet).is_err());
    assert!(vc1_decoder.flush().expect("flush").is_empty());

    drive_encoder(&device, Codec::H264, 1280, 720, true);
    drive_encoder(&device, Codec::H264, 64, 48, true);
    for codec in [
        Codec::H265,
        Codec::Av1,
        Codec::Vp9,
        Codec::Mpeg2,
        Codec::Vc1,
    ] {
        drive_encoder(&device, codec, 64, 48, false);
    }
}

/// Eight frames plus a drain through `codec`, or its loud fallback.
fn drive_encoder(device: &D3d11Device, codec: Codec, width: u32, height: u32, required: bool) {
    let mut encoder = match D3d11Encoder::open(device, codec) {
        Ok(encoder) => encoder,
        Err(error) => return fallback(codec, required, &error),
    };
    assert_eq!(encoder.codec(), codec);
    let frame = Synth::grey_nv12(width, height);
    let start = std::time::Instant::now();
    let mut count = 0usize;
    let mut bytes = 0usize;
    for _ in 0..8 {
        let packets = match encoder.encode(&frame) {
            Ok(packets) => packets,
            Err(error) => return fallback(codec, required, &error),
        };
        count += packets.len();
        bytes += packets
            .iter()
            .map(|packet| packet.data().len())
            .sum::<usize>();
    }
    let drained = match encoder.flush() {
        Ok(drained) => drained,
        Err(error) => return fallback(codec, required, &error),
    };
    count += drained.len();
    bytes += drained
        .iter()
        .map(|packet| packet.data().len())
        .sum::<usize>();
    println!(
        "{codec} encode: {count} packets, {bytes} bytes via {}, d3d11: {} in {:?}",
        encoder.mft_name(),
        encoder.uses_d3d11(),
        start.elapsed()
    );
    assert!(bytes > 0, "{codec} encoder emitted no bytes");
}

/// Loud fallback for a `codec` the machine cannot encode.
fn fallback(codec: Codec, required: bool, error: &D3d11Error) {
    println!("{codec} encode unavailable: {error}");
    assert!(!required, "{codec} encoder required: {error}");
    assert!(
        matches!(error, D3d11Error::Unsupported(_)),
        "loud fallback: {error}"
    );
    assert!(
        error.to_string().contains("encoder MFT"),
        "loud fallback: {error}"
    );
}

/// GPU transcode output decodes clean under ffmpeg.
#[test]
fn gpu_transcode_stays_ffmpeg_clean() {
    let Some(device) = device() else {
        return;
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("d3d11 skipped: no h264 transcode ({error})");
            return;
        }
    };
    let raws = AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl);
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
    let path = std::env::temp_dir().join("rust-video-d3d11-e2e.h264");
    std::fs::write(&path, &out).expect("oracle input writable");
    ffmpeg_clean(&path);
    std::fs::remove_file(&path).ok();
    println!("transcode: {coded_packets} packets, {coded_bytes} bytes, route=gpu, ffmpeg-clean");
}
