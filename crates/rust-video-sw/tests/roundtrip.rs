//! Multi-slice output still decodes to full-size frames.

#![cfg(all(feature = "h264-openh264", nasm_available))]

use bytes::Bytes;
use openh264::decoder::{DecodeOptions, Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;
use rust_video_caps::common::PictureSize;
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
use rust_video_caps::traits::Encoder as _;
use rust_video_harness::Synth;
use rust_video_sw::H264Encoder;

fn yuv420p_from_noise(seed: u64) -> CpuFrame {
    let source = Synth::noise(1920, 1080, seed);
    let planes = source.planes();
    let mut u = Vec::with_capacity(planes[1].data().len() / 2);
    let mut v = Vec::with_capacity(planes[1].data().len() / 2);
    for pair in planes[1].data().chunks_exact(2) {
        u.push(pair[0]);
        v.push(pair[1]);
    }
    let size = PictureSize::new(1920, 1080).expect("size");
    let y = Plane::new(Bytes::copy_from_slice(planes[0].data()), 1920).expect("luma");
    let u = Plane::new(Bytes::from(u), 960).expect("chroma u");
    let v = Plane::new(Bytes::from(v), 960).expect("chroma v");
    CpuFrame::new(size, PixelFormat::Yuv420P, vec![y, u, v]).expect("frame")
}

#[test]
fn multislice_output_decodes_to_full_size() {
    let mut encoder = H264Encoder::open(1920, 1080, 5_000_000, 4).expect("open");
    let api = openh264::OpenH264API::from_source();
    let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
    let mut decoder = Decoder::with_api_config(api, config).expect("decoder");
    let mut frames = 0;
    for seed in 0..3u64 {
        for packet in encoder.encode(&yuv420p_from_noise(seed)).expect("encode") {
            if packet.data().is_empty() {
                continue;
            }
            let options = DecodeOptions::new().flush_after_decode(Flush::NoFlush);
            if let Some(out) = decoder
                .decode_with_options(packet.data(), options)
                .expect("decode")
            {
                assert_eq!(out.dimensions(), (1920, 1080));
                frames += 1;
            }
        }
    }
    assert!(frames >= 1, "at least the IDR decodes");
}
