//! Surface contract over the `MediaCodec` tier: metadata, pinning, wait.
//!
//! Device cases need an Android target plus `RUST_VIDEO_MEDIACODEC_DEVICE=1`;
//! anything else skips loudly. Host builds compile this file to nothing.

#![cfg(target_os = "android")]

use bytes::Bytes;

use rust_video_caps::frame::GpuExport;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_mediacodec::{
    DecodeState, FormatSpec, MediaCodecDecoder, MediaCodecError, MediaCodecSurface, SurfaceSlots,
};

/// True when the device gate env requests on-device cases.
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("SKIP mediacodec surface: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
fn open_or_decline(name: &str, spec: &FormatSpec) -> Option<MediaCodecDecoder> {
    match MediaCodecDecoder::open(spec) {
        Ok(decoder) => Some(decoder),
        Err(error @ (MediaCodecError::NoDecoder { .. } | MediaCodecError::BelowMinSdk { .. })) => {
            println!("SKIP mediacodec {name} on this device: {error}");
            None
        }
        Err(error) => panic!("{name}: unexpected open failure: {error}"),
    }
}

/// Canonical metadata invariants over one surface.
fn check_surface(surface: &MediaCodecSurface) {
    assert_eq!(surface.slice(), 0);
    assert_eq!(surface.origin(), (0, 0));
    assert_eq!(surface.size(), surface.frame().size());
    assert_eq!(surface.format(), surface.frame().format());
    assert!(surface.coded().width() >= surface.size().width());
    assert!(surface.coded().height() >= surface.size().height());
    assert!(matches!(
        surface.export(),
        GpuExport::Opaque { name: "mediacodec" }
    ));
    surface.wait().expect("wait completes");
}

#[test]
fn slots_pin_and_release_on_device() {
    if !device_gate() {
        return;
    }
    let slots = SurfaceSlots::new(2);
    assert_eq!(slots.live(), 0);
    let first = slots.acquire().expect("slot one");
    let _second = slots.acquire().expect("slot two");
    assert_eq!(slots.live(), 2);
    assert_eq!(slots.acquire().unwrap_err(), MediaCodecError::NoFreeSlot);
    assert_eq!(
        MediaCodecError::NoFreeSlot.to_string(),
        "no free surface slot"
    );
    drop(first);
    assert_eq!(slots.live(), 1);
    slots.acquire().expect("slot freed");
}

#[test]
fn decoder_issues_contract_surfaces() {
    if !device_gate() {
        return;
    }
    let spec = FormatSpec::h264(64, 64)
        .expect("valid dims")
        .with_csd_0(vec![0, 0, 1])
        .with_csd_1(vec![0, 0, 1]);
    let Some(mut decoder) = open_or_decline("h264", &spec) else {
        return;
    };
    assert_eq!(decoder.state(), DecodeState::Started);
    assert_eq!(decoder.live_surfaces(), 0);
    let empty = Packet::new(Bytes::new());
    let surfaces = decoder.decode(&empty).expect("empty packet queues");
    assert_eq!(decoder.live_surfaces(), surfaces.len());
    for surface in &surfaces {
        check_surface(surface);
    }
    drop(surfaces);
    assert_eq!(decoder.live_surfaces(), 0);
}
