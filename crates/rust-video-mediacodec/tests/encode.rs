//! Device encode matrix over the three shipped codecs.
//!
//! Each case opens its codec, encodes one black frame, drains, and resets.
//! A missing codec or a device below the floor declines loudly; any other
//! failure is a real failure. Host builds compile this file to nothing.

#![cfg(target_os = "android")]

use rust_video_caps::traits::Encoder;
use rust_video_harness::Synth;
use rust_video_mediacodec::{EncodeSpec, EncodeState, MediaCodecEncoder, MediaCodecError};

/// Encoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(name: &str, spec: &EncodeSpec) -> Option<MediaCodecEncoder> {
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
fn run_case(name: &str, spec: &EncodeSpec) {
    let Some(mut encoder) = open_or_decline(name, spec) else {
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
    run_case("h264", &spec);
}

#[test]
fn h265_encodes_black_frame() {
    let spec = EncodeSpec::h265(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_case("h265", &spec);
}

#[test]
fn av1_encodes_black_frame() {
    let spec = EncodeSpec::av1(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_case("av1", &spec);
}
