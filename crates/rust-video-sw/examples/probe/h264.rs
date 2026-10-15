//! openh264 decode and encode probes.

use std::fs;
use std::path::Path;
use std::time::Instant;

use openh264::decoder::{DecodeOptions, Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;

use crate::common::{pack_i420, split_nalus};

/// Decode Annex B `stream`, at most `max` frames, packed raw to `dump`.
pub fn run_decode(stream: &Path, max: usize, dump: Option<&Path>) {
    let bytes = fs::read(stream).expect("stream readable");
    let units = split_nalus(&bytes);
    println!("h264dec: {} NAL units", units.len());
    let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
    let api = openh264::OpenH264API::from_source();
    let mut decoder = Decoder::with_api_config(api, config).expect("decoder init");
    let mut packed_all = Vec::new();
    let mut frames = 0usize;
    let start = Instant::now();
    for unit in &units {
        if frames >= max {
            break;
        }
        let options = DecodeOptions::new().flush_after_decode(Flush::NoFlush);
        let Some(yuv) = decoder.decode_with_options(unit, options).expect("decode") else {
            continue;
        };
        push_packed(&yuv, &mut packed_all);
        frames += 1;
    }
    for yuv in decoder.flush_remaining().expect("flush") {
        if frames >= max {
            break;
        }
        push_packed(&yuv, &mut packed_all);
        frames += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "h264dec: {frames} frames in {secs:.2}s = {:.1} fps",
        crate::common::fps(frames, secs)
    );
    if let Some(path) = dump {
        fs::write(path, &packed_all).expect("dump written");
        println!("h264dec: {} bytes dumped", packed_all.len());
    }
}

/// Packed frame bytes appended to `out`.
fn push_packed(yuv: &openh264::decoder::DecodedYUV<'_>, out: &mut Vec<u8>) {
    let (w, h) = yuv.dimensions();
    let (ys, cs, _) = yuv.strides();
    out.extend_from_slice(&pack_i420(yuv.y(), yuv.u(), yuv.v(), w, h, ys, cs));
}

/// Encode `frames` I420 frames from `raw` at `width`x`height` into `out`.
pub fn run_encode(raw: &Path, width: u32, height: u32, frames: usize, out: &Path) {
    use bytes::Bytes;
    use rust_video_caps::common::PictureSize;
    use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
    use rust_video_caps::traits::Encoder as _;
    use rust_video_sw::H264Encoder;

    let pitch = width as usize * height as usize;
    let bytes = fs::read(raw).expect("raw readable");
    let size = pitch * 3 / 2;
    assert!(bytes.len() >= size * frames, "raw holds frames");
    let mut encoder = H264Encoder::open(width, height, 8_000_000, 4).expect("encoder init");
    let picture = PictureSize::new(width, height).expect("size");
    let mut coded = Vec::new();
    let start = Instant::now();
    for chunk in bytes.chunks_exact(size).take(frames) {
        let luma = &chunk[0..pitch];
        let chroma_u = &chunk[pitch..pitch + pitch / 4];
        let chroma_v = &chunk[pitch + pitch / 4..pitch * 3 / 2];
        let planes = [
            Plane::new(Bytes::copy_from_slice(luma), width).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_u), width / 2).expect("plane"),
            Plane::new(Bytes::copy_from_slice(chroma_v), width / 2).expect("plane"),
        ];
        let frame = CpuFrame::new(picture, PixelFormat::Yuv420P, planes.into()).expect("frame");
        for packet in encoder.encode(&frame).expect("encode") {
            coded.extend_from_slice(packet.data());
        }
    }
    for packet in encoder.flush().expect("flush") {
        coded.extend_from_slice(packet.data());
    }
    let secs = start.elapsed().as_secs_f64();
    fs::write(out, &coded).expect("bitstream written");
    println!(
        "h264enc: {frames} frames in {secs:.2}s = {:.1} fps, {} bytes",
        crate::common::fps(frames, secs),
        coded.len()
    );
}
