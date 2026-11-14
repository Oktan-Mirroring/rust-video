//! VA-API throughput: 1080p decode and encode against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#[cfg(target_os = "linux")]
use bytes::Bytes;
#[cfg(target_os = "linux")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "linux")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "linux")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::{Decoder, Encoder};
#[cfg(target_os = "linux")]
use rust_video_harness::{bench_dir, AnnexB, H264Synth, Synth};
#[cfg(target_os = "linux")]
use rust_video_vaapi::{
    Av1Decoder, EncodeCodec, H264Decoder, H265Decoder, VaApiDevice, VaApiEncoder, VaApiError,
    VaApiFrame,
};

#[cfg(target_os = "linux")]
/// Tier realtime bar in frames per second.
const BAR: f64 = 60.0;

#[cfg(target_os = "linux")]
/// Timed iterations per bench.
const ITERS: u32 = 30;

#[cfg(target_os = "linux")]
/// Measured fps of `ITERS` passes of `packets` through `decoder`.
fn decode_fps(
    decoder: &mut impl Decoder<Frame = VaApiFrame, Error = VaApiError>,
    packets: &[Packet],
) -> f64 {
    for packet in packets {
        decoder.decode(packet).expect("warmup decode");
    }
    let mut count = 0_u32;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in packets {
            let frames = decoder.decode(packet).expect("timed decode");
            count += u32::try_from(frames.len()).expect("frame count fits");
        }
    }
    let drained = decoder.flush().expect("timed flush");
    count += u32::try_from(drained.len()).expect("drain count fits");
    let elapsed = start.elapsed().as_secs_f64();
    assert!(count > 0, "bench decoded no frames");
    f64::from(count) / elapsed
}

#[cfg(target_os = "linux")]
/// Measured fps of `ITERS` passes of `frames` through `encoder`.
fn encode_fps(encoder: &mut impl Encoder<Error = VaApiError>, frames: &[CpuFrame]) -> f64 {
    for frame in frames {
        assert!(
            !encoder.encode(frame).expect("warmup encode").is_empty(),
            "warmup frame codes"
        );
    }
    let mut coded = 0_u32;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for frame in frames {
            assert!(
                !encoder.encode(frame).expect("timed encode").is_empty(),
                "every frame codes"
            );
            coded += 1;
        }
    }
    encoder.flush().expect("timed flush");
    let elapsed = start.elapsed().as_secs_f64();
    f64::from(coded) / elapsed
}

#[cfg(target_os = "linux")]
/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

#[cfg(target_os = "linux")]
/// VA-API AV1 1080p decode measured against the realtime bar.
#[test]
fn av1_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vaapi av1 decode 1080p skipped: {error}");
            return;
        }
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = packets(&[bytes]);
    let mut decoder = Av1Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi av1 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "linux")]
/// VA-API H.264 1080p decode measured against the realtime bar.
#[test]
fn h264_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vaapi h264 decode 1080p skipped: {error}");
            return;
        }
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let packets = packets(&raws);
    let mut decoder = H264Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi h264 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "linux")]
/// VA-API H.264 1080p encode measured against the realtime bar.
#[test]
fn h264_encode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vaapi h264 encode 1080p skipped: {error}");
            return;
        }
    };
    let size = PictureSize::new(1920, 1080).expect("1080p extent");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H264, size) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("vaapi h264 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = [
        Synth::noise(1920, 1080, 1),
        Synth::noise(1920, 1080, 2),
        Synth::noise(1920, 1080, 3),
        Synth::noise(1920, 1080, 4),
    ];
    let measured = encode_fps(&mut encoder, &frames);
    println!("vaapi h264 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "linux")]
/// VA-API H.265 1080p decode measured against the realtime bar.
#[test]
fn h265_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vaapi h265 decode 1080p skipped: {error}");
            return;
        }
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = packets(&raws);
    let mut decoder = H265Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi h265 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "linux")]
/// VA-API H.265 1080p encode measured against the realtime bar.
#[test]
fn h265_encode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("vaapi h265 encode 1080p skipped: {error}");
            return;
        }
    };
    let size = PictureSize::new(1920, 1080).expect("1080p extent");
    let mut encoder = match VaApiEncoder::open(&device, EncodeCodec::H265, size) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("vaapi h265 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = [
        Synth::noise(1920, 1080, 1),
        Synth::noise(1920, 1080, 2),
        Synth::noise(1920, 1080, 3),
        Synth::noise(1920, 1080, 4),
    ];
    let measured = encode_fps(&mut encoder, &frames);
    println!("vaapi h265 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(not(target_os = "linux"))]
/// VA-API tier absent off Linux; the benches compile out.
#[test]
fn throughput_skipped_off_linux() {
    println!("vaapi throughput skipped: linux-only tier");
}
