//! Device encode matrix over the three shipped codecs.
//!
//! Each case opens its codec, encodes one black frame, drains, and resets.
//! A missing codec or a device below the floor declines loudly; any other
//! failure is a real failure. Host builds compile this file to nothing.

#[cfg(target_os = "android")]
use bytes::Bytes;

#[cfg(target_os = "android")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "android")]
use rust_video_caps::frame::{CpuFrame, PixelFormat, Plane};
#[cfg(target_os = "android")]
use rust_video_caps::traits::Encoder;
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

/// Black planar frame at the spec extent.
#[cfg(target_os = "android")]
fn black_frame(spec: &EncodeSpec) -> CpuFrame {
    let width = usize::try_from(spec.width()).expect("device width fits");
    let height = usize::try_from(spec.height()).expect("device height fits");
    CpuFrame::new(
        PictureSize::new(spec.width(), spec.height()).expect("valid dims"),
        PixelFormat::Yuv420P,
        vec![
            Plane::new(Bytes::from(vec![0_u8; width * height]), spec.width()).expect("valid luma"),
            Plane::new(
                Bytes::from(vec![128_u8; width * height / 4]),
                spec.width() / 2,
            )
            .expect("valid chroma"),
            Plane::new(
                Bytes::from(vec![128_u8; width * height / 4]),
                spec.width() / 2,
            )
            .expect("valid chroma"),
        ],
    )
    .expect("three planes")
}

/// Open, one-frame encode, flush, and reset for one codec.
#[cfg(target_os = "android")]
fn run_case(name: &str, spec: &EncodeSpec) {
    let quirks = QuirkTable::load().expect("embedded quirks parse");
    let Some(mut encoder) = open_or_decline(name, spec, &quirks) else {
        return;
    };
    assert_eq!(encoder.state(), EncodeState::Started);
    let frame = black_frame(spec);
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
