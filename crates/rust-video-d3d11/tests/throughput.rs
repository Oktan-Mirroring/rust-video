//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.
//! Also covers encode latency, VP9 tuning, and seam alloc cost.

#![cfg(target_os = "windows")]

use std::hint::black_box;
use std::time::Instant;

use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, D3d11Frame, GpuSurface, H264Decoder,
    HevcDecoder, OverlappedPump, Transcoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::delivery::ReceiveFrames;
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, EncodeSurface, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_harness::{
    bench_dir, decode_dir, AnnexB, CountingAlloc, Drive, H264Synth, Ivf, Packets, Synth,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Hops timed after warmup.
const HOPS: u32 = 30;

/// Tier1 realtime bar in frames per second.
const BAR: f64 = 60.0;

/// Sustained overlap bars: quiet medians minus 20% headroom, 2026-11-18.
const SUSTAINED_H264_BAR: f64 = 860.0;
const SUSTAINED_HEVC_BAR: f64 = 760.0;
const SUSTAINED_AV1_BAR: f64 = 880.0;
const SUSTAINED_VP9_BAR: f64 = 700.0;

/// Warmup frames ahead of the timed loop.
const WARMUP: u64 = 4;

/// Timed runs behind the VP9 median.
const VP9_REPEATS: usize = 3;

/// Surface decode behind the surface benches.
trait SurfaceDecode {
    /// GPU surfaces decoded from `packet`, possibly none.
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error>;

    /// GPU surfaces held back for reordering or buffering.
    fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error>;
}

impl SurfaceDecode for H264Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        H264Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        H264Decoder::flush_surface(self)
    }
}

impl SurfaceDecode for HevcDecoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        HevcDecoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        HevcDecoder::flush_surface(self)
    }
}

impl SurfaceDecode for Av1Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        Av1Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        Av1Decoder::flush_surface(self)
    }
}

impl SurfaceDecode for Vp9Decoder<'_> {
    fn decode_surface(&mut self, packet: &Packet) -> Result<Vec<GpuSurface>, D3d11Error> {
        Vp9Decoder::decode_surface(self, packet)
    }

    fn flush_surface(&mut self) -> Result<Vec<GpuSurface>, D3d11Error> {
        Vp9Decoder::flush_surface(self)
    }
}

/// Measured fps of `HOPS` 1080p hops through `transcoder`.
fn fps(transcoder: &mut Transcoder, warmup: &[Vec<u8>], raws: &[Vec<u8>]) -> f64 {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for packet in &warmup {
        transcoder.transcode(packet).expect("warmup transcode");
        assert!(matches!(transcoder.plan().route(), Route::Gpu));
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            let coded = transcoder.transcode(packet).expect("timed transcode");
            assert!(matches!(transcoder.plan().route(), Route::Gpu));
            bytes += coded
                .iter()
                .map(|packet| packet.data().len())
                .sum::<usize>();
        }
    }
    transcoder.flush().expect("timed flush");
    assert!(matches!(transcoder.plan().route(), Route::Gpu));
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(HOPS) / elapsed
}

