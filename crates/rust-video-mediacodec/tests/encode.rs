//! Device encode matrix over the three shipped codecs.
//!
//! Each case opens its codec, encodes one black frame, drains, and resets.
//! A missing codec or a device below the floor declines loudly; any other
//! failure is a real failure. Host builds compile this file to nothing.

#[cfg(target_os = "android")]
use rust_video_caps::traits::Encoder;
#[cfg(target_os = "android")]
use rust_video_harness::Synth;
#[cfg(target_os = "android")]
use rust_video_mediacodec::{
    EncodeSpec, EncodeState, MediaCodecEncoder, MediaCodecError, QuirkTable,
};

/// Encoder for `spec`, or loud decline when the device lacks the codec.
#[cfg(target_os = "android")]
fn open_or_decline(
    name: &str,
    spec: &EncodeSpec,
    quirks: &QuirkTable,
) -> Option<MediaCodecEncoder> {
    match MediaCodecEncoder::open(spec, "generic", quirks) {
        Ok(encoder) => Some(encoder),
        Err(error @ (MediaCodecError::NoEncoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("declined: mediacodec {name} encode on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Open, one-frame encode, flush, and reset for one codec.
#[cfg(target_os = "android")]
fn run_case(name: &str, spec: &EncodeSpec) {
    let quirks = QuirkTable::load().expect("embedded quirks parse");
    let Some(mut encoder) = open_or_decline(name, spec, &quirks) else {
        return;
    };
    assert_eq!(encoder.state(), EncodeState::Started);
    let frame = Synth::black_yuv420p(spec.width(), spec.height());
    encoder.encode(&frame).expect("black frame encodes");
    encoder.flush().expect("flush drains");
    encoder.reset().expect("reset drains and flushes");
}

#[cfg(target_os = "android")]
#[test]
fn h264_encodes_black_frame() {
    let spec = EncodeSpec::h264(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_case("h264", &spec);
}

#[cfg(target_os = "android")]
#[test]
fn h265_encodes_black_frame() {
    let spec = EncodeSpec::h265(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_case("h265", &spec);
}

#[cfg(target_os = "android")]
#[test]
fn av1_encodes_black_frame() {
    let spec = EncodeSpec::av1(64, 64, 1_000_000, 30, 1).expect("valid spec");
    run_case("av1", &spec);
}
