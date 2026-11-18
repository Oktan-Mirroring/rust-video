//! GPU transcode throughput: 1080p hops against the 60 fps bar.
//!
//! Skips without hardware; a hardware run fails below the bar.

#![cfg(target_os = "windows")]

use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, D3d11Frame, GpuSurface, H264Decoder,
    HevcDecoder, OverlappedPump, SplitDecoder, Transcoder, Vp9Decoder,
};

use bytes::Bytes;
use rust_video_caps::delivery::{ReceiveFrames, SendPackets, TrySendError};
use rust_video_caps::frame::CpuFrame;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_caps::transcode::Route;
use rust_video_harness::{bench_dir, AnnexB, H264Synth, Ivf, Synth};

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

/// Serialized benches: one test at a time per binary.
///
/// Parallel benches share NVDEC/NVENC engines, CPU, and the software
/// VP9 MFT; contention inside the timed loop reads as flake
/// (`encode_vp9_1080p` passes isolated, fails in-suite at ~57fps even
/// with timed-section-only serialization, because sibling warmups and
/// noise generation overlap the measurement). Every test holds the
/// lock for its whole body, so the bar and the measured path are
/// unchanged and the measurement sees a quiet binary.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Whole-test serial guard, poison-tolerant.
fn measure() -> std::sync::MutexGuard<'static, ()> {
    MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

/// Packets over `raws`, built once outside the timed loop.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

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
    let warmup = packets(warmup);
    let raws = packets(raws);
    for packet in &warmup {
        for surface in decoder.decode_surface(packet).expect("warmup decode") {
            surface.wait().expect("warmup wait");
        }
    }
    let mut surfaces = 0;
    let start = std::time::Instant::now();
    for _ in 0..HOPS {
        for packet in &raws {
            for surface in decoder.decode_surface(packet).expect("timed decode") {
                surface.wait().expect("timed wait");
                surfaces += 1;
            }
        }
    }
    for surface in decoder.flush_surface().expect("timed flush") {
        surface.wait().expect("timed wait");
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
    surface.wait().expect("surface completes");
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
    let _serial = measure();
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

/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn decode_h264_1080p_measures_realtime() {
    let _serial = measure();
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
    let _serial = measure();
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

/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn decode_av1_1080p_measures_realtime() {
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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

/// 1080p HEVC encode measured against the realtime bar.
#[test]
fn encode_hevc_1080p_measures_realtime() {
    let _serial = measure();
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

/// 1080p VP9 encode measured against the realtime bar.
///
/// Median of three runs, alone via just benches: sharing a run taxes
/// the software encoder past the bar.
#[test]
#[ignore = "runs alone under just benches"]
fn encode_vp9_1080p_measures_realtime() {
    let _serial = measure();
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
        let mut encoder = match D3d11Encoder::open(&device, Codec::Vp9) {
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
    let _serial = measure();
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

/// 1080p H.264 surface decode measured against the realtime bar.
#[test]
fn decode_surface_h264_1080p_measures_realtime() {
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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
    let _serial = measure();
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

/// Measured fps of one sustained overlap pass of `raws`, plus frame count and peak depth.
fn sustained_fps<D>(decoder: D, raws: &[Vec<u8>]) -> (f64, usize, usize)
where
    D: SplitDecoder<Frame = D3d11Frame>,
{
    let packets = packets(raws);
    let mut pump = OverlappedPump::open(decoder);
    let mut frames = Vec::new();
    let start = std::time::Instant::now();
    for packet in packets {
        let mut pending = packet;
        loop {
            match pump.try_send(pending) {
                Ok(()) => break,
                Err(TrySendError::Full { packet }) => {
                    pending = packet;
                    let frame = pump
                        .receive()
                        .expect("in-flight frame completes")
                        .expect("input unfinished");
                    frames.push(frame);
                }
                Err(TrySendError::Failed { error }) => panic!("send failed: {error}"),
            }
        }
    }
    pump.finish().expect("finish drains");
    while let Some(frame) = pump.receive().expect("drain") {
        frames.push(frame);
    }
    let wall = start.elapsed();
    let depth = pump.max_depth();
    let total = u32::try_from(frames.len()).expect("frame count fits");
    (f64::from(total) / wall.as_secs_f64(), frames.len(), depth)
}

/// Sustained 1080p H.264 overlap throughput against its bar.
#[test]
fn sustained_h264_1080p_measures_realtime() {
    let _serial = measure();
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
    let (measured, count, depth) = sustained_fps(decoder, &raws);
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
    let _serial = measure();
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
    let data = std::fs::read(bench_dir().join("h265-1080p_ip.h265")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "bench vector holds pictures");
    let decoder = HevcDecoder::open(&device);
    let (measured, count, depth) = sustained_fps(decoder, &raws);
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
    let _serial = measure();
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
    let (measured, count, depth) = sustained_fps(decoder, &raws);
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
    let _serial = measure();
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
    let (measured, count, depth) = sustained_fps(decoder, &raws);
    println!("d3d11 vp9 sustained overlap 1080p: {measured:.1}fps over {count} frames, depth {depth}, bar {SUSTAINED_VP9_BAR:.0}fps");
    assert_eq!(count, raws.len(), "every picture yields its frame");
    assert!(
        measured >= SUSTAINED_VP9_BAR,
        "below sustained bar: {measured:.1}fps < {SUSTAINED_VP9_BAR:.0}fps"
    );
}