/// Measured fps decoding `HOPS` passes over `raws` after `warmup`.
fn decode_fps(
    decoder: &mut impl Decoder<Frame = D3d11Frame, Error = D3d11Error>,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> f64 {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
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

/// Measured fps decoding `HOPS` surface passes over `raws` after `warmup`.
///
/// Every surface waits for GPU completion inside the timed loop, so the
/// number paces engine-plus-fence cost without any CPU readback. Each
/// surface drops after its wait, keeping the 16 decoder slots free.
fn decode_surface_fps(
    decoder: &mut impl SurfaceDecode,
    warmup: &[Vec<u8>],
    raws: &[Vec<u8>],
) -> f64 {
    let warmup = Packets::from_raws(warmup);
    let raws = Packets::from_raws(raws);
    for packet in &warmup {
        for surface in decoder.decode_surface(packet).expect("warmup decode") {
            surface.try_wait().expect("warmup wait");
        }
    }
    let mut surfaces = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            for surface in decoder.decode_surface(packet).expect("timed decode") {
                surface.try_wait().expect("timed wait");
                surfaces += 1;
            }
        }
    }
    for surface in decoder.flush_surface().expect("timed flush") {
        surface.try_wait().expect("timed wait");
        surfaces += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(surfaces > 0);
    f64::from(surfaces) / elapsed
}

/// Measured fps encoding `surface` `HOPS` times after `warmup` encodes.
///
/// One decoded 1080p surface feeds every call: the leg under test is
/// texture import plus engine encode, not decode.
fn encode_surface_fps(encoder: &mut D3d11Encoder, surface: &GpuSurface) -> f64 {
    for _ in 0..WARMUP {
        encoder.encode_surface(surface).expect("warmup encode");
    }
    let mut bytes = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in encoder.encode_surface(surface).expect("timed encode") {
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

/// Noise 1080p frame from `seed`.
fn noise(seed: u64) -> CpuFrame {
    Synth::noise(1920, 1080, seed)
}

/// Warmup plus timed noise 1080p frames on distinct seeds.
fn noise_frames() -> (Vec<CpuFrame>, Vec<CpuFrame>) {
    let warmup = (0..WARMUP).map(noise).collect();
    let timed = (WARMUP..WARMUP + u64::from(HOPS)).map(noise).collect();
    (warmup, timed)
}

/// One decoded 1080p surface behind the encode-surface benches.
///
/// The synth set holds a single IDR; the surface outlives its decoder
/// by shared ownership, so any encoder MFT can import it.
fn surface_1080p(device: &D3d11Device) -> GpuSurface {
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let mut decoder = H264Decoder::open(device);
    let mut surfaces = Vec::new();
    for raw in &raws {
        surfaces.extend(
            decoder
                .decode_surface(&Packet::new(Bytes::copy_from_slice(raw)))
                .expect("surface decode"),
        );
    }
    surfaces.extend(decoder.flush_surface().expect("surface drain"));
    assert_eq!(surfaces.len(), 1, "synth set holds one picture");
    let surface = surfaces.pop().expect("one surface");
    surface.try_wait().expect("surface completes");
    let size = surface.size();
    assert_eq!(
        (size.width(), size.height()),
        (1920, 1080),
        "synth set is 1080p"
    );
    surface
}

/// 1080p GPU hops measured against the realtime bar.
#[test]
fn xcode_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let small = Packets::from_raws(&H264Synth::annex_b_packets());
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

/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let _serial = Drive::serial();
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

/// 1080p HEVC decode measured against the realtime bar.
#[test]
fn decode_hevc_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::H265) {
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

/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let _serial = Drive::serial();
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

/// 1080p VP9 decode measured against the realtime bar.
#[test]
fn decode_vp9_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::Vp9) {
        println!("d3d11 skipped: no vp9 decoder profile");
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
    let measured = decode_fps(&mut decoder, &raws, &raws);
    println!("d3d11 vp9 decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn encode_h264_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::H264).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
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

/// 1080p HEVC encode measured against the realtime bar.
#[test]
fn encode_hevc_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::H265).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
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

/// 1080p VP9 encode measured against the realtime bar.
///
/// Median of three runs, alone via just benches: sharing a run taxes
/// the software encoder past the bar.
#[test]
#[ignore = "runs alone under just benches"]
fn encode_vp9_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let (warmup, frames) = noise_frames();
    let mut runs = Vec::new();
    for _ in 0..VP9_REPEATS {
        let config = D3d11Encoder::auto_config(Codec::Vp9).expect("auto config");
        let mut encoder = match D3d11Encoder::open(&device, config) {
            Ok(encoder) => encoder,
            Err(error) => {
                println!("d3d11 skipped: no vp9 encoder MFT ({error})");
                return;
            }
        };
        runs.push(encode_fps(&mut encoder, &warmup, &frames));
    }
    runs.sort_by(f64::total_cmp);
    let measured = runs[VP9_REPEATS / 2];
    println!("d3d11 vp9 encode 1080p: {measured:.1}fps median of {runs:?} over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 encode measured against the realtime bar.
#[test]
fn encode_av1_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::Av1).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
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

/// 1080p H.264 surface decode measured against the realtime bar.
#[test]
fn decode_surface_h264_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = decode_surface_fps(&mut decoder, &raws, &raws);
    println!(
        "d3d11 h264 surface decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p HEVC surface decode measured against the realtime bar.
#[test]
fn decode_surface_hevc_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::H265) {
        println!("d3d11 skipped: no hevc decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let mut decoder = HevcDecoder::open(&device);
    let measured = decode_surface_fps(&mut decoder, &raws, &raws);
    println!(
        "d3d11 hevc surface decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 surface decode measured against the realtime bar.
#[test]
fn decode_surface_av1_1080p_measures_realtime() {
    let _serial = Drive::serial();
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
    let measured = decode_surface_fps(&mut decoder, &raws, &raws);
    println!("d3d11 av1 surface decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p VP9 surface decode measured against the realtime bar.
#[test]
fn decode_surface_vp9_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::Vp9) {
        println!("d3d11 skipped: no vp9 decoder profile");
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let mut decoder = Vp9Decoder::open(&device);
    let measured = decode_surface_fps(&mut decoder, &raws, &raws);
    println!("d3d11 vp9 surface decode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p H.264 surface encode measured against the realtime bar.
#[test]
fn encode_surface_h264_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::H264).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no h264 encoder MFT ({error})");
            return;
        }
    };
    let surface = surface_1080p(&device);
    if let Err(error) = encoder.encode_surface(&surface) {
        println!("d3d11 skipped: no surface encode import ({error})");
        return;
    }
    let measured = encode_surface_fps(&mut encoder, &surface);
    println!(
        "d3d11 h264 surface encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p HEVC surface encode measured against the realtime bar.
#[test]
fn encode_surface_hevc_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::H265).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no hevc encoder MFT ({error})");
            return;
        }
    };
    let surface = surface_1080p(&device);
    if let Err(error) = encoder.encode_surface(&surface) {
        println!("d3d11 skipped: no surface encode import ({error})");
        return;
    }
    let measured = encode_surface_fps(&mut encoder, &surface);
    println!(
        "d3d11 hevc surface encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps"
    );
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p VP9 surface encode measured against the realtime bar.
#[test]
fn encode_surface_vp9_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::Vp9).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no vp9 encoder MFT ({error})");
            return;
        }
    };
    let surface = surface_1080p(&device);
    if let Err(error) = encoder.encode_surface(&surface) {
        println!("d3d11 skipped: no surface encode import ({error})");
        return;
    }
    let measured = encode_surface_fps(&mut encoder, &surface);
    println!("d3d11 vp9 surface encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

/// 1080p AV1 surface encode measured against the realtime bar.
#[test]
fn encode_surface_av1_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(Codec::Av1).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("d3d11 skipped: no av1 encoder MFT ({error})");
            return;
        }
    };
    let surface = surface_1080p(&device);
    if let Err(error) = encoder.encode_surface(&surface) {
        println!("d3d11 skipped: no surface encode import ({error})");
        return;
    }
    let measured = encode_surface_fps(&mut encoder, &surface);
    println!("d3d11 av1 surface encode 1080p: {measured:.1}fps over {HOPS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

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

/// Measured fps of one sustained overlap pass of `raws`, plus frame count and peak depth.
macro_rules! sustained_fps {
    ($decoder:expr, $raws:expr) => {{
        let mut pump = OverlappedPump::open($decoder);
        let (measured, count) = Drive::sustained(&mut pump, &Packets::from_raws($raws), drain);
        (measured, count, pump.max_depth())
    }};
}

/// Sustained 1080p H.264 overlap throughput against its bar.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::H264) {
        println!("d3d11 skipped: no h264 decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h264-1080p_ip.h264")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_h264_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let decoder = H264Decoder::open(&device);
    let (measured, count, depth) = sustained_fps!(decoder, &raws);
    println!("d3d11 h264 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_H264_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_H264_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_H264_BAR:.0}fps"
    );
}

