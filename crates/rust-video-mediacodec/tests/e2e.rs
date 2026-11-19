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
use rust_video_harness::Synth;
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

/// `open` demands both blobs on H.264/H.265; the empty packet never decodes them.
fn with_csd(spec: FormatSpec) -> FormatSpec {
    spec.with_csd_0(vec![0, 0, 1]).with_csd_1(vec![0, 0, 1])
}

#[test]
fn h264_decodes_empty_packet() {
    let spec = with_csd(FormatSpec::h264(64, 64).expect("valid dims"));
    run_decode_case("h264", &spec);
}

#[test]
fn h265_decodes_empty_packet() {
    let spec = with_csd(FormatSpec::h265(64, 64).expect("valid dims"));
    run_decode_case("h265", &spec);
}

#[test]
fn av1_decodes_empty_packet() {
    let spec = FormatSpec::av1(64, 64).expect("valid dims");
    run_decode_case("av1", &spec);
}

#[test]
fn vp9_decodes_empty_packet() {
    let spec = FormatSpec::vp9(64, 64).expect("valid dims");
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
    let spec = EncodeSpec::h264(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("h264", &spec);
}

#[test]
fn h265_encodes_black_frame() {
    let spec = EncodeSpec::h265(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("h265", &spec);
}

#[test]
fn av1_encodes_black_frame() {
    let spec = EncodeSpec::av1(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_encode_case("av1", &spec);
}
