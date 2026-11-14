//! Software codec throughput: 1080p decode and encode against the 60 fps bar.
//!
//! Skips without a backend; a backend run fails below the bar.

#[cfg(feature = "ffmpeg")]
use std::fs;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use std::time::Instant;

#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use bytes::Bytes;
#[cfg(any(feature = "ffmpeg", all(feature = "h264-openh264", nasm_available)))]
use rust_video_caps::common::PictureSize;
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
use rust_video_sw::{Av1Decoder, H264Decoder, H265Decoder, Vp8Decoder, Vp9Decoder};
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
    let start = Instant::now();
    for frame in frames
        .iter()
        .cycle()
        .take(usize::try_from(ITERS).expect("iters fit"))
    {
        for packet in &encoder.encode(frame).expect("timed encode") {
            bytes += packet.data().len();
        }
    }
    for packet in &encoder.flush().expect("timed flush") {
        bytes += packet.data().len();
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes > 0);
    f64::from(ITERS) / elapsed
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
