//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#![cfg(target_os = "windows")]

use rust_video_d3d12::{Codec, D3d12Device, Transcoder};

use rust_video_caps::delivery::ReceiveFrames;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_d3d12::{
    Av1Decoder, D3d12Encoder, D3d12Error, D3d12Frame, GpuSurface, H264Decoder, HevcDecoder,
    OverlappedAv1Pump, OverlappedH264Pump, OverlappedHevcPump,
};
use rust_video_harness::H264Synth;
use rust_video_harness::{bench_dir, AnnexB, Drive, Ivf, Packets, Synth};

/// Hops timed after warmup.
const HOPS: u32 = 30;

/// Tier1 realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Sustained overlap bar: quiet median minus 20% headroom, 2026-11-19.
const SUSTAINED_H264_BAR: f64 = 840.0;

/// Sustained HEVC overlap bar: quiet median minus 20% headroom, 2026-11-19.
const SUSTAINED_HEVC_BAR: f64 = 770.0;

/// Sustained AV1 overlap bar: quiet median minus 20% headroom, 2026-11-19.
const SUSTAINED_AV1_BAR: f64 = 580.0;

/// One in-flight frame into `frames`, freeing send room.
fn drain<P>(pump: &mut P, frames: &mut Vec<P::Frame>)
where
    P: ReceiveFrames,
    P::Error: std::fmt::Debug,
{
    frames.push(
        pump.receive()
            .expect("in-flight frame completes")
            .expect("input unfinished"),
    );
}

/// Measured fps of `HOPS` 1080p hops through `transcoder`.
fn fps(transcoder: &mut Transcoder, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> f64 {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for packet in &warmup {
        let outcome = transcoder.transcode(packet).expect("warmup transcode");
        assert!(matches!(outcome.plan().route(), Route::Gpu));
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            let outcome = transcoder.transcode(packet).expect("timed transcode");
            assert!(matches!(outcome.plan().route(), Route::Gpu));
            bytes += outcome
                .packets()
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
    }
    let outcome = transcoder.flush().expect("timed flush");
    assert!(matches!(outcome.plan().route(), Route::Gpu));
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(HOPS) / elapsed
}

/// 1080p GPU hops measured against the realtime bar.
#[test]
fn xcode_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("SKIP: no h264 transcode ({error})");
            return;
        }
    };
    let warmup: Vec<Vec<u8>> = H264Synth::annex_b_h264_packets()
        .into_iter()
        .chain(H264Synth::annex_b_h264_packets_cropped())
        .collect();
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = fps(&mut transcoder, &warmup, &raws);
    println!("xcode 1080p: {measured:.1}fps over {HOPS} hops, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// Measured fps of `HOPS` 1080p decodes through `decoder`.
fn decode_fps<D>(decoder: &mut D, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> Result<f64, D3d12Error>
where
    D: Decoder<Frame = D3d12Frame, Error = D3d12Error>,
{
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for packet in &warmup {
        decoder.decode(packet)?;
    }
    decoder.flush()?;
    let mut frames = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            frames += decoder.decode(packet).expect("timed decode").len();
        }
    }
    frames += decoder.flush().expect("timed drain").len();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    let total = u32::try_from(frames).expect("frame count fits");
    Ok(f64::from(total) / elapsed)
}