/// Sustained 1080p HEVC overlap throughput against its bar.
#[test]
fn sustained_hevc_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::H265) {
        println!("d3d11 skipped: no hevc decoder profile");
        return;
    }
    let data = std::fs::read(bench_dir().join("h265-1080p_ip.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let decoder = HevcDecoder::open(&device);
    let (measured, count, depth) = sustained_fps!(decoder, &raws);
    println!("d3d11 hevc sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_HEVC_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_HEVC_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_HEVC_BAR:.0}fps"
    );
}

/// Sustained 1080p AV1 overlap throughput against its bar.
#[test]
fn sustained_av1_1080p_measures_realtime() {
    let _serial = Drive::serial();
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
    let raws = Ivf::read(&bench_dir().join("av1-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let decoder = Av1Decoder::open(&device);
    let (measured, count, depth) = sustained_fps!(decoder, &raws);
    println!("d3d11 av1 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_AV1_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_AV1_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_AV1_BAR:.0}fps"
    );
}

/// Sustained 1080p VP9 overlap throughput against its bar.
#[test]
fn sustained_vp9_1080p_measures_realtime() {
    let _serial = Drive::serial();
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    if !device.supports(Codec::Vp9) {
        println!("d3d11 skipped: no vp9 decoder profile");
        return;
    }
    let raws = Ivf::read(&bench_dir().join("vp9-1080p_ip.ivf")).frames;
    assert!(!raws.is_empty(), "bench vector holds frames");
    let decoder = Vp9Decoder::open(&device);
    let (measured, count, depth) = sustained_fps!(decoder, &raws);
    println!("d3d11 vp9 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_VP9_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_VP9_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_VP9_BAR:.0}fps"
    );
}

/// Calls of `codec` each return their own stamped packet, flush stays empty.
fn audit_own_frames(codec: Codec, tag: &str) {
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("{tag} skipped: {error}");
            return;
        }
    };
    let config = D3d11Encoder::auto_config(codec).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("{tag} skipped: {error}");
            return;
        }
    };
    let mut step = None;
    let mut first = None;
    for call in 0..4u64 {
        let frame = Synth::noise(1920, 1080, call);
        let packets = encoder.encode(&frame).expect("encode runs");
        assert!(!packets.is_empty(), "{tag} call {call} held its frame back");
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
            "{tag} call {call} missed its own stamp"
        );
    }
    let drained = encoder.flush().expect("flush runs");
    assert!(
        drained.is_empty(),
        "{tag} flush held {} back",
        drained.len()
    );
}

