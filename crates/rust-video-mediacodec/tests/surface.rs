//! Surface contract over the `MediaCodec` tier: metadata, pinning, wait.
//!
//! Device cases need an Android target plus `RUST_VIDEO_MEDIACODEC_DEVICE=1`;
//! anything else skips loudly. Host builds run one loud skip.

#[cfg(target_os = "android")]
use bytes::Bytes;

#[cfg(target_os = "android")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "android")]
use rust_video_caps::traits::Decoder;
#[cfg(target_os = "android")]
use rust_video_mediacodec::{
    DecodeState, FormatSpec, MediaCodecDecoder, MediaCodecError, MediaCodecSurface, QuirkTable,
    SurfaceSlots,
};

/// True when the device gate env requests on-device cases.
#[cfg(target_os = "android")]
fn device_gate() -> bool {
    match std::env::var("RUST_VIDEO_MEDIACODEC_DEVICE") {
        Ok(value) if value == "1" => true,
        _ => {
            println!("mediacodec surface skipped: set RUST_VIDEO_MEDIACODEC_DEVICE=1 on a device");
            false
        }
    }
}

/// Decoder for `spec`, or loud decline when the device lacks the codec.
#[cfg(target_os = "android")]
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

/// Canonical metadata invariants over one surface.
#[cfg(target_os = "android")]
fn check_surface(surface: &MediaCodecSurface) {
    assert_eq!(surface.slice(), 0);
    assert_eq!(surface.origin(), (0, 0));
    assert_eq!(surface.size(), surface.frame().size());
    assert_eq!(surface.format(), surface.frame().format());
    assert!(surface.coded().width() >= surface.size().width());
    assert!(surface.coded().height() >= surface.size().height());
    let ticket = surface.export();
    assert_eq!(ticket.size(), surface.size());
    assert_eq!(ticket.format(), surface.format());
    surface.wait();
}

#[cfg(target_os = "android")]
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

#[cfg(target_os = "android")]
#[test]
fn decoder_issues_contract_surfaces() {
    if !device_gate() {
        return;
    }
    let quirks = QuirkTable::load().expect("embedded quirks parse");
    let spec = FormatSpec::h264(64, 64)
        .expect("valid dims")
        .with_csd_0(vec![0, 0, 1])
        .with_csd_1(vec![0, 0, 1]);
    let Some(mut decoder) = open_or_decline("h264", &spec, &quirks) else {
        return;
    };
    assert_eq!(decoder.state(), DecodeState::Started);
    assert_eq!(decoder.live_surfaces(), 0);
    let _codec = decoder.codec();
    let empty = Packet::new(Bytes::new());
    let surfaces = decoder.decode(&empty).expect("empty packet queues");
    assert_eq!(decoder.live_surfaces(), surfaces.len());
    for surface in &surfaces {
        check_surface(surface);
    }
    drop(surfaces);
    assert_eq!(decoder.live_surfaces(), 0);
}

#[cfg(not(target_os = "android"))]
#[test]
fn surface_skipped_off_android() {
    println!("mediacodec surface skipped: android-only tier");
}
