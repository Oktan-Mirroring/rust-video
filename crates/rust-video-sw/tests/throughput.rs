//! Software codec throughput: 1080p decode and encode against the 60 fps bar.
//!
//! Skips without a backend; a backend run fails below the bar.

#[cfg(feature = "ffmpeg")]
use std::fs;
#[cfg(feature = "ffmpeg")]
use std::hint::black_box;
#[cfg(feature = "ffmpeg")]
use std::num::NonZeroUsize;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use std::time::Instant;

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use bytes::Bytes;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_caps::common::PictureSize;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::delivery::{
    ChannelConfig, DropPolicy, PollFrames, ReceiveFrames, SendPackets,
};
#[cfg(feature = "ffmpeg")]
use rust_video_caps::frame::GpuExport;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
#[cfg(feature = "ffmpeg")]
use rust_video_caps::packet::Packet;
#[cfg(feature = "ffmpeg")]
use rust_video_caps::traits::Decoder;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_caps::traits::Encoder;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_harness::Synth;
#[cfg(feature = "ffmpeg")]
use rust_video_harness::{bench_dir, AnnexB, H264Synth, Ivf};
#[cfg(all(feature = "h264-openh264", nasm_available))]
use rust_video_sw::H264Encoder;
#[cfg(feature = "gpl")]
use rust_video_sw::H265Encoder;
#[cfg(feature = "ffmpeg")]
use rust_video_sw::{
    Av1Decoder, DecoderPump, H264Decoder, H265Decoder, SwSurface, Vp8Decoder, Vp9Decoder,
};
#[cfg(feature = "ffmpeg")]
use rust_video_sw::{Av1Encoder, Vp8Encoder, Vp9Encoder};

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Timed iterations per bench.
const ITERS: u32 = 30;

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Realtime bar in frames per second.
const BAR: f64 = 60.0;

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Bench frame width.
const WIDTH: u32 = 1920;

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Bench frame height.
const HEIGHT: u32 = 1080;

#[cfg(feature = "ffmpeg")]
/// Packets over `raws`.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

#[cfg(feature = "ffmpeg")]
/// Measured fps of `ITERS` 1080p decode passes through `decoder`.
fn decode_fps<D>(decoder: &mut D, packets: &[Packet]) -> f64
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut warm = 0;
    for packet in packets {
        warm += decoder.decode(packet).expect("warmup decode").len();
    }
    warm += decoder.flush().expect("warmup flush").len();
    assert!(warm > 0, "vector decodes");
    let mut frames: u32 = 0;
    let start = Instant::now();
    for _ in 0..ITERS {
        for packet in packets {
            let count = decoder.decode(packet).expect("timed decode").len();
            frames += u32::try_from(count).expect("pass frames fit");
        }
    }
    let tail = decoder.flush().expect("timed flush").len();
    frames += u32::try_from(tail).expect("tail frames fit");
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    f64::from(frames) / elapsed
}

#[cfg(feature = "ffmpeg")]
/// Queue bound behind the surface benches; polled eager, never near full.
fn surface_config() -> ChannelConfig {
    let bound = NonZeroUsize::new(1024).expect("queue bound");
    ChannelConfig::new(bound, DropPolicy::DropOldest)
}

#[cfg(feature = "ffmpeg")]
/// Folded plane bytes of `frame` with the surface legs read.
///
/// # Performance
///
/// One pass over every plane byte.
///
/// # Arguments
///
/// * `frame` - decoded frame; single layer, opaque export, complete at return.
fn touch_surface(frame: &CpuFrame) -> u64 {
    assert_eq!(frame.slice(), 0);
    assert_eq!(frame.coded(), frame.size());
    assert_eq!(frame.origin(), (0, 0));
    assert!(matches!(frame.export(), GpuExport::Opaque { name: "sw" }));
    frame.wait();
    let mut folded = 0u64;
    for plane in frame.planes() {
        for byte in plane.data() {
            folded = folded.wrapping_add(u64::from(*byte));
        }
    }
    folded
}

#[cfg(feature = "ffmpeg")]
/// Drained frame count plus folded surface bytes from `pump`.
///
/// # Arguments
///
/// * `pump` - split-phase pump drained through non-blocking polls.
fn drain_surfaces<D>(pump: &mut DecoderPump<D>) -> (u32, u64)
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut count = 0u32;
    let mut folded = 0u64;
    while let Some(frame) = pump.try_receive().expect("surface poll") {
        count += 1;
        folded = folded.wrapping_add(touch_surface(&frame));
    }
    (count, folded)
}