/// Measured fps of `HOPS` 1080p encodes through `encoder`.
fn encode_fps(
    encoder: &mut D3d12Encoder,
    warmup: &CpuFrame,
    frames: &[CpuFrame],
) -> Result<f64, D3d12Error> {
    encoder.encode(warmup)?;
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for frame in frames {
        for packet in encoder.encode(frame).expect("timed encode") {
            bytes += packet.data().len();
        }
    }
    for packet in encoder.flush().expect("timed drain") {
        bytes += packet.data().len();
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    Ok(f64::from(HOPS) / elapsed)
}

/// Measured fps of `HOPS` 1080p surface decodes through `decode` plus `flush`.
///
/// Each surface waits on its fence before drop, so the loop paces completed
/// GPU work rather than queued submits.
fn surface_fps<D>(
    decoder: &mut D,
    decode: fn(&mut D, &Packet) -> Result<Vec<GpuSurface>, D3d12Error>,
    flush: fn(&mut D) -> Result<Vec<GpuSurface>, D3d12Error>,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> Result<f64, D3d12Error> {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for packet in &warmup {
        for surface in decode(decoder, packet)? {
            surface.wait()?;
        }
    }
    for surface in flush(decoder)? {
        surface.wait()?;
    }
    let mut surfaces = 0u32;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            for surface in decode(decoder, packet).expect("timed decode") {
                surface.wait().expect("surface completes");
                surfaces += 1;
            }
        }
    }
    for surface in flush(decoder).expect("timed drain") {
        surface.wait().expect("surface completes");
        surfaces += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(surfaces > 0);
    Ok(f64::from(surfaces) / elapsed)
}

/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H264) {
        println!("SKIP: no h264 decoder profile");
        return;
    }
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = match decode_fps(&mut decoder, &raws, &raws) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 h264 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p HEVC decode measured against the realtime bar.
#[test]
fn decode_hevc_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H265) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let mut decoder = HevcDecoder::open(&device);
    let path = bench_dir().join("h265-1080p-nvenc.h265");
    let data = std::fs::read(&path).expect("vector readable");
    let grouped = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!grouped.is_empty(), "hevc vector groups to pictures");
    let first = grouped[..1].to_vec();
    let measured = match decode_fps(&mut decoder, &grouped, &first) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 hevc decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::Av1) {
        println!("SKIP: no av1 decoder profile");
        return;
    }
    let mut decoder = Av1Decoder::open(&device);
    let data = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let raws = vec![data];
    let measured = match decode_fps(&mut decoder, &raws, &raws) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 av1 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn encode_h264_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    let mut encoder = match D3d12Encoder::open(&device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no h264 encoder ({error})");
            return;
        }
    };
    let frames: Vec<CpuFrame> = (0..HOPS)
        .map(|seed| Synth::noise(1920, 1080, u64::from(seed)))
        .collect();
    let warmup = Synth::noise(1920, 1080, u64::from(HOPS));
    let measured = match encode_fps(&mut encoder, &warmup, &frames) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup encode: {error}"),
    };
    println!("d3d12 h264 encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p HEVC encode measured against the realtime bar.
#[test]
fn encode_hevc_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    let mut encoder = match D3d12Encoder::open(&device, Codec::H265) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("SKIP: no hevc encoder ({error})");
            return;
        }
    };
    let frames: Vec<CpuFrame> = (0..HOPS)
        .map(|seed| Synth::noise(1920, 1080, u64::from(seed)))
        .collect();
    let warmup = Synth::noise(1920, 1080, u64::from(HOPS));
    let measured = match encode_fps(&mut encoder, &warmup, &frames) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup encode: {error}"),
    };
    println!("d3d12 hevc encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 surface decode measured against the realtime bar.
#[test]
fn decode_h264_surface_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H264) {
        println!("SKIP: no h264 decoder profile");
        return;
    }
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = match surface_fps(
        &mut decoder,
        H264Decoder::decode_surface,
        H264Decoder::flush_surface,
        &raws,
        &raws,
    ) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 h264 surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p HEVC surface decode measured against the realtime bar.
#[test]
fn decode_hevc_surface_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H265) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let mut decoder = HevcDecoder::open(&device);
    let path = bench_dir().join("h265-1080p-nvenc.h265");
    let data = std::fs::read(&path).expect("vector readable");
    let grouped = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!grouped.is_empty(), "hevc vector groups to pictures");
    let first = grouped[..1].to_vec();
    let measured = match surface_fps(
        &mut decoder,
        HevcDecoder::decode_surface,
        HevcDecoder::flush_surface,
        &grouped,
        &first,
    ) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 hevc surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 surface decode measured against the realtime bar.
#[test]
fn decode_av1_surface_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::Av1) {
        println!("SKIP: no av1 decoder profile");
        return;
    }
    let mut decoder = Av1Decoder::open(&device);
    let data = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let raws = vec![data];
    let measured = match surface_fps(
        &mut decoder,
        Av1Decoder::decode_surface,
        Av1Decoder::flush_surface,
        &raws,
        &raws,
    ) {
        Ok(fps) => fps,
        Err(error)
            if matches!(
                error,
                D3d12Error::Skipped(_) | D3d12Error::Unsupported(_) | D3d12Error::OverCaps { .. }
            ) =>
        {
            println!("SKIP: {error}");
            return;
        }
        Err(error) => panic!("warmup decode: {error}"),
    };
    println!("d3d12 av1 surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// Sustained 1080p H.264 overlap throughput against its bar.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H264) {
        println!("SKIP: no h264 decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h264-1080p_ip.h264")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut pump = OverlappedH264Pump::open(&device, OverlappedH264Pump::MAX_INFLIGHT);
    let (measured, count) = Drive::sustained(&mut pump, &Packets::from_raws(&raws), drain);
    let depth = pump.max_depth();
    println!("d3d12 h264 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_H264_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_H264_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_H264_BAR:.0}fps"
    );
}

/// Sustained 1080p HEVC overlap throughput against its bar.
#[test]
fn sustained_hevc_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::H265) {
        println!("SKIP: no hevc decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h265-1080p_ip.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut pump = OverlappedHevcPump::open(&device, OverlappedHevcPump::MAX_INFLIGHT);
    let (measured, count) = Drive::sustained(&mut pump, &Packets::from_raws(&raws), drain);
    let depth = pump.max_depth();
    println!("d3d12 hevc sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_HEVC_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_HEVC_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_HEVC_BAR:.0}fps"
    );
}

/// Sustained 1080p AV1 overlap throughput against its bar.
#[test]
fn sustained_av1_1080p_measures_realtime() {
    let _guard = Drive::serial();
    let device = match D3d12Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            return;
        }
    };
    if !device.supports(Codec::Av1) {
        println!("SKIP: no av1 decoder profile");
        return;
    }
    let raws = Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let mut pump = OverlappedAv1Pump::open(&device, OverlappedAv1Pump::MAX_INFLIGHT);
    let (measured, count) = Drive::sustained(&mut pump, &Packets::from_raws(&raws), drain);
    let depth = pump.max_depth();
    println!("d3d12 av1 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_AV1_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_AV1_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_AV1_BAR:.0}fps"
    );
}
