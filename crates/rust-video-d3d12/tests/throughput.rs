//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#![cfg(target_os = "windows")]

use rust_video_d3d12::{Codec, D3d12Device, Transcoder};

use rust_video_caps::delivery::{PhaseTiming, ReceiveError, ReceiveFrames};
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

/// Warmup passes over the warmup packets: first submits per slot cost
/// milliseconds, and fewer passes than slots leaves slow hops inside
/// the timed window.
const WARMUP_PASSES: usize = 4;

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

/// Median of `values`, zero when empty.
fn median(mut values: Vec<u64>) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

/// Whole microseconds behind `elapsed`, saturating.
fn micros(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// Mean of `values`, zero when empty.
fn mean(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.iter().sum::<u64>() / u64::try_from(values.len()).unwrap_or(1)
}

/// Phase medians behind `name` over `timings`, counted in `unit`.
fn print_phase_medians(name: &str, unit: &str, timings: &[PhaseTiming]) {
    println!(
        "{name} phases: submit {}us engine {}us copy {}us latency {}us over {} {unit}",
        median(timings.iter().map(PhaseTiming::submit_us).collect()),
        median(timings.iter().map(PhaseTiming::engine_wait_us).collect()),
        median(timings.iter().map(PhaseTiming::copy_us).collect()),
        median(timings.iter().map(PhaseTiming::latency_us).collect()),
        timings.len()
    );
}

/// Median of `values` behind `name`, counted in `unit`.
fn print_median(name: &str, unit: &str, values: &[std::time::Duration]) {
    println!(
        "{name}: median {}us over {} {unit}",
        median(values.iter().copied().map(micros).collect()),
        values.len()
    );
}

/// Untimed prime of `pump`: session build plus slot first-touches land
/// here on the stream head, and the pipe drains best-effort, so the timed
/// drive that follows starts hot.
fn prime<P>(pump: &mut P, packets: &[Packet])
where
    P: rust_video_caps::delivery::SendPackets<Error = D3d12Error>
        + rust_video_caps::delivery::PollFrames<Frame = D3d12Frame, Error = D3d12Error>
        + rust_video_caps::delivery::ReceiveFrames<Frame = D3d12Frame, Error = D3d12Error>,
{
    let prime = &packets[..WARMUP_PASSES.min(packets.len())];
    let started = std::time::Instant::now();
    let (mut frames, _) = Drive::send_all(pump, prime, drain);
    loop {
        match pump.receive_timeout(std::time::Duration::from_millis(100)) {
            Ok(Some(frame)) => frames.push(frame),
            Ok(None) | Err(ReceiveError::Timeout) => break,
            Err(error) => panic!("prime receive: {error:?}"),
        }
    }
    println!(
        "prime: {} frames landed untimed in {}us",
        frames.len(),
        micros(started.elapsed())
    );
    assert!(!frames.is_empty(), "prime lands");
}

/// Measured fps of `HOPS` 1080p hops through `transcoder`.
fn fps(transcoder: &mut Transcoder, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> f64 {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for _ in 0..WARMUP_PASSES {
        for packet in &warmup {
            transcoder.transcode(packet).expect("warmup transcode");
            assert!(matches!(transcoder.plan().route(), Route::Gpu));
        }
    }
    let mut bytes = 0;
    let mut hops_us = Vec::new();
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            let hop = std::time::Instant::now();
            let coded = transcoder.transcode(packet).expect("timed transcode");
            hops_us.push(micros(hop.elapsed()));
            assert!(matches!(transcoder.plan().route(), Route::Gpu));
            bytes += coded
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
    }
    let flushed = std::time::Instant::now();
    transcoder.flush().expect("timed flush");
    let flush_us = micros(flushed.elapsed());
    assert!(matches!(transcoder.plan().route(), Route::Gpu));
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    println!(
        "xcode loop: hop median {}us max {}us over {} hops, flush {}us",
        median(hops_us.clone()),
        hops_us.iter().max().copied().unwrap_or(0),
        hops_us.len(),
        flush_us
    );
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
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let warmup = raws.clone();
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
    for _ in 0..WARMUP_PASSES {
        for packet in &warmup {
            decoder.decode(packet)?;
        }
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
    for _ in 0..WARMUP_PASSES {
        for packet in &warmup {
            for surface in decode(decoder, packet)? {
                surface.try_wait()?;
            }
        }
    }
    let mut surfaces = 0u32;
    let mut calls_us = Vec::new();
    let mut waits_us = Vec::new();
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            let call = std::time::Instant::now();
            let batch = decode(decoder, packet).expect("timed decode");
            calls_us.push(micros(call.elapsed()));
            for surface in batch {
                let wait = std::time::Instant::now();
                surface.try_wait().expect("surface completes");
                waits_us.push(micros(wait.elapsed()));
                surfaces += 1;
            }
        }
    }
    for surface in flush(decoder).expect("timed drain") {
        surface.try_wait().expect("surface completes");
        surfaces += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(surfaces > 0);
    println!(
        "surface loop: decode_call median {}us max {}us, try_wait median {}us max {}us over {} calls",
        median(calls_us.clone()),
        calls_us.iter().max().copied().unwrap_or(0),
        median(waits_us.clone()),
        waits_us.iter().max().copied().unwrap_or(0),
        calls_us.len()
    );
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
    let timings = decoder.sync_timings();
    println!("d3d12 h264 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 h264 sync", "pictures", timings);
    print_median(
        "d3d12 h264 sync readback",
        "frames",
        decoder.sync_readback(),
    );
    print_median("d3d12 h264 sync emit", "frames", decoder.sync_emit());
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
    let timings = decoder.sync_timings();
    println!("d3d12 hevc decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 hevc sync", "pictures", timings);
    print_median(
        "d3d12 hevc sync readback",
        "frames",
        decoder.sync_readback(),
    );
    print_median("d3d12 hevc sync emit", "frames", decoder.sync_emit());
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
    let timings = decoder.sync_timings();
    println!("d3d12 av1 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 av1 sync", "pictures", timings);
    print_median("d3d12 av1 sync readback", "frames", decoder.sync_readback());
    print_median("d3d12 av1 sync emit", "frames", decoder.sync_emit());
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
    let config = D3d12Encoder::legacy_config(Codec::H264).expect("legacy h264");
    let mut encoder = match D3d12Encoder::open(&device, config) {
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
    let config = D3d12Encoder::legacy_config(Codec::H265).expect("legacy hevc");
    let mut encoder = match D3d12Encoder::open(&device, config) {
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
    let timings = decoder.surface_timings();
    println!("d3d12 h264 surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 h264 surface", "pictures", timings);
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
    let timings = decoder.surface_timings();
    println!("d3d12 hevc surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 hevc surface", "pictures", timings);
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
    let timings = decoder.surface_timings();
    println!("d3d12 av1 surface 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    print_phase_medians("d3d12 av1 surface", "pictures", timings);
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
    prime(&mut pump, &Packets::from_raws(&raws));
    let mut drain_us = 0u64;
    let mut drains = 0u64;
    let (measured, count) =
        Drive::sustained(&mut pump, &Packets::from_raws(&raws), |pump, frames| {
            let started = std::time::Instant::now();
            drain(pump, frames);
            drain_us += micros(started.elapsed());
            drains += 1;
        });
    let depth = pump.max_depth();
    assert!(
        count >= raws.len(),
        "every packet yields its frames: {count} over {}",
        raws.len()
    );
    assert!(
        count <= raws.len() + 2 * WARMUP_PASSES,
        "leftovers within prime work: {count} over {}",
        raws.len()
    );
    let wall = f64::from(u32::try_from(count).expect("frame count fits")) / measured;
    let measured = f64::from(u32::try_from(raws.len()).expect("packet count fits")) / wall;
    let timings = pump.timings();
    let latencies: Vec<u64> = timings.iter().map(PhaseTiming::latency_us).collect();
    let (quiesce_waits, quiesce_time) = pump.quiesce_stats();
    println!("d3d12 h264 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_H264_BAR:.0}fps");
    print_median(
        "d3d12 h264 sustained worker copy",
        "frames",
        pump.copy_times(),
    );
    print_phase_medians("d3d12 h264 sustained", "frames", timings);
    println!(
        "d3d12 h264 sustained quiesce: {quiesce_waits} waits {}us, latency mean {}us max {}us",
        micros(quiesce_time),
        mean(&latencies),
        latencies.iter().max().copied().unwrap_or(0)
    );
    let (send_cpu, land_cpu, drain_time) = pump.cpu_totals();
    let (ingest_cpu, pictures_cpu, harvest_cpu, downloads_cpu) = pump.send_split();
    let (sequence_cpu, prepare_cpu, track_video_cpu, track_misc_cpu) = pump.front_split();
    println!(
        "d3d12 h264 sustained thread: send {}us (ingest {} pictures {} harvest {} downloads {} sequence {} prepare {} trackvideo {} trackmisc {}) land {}us drain {}us over {count} frames",
        micros(send_cpu),
        micros(ingest_cpu),
        micros(pictures_cpu),
        micros(harvest_cpu),
        micros(downloads_cpu),
        micros(sequence_cpu),
        micros(prepare_cpu),
        micros(track_video_cpu),
        micros(track_misc_cpu),
        micros(land_cpu),
        micros(drain_time)
    );
    print_median(
        "d3d12 h264 sustained engine gaps",
        "signals",
        pump.engine_gaps(),
    );
    print_median(
        "d3d12 h264 sustained copy gaps",
        "signals",
        pump.copy_gaps(),
    );
    let (bound_hits, picture_count) = pump.drive_stats();
    println!(
        "d3d12 h264 sustained drive: {bound_hits} bound hits, {picture_count} pictures, {drains} drains {drain_us}us"
    );
    assert!(
        measured >= SUSTAINED_H264_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_H264_BAR:.0}fps"
    );
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
    prime(&mut pump, &Packets::from_raws(&raws));
    let mut drain_us = 0u64;
    let mut drains = 0u64;
    let (measured, count) =
        Drive::sustained(&mut pump, &Packets::from_raws(&raws), |pump, frames| {
            let started = std::time::Instant::now();
            drain(pump, frames);
            drain_us += micros(started.elapsed());
            drains += 1;
        });
    let depth = pump.max_depth();
    assert!(
        count >= raws.len(),
        "every packet yields its frames: {count} over {}",
        raws.len()
    );
    assert!(
        count <= raws.len() + 2 * WARMUP_PASSES,
        "leftovers within prime work: {count} over {}",
        raws.len()
    );
    let wall = f64::from(u32::try_from(count).expect("frame count fits")) / measured;
    let measured = f64::from(u32::try_from(raws.len()).expect("packet count fits")) / wall;
    let timings = pump.timings();
    let latencies: Vec<u64> = timings.iter().map(PhaseTiming::latency_us).collect();
    let (quiesce_waits, quiesce_time) = pump.quiesce_stats();
    println!("d3d12 hevc sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_HEVC_BAR:.0}fps");
    print_median(
        "d3d12 hevc sustained worker copy",
        "frames",
        pump.copy_times(),
    );
    print_phase_medians("d3d12 hevc sustained", "frames", timings);
    println!(
        "d3d12 hevc sustained quiesce: {quiesce_waits} waits {}us, latency mean {}us max {}us",
        micros(quiesce_time),
        mean(&latencies),
        latencies.iter().max().copied().unwrap_or(0)
    );
    let (send_cpu, land_cpu, drain_time) = pump.cpu_totals();
    let (ingest_cpu, pictures_cpu, harvest_cpu, downloads_cpu) = pump.send_split();
    let (sequence_cpu, prepare_cpu, track_video_cpu, track_misc_cpu) = pump.front_split();
    println!(
        "d3d12 hevc sustained thread: send {}us (ingest {} pictures {} harvest {} downloads {} sequence {} prepare {} trackvideo {} trackmisc {}) land {}us drain {}us over {count} frames",
        micros(send_cpu),
        micros(ingest_cpu),
        micros(pictures_cpu),
        micros(harvest_cpu),
        micros(downloads_cpu),
        micros(sequence_cpu),
        micros(prepare_cpu),
        micros(track_video_cpu),
        micros(track_misc_cpu),
        micros(land_cpu),
        micros(drain_time)
    );
    print_median(
        "d3d12 hevc sustained engine gaps",
        "signals",
        pump.engine_gaps(),
    );
    print_median(
        "d3d12 hevc sustained copy gaps",
        "signals",
        pump.copy_gaps(),
    );
    let (bound_hits, picture_count) = pump.drive_stats();
    println!(
        "d3d12 hevc sustained drive: {bound_hits} bound hits, {picture_count} pictures, {drains} drains {drain_us}us"
    );
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
    prime(&mut pump, &Packets::from_raws(&raws));
    let mut drain_us = 0u64;
    let mut drains = 0u64;
    let (measured, count) =
        Drive::sustained(&mut pump, &Packets::from_raws(&raws), |pump, frames| {
            let started = std::time::Instant::now();
            drain(pump, frames);
            drain_us += micros(started.elapsed());
            drains += 1;
        });
    let depth = pump.max_depth();
    assert!(
        count >= raws.len(),
        "every packet yields its frames: {count} over {}",
        raws.len()
    );
    assert!(
        count <= raws.len() + 2 * WARMUP_PASSES,
        "leftovers within prime work: {count} over {}",
        raws.len()
    );
    let wall = f64::from(u32::try_from(count).expect("frame count fits")) / measured;
    let measured = f64::from(u32::try_from(raws.len()).expect("packet count fits")) / wall;
    let timings = pump.timings();
    let latencies: Vec<u64> = timings.iter().map(PhaseTiming::latency_us).collect();
    let (quiesce_waits, quiesce_time) = pump.quiesce_stats();
    println!("d3d12 av1 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_AV1_BAR:.0}fps");
    print_median(
        "d3d12 av1 sustained worker copy",
        "frames",
        pump.copy_times(),
    );
    print_phase_medians("d3d12 av1 sustained", "frames", timings);
    println!(
        "d3d12 av1 sustained quiesce: {quiesce_waits} waits {}us, latency mean {}us max {}us",
        micros(quiesce_time),
        mean(&latencies),
        latencies.iter().max().copied().unwrap_or(0)
    );
    let (send_cpu, land_cpu, drain_time) = pump.cpu_totals();
    let (ingest_cpu, pictures_cpu, harvest_cpu, downloads_cpu) = pump.send_split();
    let (sequence_cpu, prepare_cpu, track_video_cpu, track_misc_cpu) = pump.front_split();
    println!(
        "d3d12 av1 sustained thread: send {}us (ingest {} pictures {} harvest {} downloads {} sequence {} prepare {} trackvideo {} trackmisc {}) land {}us drain {}us over {count} frames",
        micros(send_cpu),
        micros(ingest_cpu),
        micros(pictures_cpu),
        micros(harvest_cpu),
        micros(downloads_cpu),
        micros(sequence_cpu),
        micros(prepare_cpu),
        micros(track_video_cpu),
        micros(track_misc_cpu),
        micros(land_cpu),
        micros(drain_time)
    );
    print_median(
        "d3d12 av1 sustained engine gaps",
        "signals",
        pump.engine_gaps(),
    );
    print_median("d3d12 av1 sustained copy gaps", "signals", pump.copy_gaps());
    let (bound_hits, picture_count) = pump.drive_stats();
    println!(
        "d3d12 av1 sustained drive: {bound_hits} bound hits, {picture_count} pictures, {drains} drains {drain_us}us"
    );
    assert!(
        measured >= SUSTAINED_AV1_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_AV1_BAR:.0}fps"
    );
}
