//! VA-API throughput: 1080p decode and encode against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#![cfg(target_os = "linux")]

use rust_video_caps::common::PictureSize;
use rust_video_caps::encode::{EncodeConfig, Level, Profile, RateControl};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_harness::{bench_dir, AnnexB, Packets, Synth};
use rust_video_vaapi::{
    Av1Decoder, H264Decoder, H265Decoder, VaApiDevice, VaApiEncoder, VaApiError, VaApiFrame,
};

/// Tier realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Timed iterations per bench.
const ITERS: u32 = 30;

/// Surface decode behind one benchmark pass.
trait SurfaceDecode {
    /// Surface frames decoded from one packet.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot decode `packet`.
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError>;

    /// Trailing surface frames.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the backend cannot release its held frames.
    fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError>;
}

impl SurfaceDecode for Av1Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        Av1Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        Av1Decoder::flush_surface(self)
    }
}

impl SurfaceDecode for H264Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        H264Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        H264Decoder::flush_surface(self)
    }
}

impl SurfaceDecode for H265Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<VaApiFrame>, VaApiError> {
        H265Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<VaApiFrame>, VaApiError> {
        H265Decoder::flush_surface(self)
    }
}

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

/// Measured fps of `ITERS` passes of `packets` through the surface path.
fn surface_fps(decoder: &mut impl SurfaceDecode, packets: &[Packet]) -> f64 {
    for packet in packets {
        decoder.decode_surface(packet).expect("warmup decode");
    }
    let mut count = 0_u32;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for packet in packets {
            let frames = decoder.decode_surface(packet).expect("timed decode");
            count += u32::try_from(frames.len()).expect("frame count fits");
        }
    }
    let drained = decoder.flush_surface().expect("timed flush");
    count += u32::try_from(drained.len()).expect("drain count fits");
    let elapsed = start.elapsed().as_secs_f64();
    assert!(count > 0, "bench decoded no frames");
    f64::from(count) / elapsed
}

/// Measured fps of `ITERS` passes of `frames` through `encoder`.
fn encode_fps(encoder: &mut impl Encoder<Error = VaApiError>, frames: &[CpuFrame]) -> f64 {
    let mut warm = 0;
    for frame in frames {
        warm += encoder.encode(frame).expect("warmup encode").len();
    }
    warm += encoder.flush().expect("warmup flush").len();
    assert_eq!(warm, frames.len(), "warmup frames code");
    let mut coded = 0_u32;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        for frame in frames {
            coded += u32::try_from(encoder.encode(frame).expect("timed encode").len())
                .expect("packet count fits");
        }
    }
    coded += u32::try_from(encoder.flush().expect("timed flush").len()).expect("drain fits");
    assert_eq!(
        coded,
        ITERS * u32::try_from(frames.len()).expect("frame count fits"),
        "every frame codes"
    );
    let elapsed = start.elapsed().as_secs_f64();
    f64::from(coded) / elapsed
}

/// VA-API AV1 1080p decode measured against the realtime bar.
#[test]
fn av1_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi av1 decode 1080p: {error}");
            return;
        }
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = Packets::from_raws(&[bytes]);
    let mut decoder = Av1Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi av1 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// VA-API H.264 1080p decode measured against the realtime bar.
#[test]
fn h264_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h264 decode 1080p: {error}");
            return;
        }
    };
    let raws =
        vec![std::fs::read(bench_dir().join("h264-1080p-synth.h264")).expect("vector readable")];
    let packets = Packets::from_raws(&raws);
    let mut decoder = H264Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi h264 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// VA-API H.264 1080p encode measured against the realtime bar.
#[test]
fn h264_encode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h264 encode 1080p: {error}");
            return;
        }
    };
    let size = PictureSize::new(1920, 1080).expect("1080p extent");
    let config = EncodeConfig {
        rate_control: RateControl::Auto,
        profile: Profile::H264(rust_video_caps::h264::EncodeProfile::High),
        level: Level::H264(rust_video_caps::h264::EncodeLevel::Auto),
        ..EncodeConfig::default()
    };
    let mut encoder = match VaApiEncoder::open(&device, size, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP vaapi h264 encode 1080p: {error}");
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

/// VA-API H.265 1080p decode measured against the realtime bar.
#[test]
fn h265_decode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h265 decode 1080p: {error}");
            return;
        }
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = Packets::from_raws(&raws);
    let mut decoder = H265Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &packets);
    println!("vaapi h265 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// VA-API H.265 1080p encode measured against the realtime bar.
#[test]
fn h265_encode_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h265 encode 1080p: {error}");
            return;
        }
    };
    let size = PictureSize::new(1920, 1080).expect("1080p extent");
    let config = EncodeConfig {
        rate_control: RateControl::Auto,
        profile: Profile::H265(rust_video_caps::h265::EncodeProfile::Main),
        level: Level::H265(rust_video_caps::h265::EncodeLevel::Auto),
        ..EncodeConfig::default()
    };
    let mut encoder = match VaApiEncoder::open(&device, size, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP vaapi h265 encode 1080p: {error}");
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

/// VA-API AV1 1080p surface decode measured against the realtime bar.
#[test]
fn av1_decode_surface_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi av1 surface decode 1080p: {error}");
            return;
        }
    };
    let bytes = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("bench obu readable");
    let packets = Packets::from_raws(&[bytes]);
    let mut decoder = Av1Decoder::open(&device);
    let measured = surface_fps(&mut decoder, &packets);
    println!(
        "vaapi av1 surface decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// VA-API H.264 1080p surface decode measured against the realtime bar.
#[test]
fn h264_decode_surface_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h264 surface decode 1080p: {error}");
            return;
        }
    };
    let raws =
        vec![std::fs::read(bench_dir().join("h264-1080p-synth.h264")).expect("vector readable")];
    let packets = Packets::from_raws(&raws);
    let mut decoder = H264Decoder::open(&device);
    let measured = surface_fps(&mut decoder, &packets);
    println!(
        "vaapi h264 surface decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// VA-API H.265 1080p surface decode measured against the realtime bar.
#[test]
fn h265_decode_surface_1080p_measures_realtime() {
    let device = match VaApiDevice::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP vaapi h265 surface decode 1080p: {error}");
            return;
        }
    };
    let raws = AnnexB::picture_packets(
        &bench_dir().join("h265-1080p-nvenc.h265"),
        AnnexB::is_hevc_vcl,
    );
    let packets = Packets::from_raws(&raws);
    let mut decoder = H265Decoder::open(&device);
    let measured = surface_fps(&mut decoder, &packets);
    println!(
        "vaapi h265 surface decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