#[test]
fn encode_returns_own_frames() {
    audit_own_frames(Codec::H264, "h264");
    audit_own_frames(Codec::H265, "hevc");
    audit_own_frames(Codec::Av1, "av1");
    audit_own_frames(Codec::Vp9, "vp9");
}

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
    let config = D3d11Encoder::auto_config(Codec::Vp9).expect("auto config");
    let mut encoder = match D3d11Encoder::open(&device, config) {
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

/// Frames decoded from `packets` plus the flush drain.
fn decode_all(decoder: &mut H264Decoder<'_>, packets: &[Packet]) -> usize {
    let mut frames = 0;
    for packet in packets {
        frames += decoder.decode(packet).expect("steady decode").len();
    }
    frames += decoder.flush().expect("flush drains").len();
    frames
}

/// Boxed export cost stays under one percent of frame decode time.
///
/// `emit_frame` boxes one 16-byte `SharedExport` (`Arc` plus handle)
/// per frame behind the `FrameExport` seam. The count below covers
/// Rust-side allocs only (COM and driver allocs bypass the counter);
/// the Box microbench sizes one 16-byte alloc-plus-free against one
/// frame decode on this hardware.
#[test]
fn seam_box_cost_is_negligible_per_frame() {
    /// 16-byte payload, the `SharedExport` size behind the seam.
    const BOX_PAYLOAD: [u64; 2] = [0x1234_5678, 0x9ABC_DEF0];
    /// Box alloc-plus-free iterations in the microbench.
    const BOX_ITERS: u32 = 200_000;
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let path = decode_dir().join("valid_320x240.h264");
    let raws = AnnexB::picture_packets(&path, AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let packets: Vec<Packet> = raws
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect();
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(
        decode_all(&mut decoder, &packets),
        15,
        "warmup yields every picture"
    );
    CountingAlloc::reset();
    let start = Instant::now();
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(
        decode_all(&mut decoder, &packets),
        15,
        "measured yields every picture"
    );
    let frame_ns = start.elapsed().as_nanos() / 15;
    let allocs = CountingAlloc::allocation_count();
    let start = Instant::now();
    for _ in 0..BOX_ITERS {
        drop(black_box(Box::new(BOX_PAYLOAD)));
    }
    let box_ns = start.elapsed().as_nanos() / u128::from(BOX_ITERS);
    let millipct = box_ns * 100_000 / frame_ns;
    println!(
        "seam allocs: {allocs} Rust-side allocs over 15 frames ({} per frame), \
         frame {frame_ns}ns, 16B box {box_ns}ns ({}.{:03}% of frame)",
        allocs / 15,
        millipct / 1000,
        millipct % 1000,
    );
    assert!(
        box_ns * 100 < frame_ns,
        "box {box_ns}ns exceeds 1% of frame {frame_ns}ns"
    );
}
