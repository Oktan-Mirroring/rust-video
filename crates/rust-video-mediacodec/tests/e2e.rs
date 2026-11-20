//! Device decode and encode matrices over the shipped codecs.
//!
//! Each decode case opens its codec, queues one empty packet, drains, and
//! resets; each encode case encodes one black frame instead. A missing
//! codec or a device below the floor declines loudly; any other failure
//! is a real failure. Host builds compile this file to nothing.

#![cfg(target_os = "android")]

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_harness::{decode_dir, AnnexB, Synth};
use rust_video_mediacodec::{
    DecodeState, EncodeSpec, EncodeState, FormatSpec, MediaCodecDecoder, MediaCodecEncoder,
    MediaCodecError,
};

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_decoder_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Open, empty decode, flush, and reset for one codec.
fn run_decode_case(name: &str, spec: &FormatSpec) {
    let Some(mut decoder) = open_decoder_or_decline(name, spec) else {
        return;
    };
    assert_eq!(decoder.state(), DecodeState::Started);
    let empty = Packet::new(Bytes::new());
    decoder.decode(&empty).expect("empty packet queues");
    decoder.flush().expect("flush drains");
    decoder.reset().expect("reset drains and flushes");
}

/// First picture bytes of a decode vector.
fn first_picture(name: &str, is_vcl: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    AnnexB::picture_packets(&decode_dir().join(name), is_vcl)
        .into_iter()
        .next()
        .expect("vector groups into pictures")
}

/// H.264 spec carrying real SPS/PPS from the shared vector.
fn h264_spec() -> FormatSpec {
    let packet = first_picture("valid_320x240.h264", AnnexB::is_h264_vcl);
    let (sps, pps) = AnnexB::avc_csd(&packet).expect("sps/pps in vector");
    FormatSpec::h264(320, 240)
        .expect("valid dims")
        .with_csd_0(sps)
        .with_csd_1(pps)
}

/// H.265 spec carrying real VPS/SPS/PPS from the shared vector.
fn h265_spec() -> FormatSpec {
    let packet = first_picture("inter_320x240.hevc", AnnexB::is_hevc_vcl);
    let (vps, sets) = AnnexB::hevc_csd(&packet).expect("vps/sps/pps in vector");
    FormatSpec::h265(320, 240)
        .expect("valid dims")
        .with_csd_0(vps)
        .with_csd_1(sets)
}

/// VPCodecConfigurationRecord: profile 0, level 1.0, 8-bit 4:2:0, unspecified color.
fn vp9_csd_0() -> Vec<u8> {
    vec![0x00, 0x0A, 0x82, 0x02, 0x02, 0x02, 0x00, 0x00]
}

#[test]
fn h264_decodes_empty_packet() {
    run_decode_case("h264", &h264_spec());
}

#[test]
fn h265_decodes_empty_packet() {
    run_decode_case("h265", &h265_spec());
}

#[test]
fn av1_decodes_empty_packet() {
    let spec = FormatSpec::av1(64, 64).expect("valid dims");
    run_decode_case("av1", &spec);
}

#[test]
fn vp9_decodes_empty_packet() {
    let spec = FormatSpec::vp9(320, 240)
        .expect("valid dims")
        .with_csd_0(vp9_csd_0());
    run_decode_case("vp9", &spec);
}

#[test]
fn vp8_decodes_empty_packet() {
    let spec = FormatSpec::vp8(64, 64).expect("valid dims");
    run_decode_case("vp8", &spec);
}

/// Encoder for `spec`, or loud decline when the device lacks the codec.
fn open_encoder_or_decline(name: &str, spec: &EncodeSpec) -> Option<MediaCodecEncoder> {
    match MediaCodecEncoder::open(spec) {
        Ok(encoder) => Some(encoder),
        Err(error @ (MediaCodecError::NoEncoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} encode on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Open, one-frame encode, flush, and reset for one codec.
fn run_encode_case(name: &str, spec: &EncodeSpec) {
    let Some(mut encoder) = open_encoder_or_decline(name, spec) else {
        return;
    };
    assert_eq!(encoder.state(), EncodeState::Started);
    let frame = Synth::black_yuv420p(spec.width(), spec.height());
    encoder.encode(&frame).expect("black frame encodes");
    encoder.flush().expect("flush drains");
    encoder.reset().expect("reset drains and flushes");
}

#[test]
fn h264_encodes_black_frame() {
    let spec = EncodeSpec::h264(320, 240, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("h264", &spec);
}

#[test]
fn h265_encodes_black_frame() {
    let spec = EncodeSpec::h265(320, 240, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("h265", &spec);
}

#[test]
fn av1_encodes_black_frame() {
    let spec = EncodeSpec::av1(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("av1", &spec);
}

#[test]
fn vp9_encodes_black_frame() {
    let spec = EncodeSpec::vp9(320, 240, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("vp9", &spec);
}

#[test]
fn vp8_encodes_black_frame() {
    let spec = EncodeSpec::vp8(320, 240, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("vp8", &spec);
}
