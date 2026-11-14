//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#[cfg(target_os = "windows")]
use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, D3d11Frame, H264Decoder, HevcDecoder,
    Transcoder,
};

#[cfg(target_os = "windows")]
use bytes::Bytes;
#[cfg(target_os = "windows")]
use rust_video_caps::frame::CpuFrame;
#[cfg(target_os = "windows")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "windows")]
use rust_video_caps::traits::{Decoder, Encoder};
#[cfg(target_os = "windows")]
use rust_video_caps::transcode::Route;
#[cfg(target_os = "windows")]
use rust_video_harness::{bench_dir, AnnexB, H264Synth, Synth};

#[cfg(target_os = "windows")]
/// Hops timed after warmup.
const HOPS: u32 = 30;

#[cfg(target_os = "windows")]
/// Tier1 realtime bar in frames per second.
const BAR: f64 = 60.0;

#[cfg(target_os = "windows")]
/// Warmup frames ahead of the timed loop.
const WARMUP: u64 = 4;

#[cfg(target_os = "windows")]
/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

#[cfg(target_os = "windows")]
/// Measured fps of `HOPS` 1080p hops through `transcoder`.
fn fps(transcoder: &mut Transcoder, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> f64 {
    let warmup = packets(warmup);
    let raws = packets(raws);
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

#[cfg(target_os = "windows")]
/// Measured fps decoding `HOPS` passes over `raws` after `warmup`.
fn decode_fps(
    decoder: &mut impl Decoder<Frame = D3d11Frame, Error = D3d11Error>,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> f64 {
    let warmup = packets(warmup);
    let raws = packets(raws);
    for packet in &warmup {
        decoder.decode(packet).expect("warmup decode");
    }
    let mut frames = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            frames += decoder.decode(packet).expect("timed decode").len();
        }
    }
    frames += decoder.flush().expect("timed flush").len();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    let total = u32::try_from(frames).expect("frame count fits");
    f64::from(total) / elapsed
}

#[cfg(target_os = "windows")]
/// Measured fps encoding `frames` after `warmup`.
fn encode_fps(encoder: &mut D3d11Encoder, warmup: &[CpuFrame], frames: &[CpuFrame]) -> f64 {
    for frame in warmup {
        encoder.encode(frame).expect("warmup encode");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for frame in frames {
        for packet in encoder.encode(frame).expect("timed encode") {
            bytes += packet.data().len();
        }
    }
    for packet in encoder.flush().expect("timed flush") {
        bytes += packet.data().len();
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(HOPS) / elapsed
}

#[cfg(target_os = "windows")]
/// Noise 1080p frame from `seed`.
fn noise(seed: u64) -> CpuFrame {
    Synth::noise(1920, 1080, seed)
}

#[cfg(target_os = "windows")]
/// Warmup plus timed noise 1080p frames on distinct seeds.
fn noise_frames() -> (Vec<CpuFrame>, Vec<CpuFrame>) {
    let warmup = (0..WARMUP).map(noise).collect();
    let timed = (WARMUP..WARMUP + u64::from(HOPS)).map(noise).collect();
    (warmup, timed)
}

#[cfg(target_os = "windows")]
/// 1080p GPU hops measured against the realtime bar.
#[test]
fn xcode_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let small = packets(&H264Synth::annex_b_packets());
    let mut decoder = H264Decoder::open(&device);
    let mut small_frames = 0;
    for packet in &small {
        small_frames += decoder.decode(packet).expect("warmup decode").len();
    }
    small_frames += decoder.flush().expect("warmup drain").len();
    assert!(small_frames > 0, "small vector decodes");
    let mut transcoder = match Transcoder::open(&device, Codec::H264, Codec::H264) {
        Ok(transcoder) => transcoder,
        Err(error) => {
            println!("d3d11 skipped: {error}");
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

#[cfg(target_os = "windows")]
/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = decode_fps(&mut decoder, &raws, &raws);
    println!("d3d11 h264 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p HEVC decode measured against the realtime bar.
#[test]
fn decode_hevc_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::Hevc) {
        println!("d3d11 skipped: no hevc decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut decoder = HevcDecoder::open(&device);
    let measured = decode_fps(&mut decoder, &raws, &raws);
    println!("d3d11 hevc decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::Av1) {
        println!("d3d11 skipped: no av1 decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let raws = vec![data];
    let mut decoder = Av1Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &raws, &raws);
    println!("d3d11 av1 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn encode_h264_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, Codec::H264) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no h264 encoder MFT ({error})");
            return;
        }
    };
    let (warmup, frames) = noise_frames();
    let measured = encode_fps(&mut encoder, &warmup, &frames);
    println!("d3d11 h264 encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p HEVC encode measured against the realtime bar.
#[test]
fn encode_hevc_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, Codec::Hevc) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no hevc encoder MFT ({error})");
            return;
        }
    };
    let (warmup, frames) = noise_frames();
    let measured = encode_fps(&mut encoder, &warmup, &frames);
    println!("d3d11 hevc encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p VP9 encode measured against the realtime bar.
#[test]
fn encode_vp9_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, Codec::Vp9) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no vp9 encoder MFT ({error})");
            return;
        }
    };
    let (warmup, frames) = noise_frames();
    let measured = encode_fps(&mut encoder, &warmup, &frames);
    println!("d3d11 vp9 encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(target_os = "windows")]
/// 1080p AV1 encode measured against the realtime bar.
#[test]
fn encode_av1_1080p_measures_realtime() {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut encoder = match D3d11Encoder::open(&device, Codec::Av1) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no av1 encoder MFT ({error})");
            return;
        }
    };
    let (warmup, frames) = noise_frames();
    let measured = encode_fps(&mut encoder, &warmup, &frames);
    println!("d3d11 av1 encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