#[cfg(feature = "ffmpeg")]
/// Measured fps of `ITERS` 1080p surface passes through `decoder`.
///
/// Each pass sends every packet through a split-phase pump and reads each
/// frame through the surface legs plus a plane fold.
fn surface_fps<D>(decoder: D, packets: &[Packet]) -> f64
where
    D: Decoder<Frame = CpuFrame>,
    D::Error: std::fmt::Debug,
{
    let mut pump = DecoderPump::open(decoder, surface_config());
    let mut warm = 0u32;
    for packet in packets {
        pump.try_send(packet.clone()).expect("warmup send");
        warm += drain_surfaces(&mut pump).0;
    }
    assert!(warm > 0, "vector decodes");
    let mut frames = 0u32;
    let mut folded = 0u64;
    let start = Instant::now();
    for _ in 0..ITERS {
        for packet in packets {
            pump.try_send(packet.clone()).expect("timed send");
            let (count, bytes) = drain_surfaces(&mut pump);
            frames += count;
            folded = folded.wrapping_add(bytes);
        }
    }
    pump.finish().expect("timed finish");
    while let Some(frame) = pump.receive().expect("tail drain") {
        frames += 1;
        folded = folded.wrapping_add(touch_surface(&frame));
    }
    assert_eq!(pump.dropped(), 0, "roomy queue drops nothing");
    black_box(folded);
    let elapsed = start.elapsed().as_secs_f64();
    assert!(frames > 0);
    f64::from(frames) / elapsed
}

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Measured fps of `ITERS` 1080p encodes through `encoder`.
fn encode_fps<E>(encoder: &mut E, frames: &[CpuFrame]) -> f64
where
    E: Encoder,
    E::Error: std::fmt::Debug,
{
    for frame in frames {
        encoder.encode(frame).expect("warmup encode");
    }
    encoder.flush().expect("warmup flush");
    let mut bytes = 0;
    let mut emitted = 0u32;
    let start = Instant::now();
    for frame in frames
        .iter()
        .cycle()
        .take(usize::try_from(ITERS).expect("iters fit"))
    {
        for packet in &encoder.encode(frame).expect("timed encode") {
            bytes += packet.data().len();
            emitted += 1;
        }
    }
    for packet in &encoder.flush().expect("timed flush") {
        bytes += packet.data().len();
        emitted += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    assert!(emitted > 0);
    f64::from(emitted) / elapsed
}

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// YUV420P frame deinterleaved from NV12 noise `seed`.
fn yuv420p_from_noise(seed: u64) -> CpuFrame {
    let source = Synth::noise(WIDTH, HEIGHT, seed);
    let planes = source.planes();
    let mut u = Vec::with_capacity(planes[1].data().len() / 2);
    let mut v = Vec::with_capacity(planes[1].data().len() / 2);
    for pair in planes[1].data().chunks_exact(2) {
        u.push(pair[0]);
        v.push(pair[1]);
    }
    let size = PictureSize::new(WIDTH, HEIGHT).expect("nonzero extent");
    let y = Plane::new(Bytes::copy_from_slice(planes[0].data()), WIDTH).expect("luma");
    let u = Plane::new(Bytes::from(u), WIDTH / 2).expect("chroma u");
    let v = Plane::new(Bytes::from(v), WIDTH / 2).expect("chroma v");
    CpuFrame::new(size, PixelFormat::Yuv420P, vec![y, u, v]).expect("frame")
}

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
/// Four noise frames cycling through the timed encodes.
fn noise_frames() -> Vec<CpuFrame> {
    (0..4u64).map(yuv420p_from_noise).collect()
}

#[cfg(feature = "ffmpeg")]
/// 1080p H.264 decode measured against the realtime bar.
#[test]
fn h264_decode_1080p_measures_realtime() {
    let mut decoder = match H264Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw h264 decode 1080p skipped: {error}");
            return;
        }
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = decode_fps(&mut decoder, &packets(&raws));
    println!("sw h264 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p HEVC decode measured against the realtime bar.
#[test]
fn h265_decode_1080p_measures_realtime() {
    let mut decoder = match H265Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw h265 decode 1080p skipped: {error}");
            return;
        }
    };
    let bytes = fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("h265 vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    let measured = decode_fps(&mut decoder, &packets(&raws));
    println!("sw h265 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP8 decode measured against the realtime bar.
#[test]
fn vp8_decode_1080p_measures_realtime() {
    let mut decoder = match Vp8Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw vp8 decode 1080p skipped: {error}");
            return;
        }
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    let measured = decode_fps(&mut decoder, &packets(&raws));
    println!("sw vp8 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP9 decode measured against the realtime bar.
#[test]
fn vp9_decode_1080p_measures_realtime() {
    let mut decoder = match Vp9Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw vp9 decode 1080p skipped: {error}");
            return;
        }
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    let measured = decode_fps(&mut decoder, &packets(&raws));
    println!("sw vp9 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p AV1 decode measured against the realtime bar.
#[test]
fn av1_decode_1080p_measures_realtime() {
    let mut decoder = match Av1Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw av1 decode 1080p skipped: {error}");
            return;
        }
    };
    let bytes = fs::read(bench_dir().join("av1-1080p.obu")).expect("av1 vector readable");
    let packets = [Packet::new(Bytes::from(bytes))];
    let measured = decode_fps(&mut decoder, &packets);
    println!("sw av1 decode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p H.264 surface path measured against the realtime bar.
#[test]
fn h264_surface_1080p_measures_realtime() {
    let decoder = match H264Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw h264 surface 1080p skipped: {error}");
            return;
        }
    };
    let raws = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
    let measured = surface_fps(decoder, &packets(&raws));
    println!("sw h264 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p HEVC surface path measured against the realtime bar.
#[test]
fn h265_surface_1080p_measures_realtime() {
    let decoder = match H265Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw h265 surface 1080p skipped: {error}");
            return;
        }
    };
    let bytes = fs::read(bench_dir().join("h265-1080p-nvenc.h265")).expect("h265 vector readable");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    let measured = surface_fps(decoder, &packets(&raws));
    println!("sw h265 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP8 surface path measured against the realtime bar.
#[test]
fn vp8_surface_1080p_measures_realtime() {
    let decoder = match Vp8Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw vp8 surface 1080p skipped: {error}");
            return;
        }
    };
    let raws = Ivf::read(&bench_dir().join("vp8-1080p.ivf")).frames;
    let measured = surface_fps(decoder, &packets(&raws));
    println!("sw vp8 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP9 surface path measured against the realtime bar.
#[test]
fn vp9_surface_1080p_measures_realtime() {
    let decoder = match Vp9Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw vp9 surface 1080p skipped: {error}");
            return;
        }
    };
    let raws = Ivf::read(&bench_dir().join("vp9-1080p.ivf")).frames;
    let measured = surface_fps(decoder, &packets(&raws));
    println!("sw vp9 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p AV1 surface path measured against the realtime bar.
#[test]
fn av1_surface_1080p_measures_realtime() {
    let decoder = match Av1Decoder::open(4) {
        Ok(decoder) => decoder,
        Err(error) => {
            println!("sw av1 surface 1080p skipped: {error}");
            return;
        }
    };
    let bytes = fs::read(bench_dir().join("av1-1080p.obu")).expect("av1 vector readable");
    let packets = [Packet::new(Bytes::from(bytes))];
    let measured = surface_fps(decoder, &packets);
    println!("sw av1 surface 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(all(feature = "h264-openh264", nasm_available))]
/// 1080p H.264 encode measured against the realtime bar.
#[test]
fn h264_encode_1080p_measures_realtime() {
    let mut encoder = match H264Encoder::open(WIDTH, HEIGHT, 5_000_000, 4) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("sw h264 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = noise_frames();
    let measured = encode_fps(&mut encoder, &frames);
    println!("sw h264 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(all(feature = "h264-openh264", not(nasm_available)))]
#[test]
fn h264_encode_skipped_without_nasm() {
    println!("sw h264 encode skipped: openh264 without nasm cannot hold realtime");
}

#[cfg(feature = "gpl")]
/// 1080p HEVC encode measured against the realtime bar.
#[test]
fn h265_encode_1080p_measures_realtime() {
    let mut encoder = match H265Encoder::open(WIDTH, HEIGHT, 5_000, 4) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("sw h265 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = noise_frames();
    let measured = encode_fps(&mut encoder, &frames);
    println!("sw h265 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP8 encode measured against the realtime bar.
#[test]
fn vp8_encode_1080p_measures_realtime() {
    let mut encoder = match Vp8Encoder::open(WIDTH, HEIGHT, 5_000, 4) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("sw vp8 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = noise_frames();
    let measured = encode_fps(&mut encoder, &frames);
    println!("sw vp8 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p VP9 encode measured against the realtime bar.
#[test]
fn vp9_encode_1080p_measures_realtime() {
    let mut encoder = match Vp9Encoder::open(WIDTH, HEIGHT, 5_000, 4) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("sw vp9 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = noise_frames();
    let measured = encode_fps(&mut encoder, &frames);
    println!("sw vp9 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}

#[cfg(feature = "ffmpeg")]
/// 1080p AV1 encode measured against the realtime bar.
#[test]
fn av1_encode_1080p_measures_realtime() {
    let mut encoder = match Av1Encoder::open(WIDTH, HEIGHT, 5_000, 4) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("sw av1 encode 1080p skipped: {error}");
            return;
        }
    };
    let frames = noise_frames();
    let measured = encode_fps(&mut encoder, &frames);
    println!("sw av1 encode 1080p: {measured:.1}fps over {ITERS} iters, bar {BAR:.0}fps");
    assert!(
        measured >= BAR,
        "below realtime bar: {measured:.1}fps < {BAR:.0}fps"
    );
}
