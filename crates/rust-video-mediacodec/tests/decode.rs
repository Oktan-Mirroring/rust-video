#![cfg(target_os = "android")]

//! Device decode matrix over the five shipped codecs.
//!
//! Each case opens its codec, queues one empty packet, drains, and resets.
//! A missing codec or a device below the floor declines loudly; any other
//! failure is a real failure. Host builds compile this file to nothing.

use bytes::Bytes;

use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_mediacodec::{
    DecodeState, FormatSpec, MediaCodecDecoder, MediaCodecError, QuirkTable,
};

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(
    name: &str,
    spec: &FormatSpec,
    quirks: &QuirkTable,
) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec, "generic", quirks) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("declined: mediacodec {name} on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Open, empty decode, flush, and reset for one codec.
fn run_case(name: &str, spec: &FormatSpec) {
    let quirks = QuirkTable::load().expect("embedded quirks parse");
    let Some(mut decoder) = open_or_decline(name, spec, &quirks) else {
        return;
    };
    assert_eq!(decoder.state(), DecodeState::Started);
    let empty = Packet::new(Bytes::new());
    decoder.decode(&empty).expect("empty packet queues");
    decoder.flush().expect("flush drains");
    decoder.reset().expect("reset drains and flushes");
}

#[test]
fn h264_decodes_empty_packet() {
    let spec = FormatSpec::h264(64, 64).expect("valid dims");
    run_case("h264", &spec);
}

#[test]
fn h265_decodes_empty_packet() {
    let spec = FormatSpec::h265(64, 64).expect("valid dims");
    run_case("h265", &spec);
}

#[test]
fn av1_decodes_empty_packet() {
    let spec = FormatSpec::av1(64, 64).expect("valid dims");
    run_case("av1", &spec);
}

#[test]
fn vp9_decodes_empty_packet() {
    let spec = FormatSpec::vp9(64, 64).expect("valid dims");
    run_case("vp9", &spec);
}

#[test]
fn vp8_decodes_empty_packet() {
    let spec = FormatSpec::vp8(64, 64).expect("valid dims");
    run_case("vp8", &spec);
}
