//! VA-API surface contract: geometry, provenance, wait, and pinning.
//!
//! Needs Linux with VA-API hardware; every test skips loudly without it.

#[cfg(target_os = "linux")]
use bytes::Bytes;
#[cfg(target_os = "linux")]
use rust_video_caps::common::PictureSize;
#[cfg(target_os = "linux")]
use rust_video_caps::frame::{GpuExport, PixelFormat};
#[cfg(target_os = "linux")]
use rust_video_caps::packet::Packet;
#[cfg(target_os = "linux")]
use rust_video_caps::traits::{Decoder, Device};
#[cfg(target_os = "linux")]
use rust_video_harness::H264Synth;
#[cfg(target_os = "linux")]
use rust_video_vaapi::{H264Decoder, VaApiDevice, VaApiError};

#[cfg(target_os = "linux")]
/// Packets over raw Annex B payloads.
fn packets(raws: &[Vec<u8>]) -> Vec<Packet> {
    raws.iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect()
}

#[cfg(target_os = "linux")]
/// Open device, skipping `test` loudly without hardware.
fn device(test: &str) -> Option<VaApiDevice> {
    match VaApiDevice::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("vaapi {test} skipped: {error}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
/// Native device, decoder, and context handles are exposed by reference.
#[test]
fn native_handles_are_exposed() {
    let Some(device) = device("surface native handles") else {
        return;
    };
    assert!(!device.display().is_null());
    assert_ne!(device.name(), "");
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(decoder.device().name(), device.name());
    assert!(decoder.context().is_none());
    let raws = H264Synth::annex_b_packets();
    let packets = packets(&raws);
    for packet in &packets {
        decoder.decode(packet).expect("synth decodes");
    }
    let context = decoder.context().expect("live context");
    assert_eq!(context.surfaces().len(), context.pool_size());
    assert_eq!(context.coded_size(), (64, 64));
}

#[cfg(target_os = "linux")]
/// Readback frames carry visible size, format, slot, coded extent, and origin.
#[test]
fn readback_frames_carry_surface_metadata() {
    let Some(device) = device("surface readback metadata") else {
        return;
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets();
    let packets = packets(&raws);
    let mut frames = Vec::new();
    for packet in &packets {
        frames.extend(decoder.decode(packet).expect("synth decodes"));
    }
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.size(), PictureSize::new(320, 240).expect("extent"));
    assert_eq!(frame.format(), PixelFormat::Nv12);
    assert_eq!(frame.slice(), 0);
    assert_eq!(frame.coded(), PictureSize::new(320, 240).expect("extent"));
    assert_eq!(frame.origin(), (0, 0));
    assert_eq!(frame.surface(), None);
    assert!(frame.wait().is_ok());
    assert!(matches!(
        frame.export(),
        GpuExport::Opaque { name: "vaapi" }
    ));
    assert_eq!(frame.nv12().len(), 320 * 240 * 3 / 2);
}

#[cfg(target_os = "linux")]
/// Cropped frames separate the visible size from the coded extent.
#[test]
fn cropped_frames_separate_visible_from_coded() {
    let Some(device) = device("surface cropped metadata") else {
        return;
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets_cropped();
    let packets = packets(&raws);
    let mut frames = Vec::new();
    for packet in &packets {
        frames.extend(decoder.decode(packet).expect("synth decodes"));
    }
    frames.extend(decoder.flush().expect("flush drains"));
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.size(), PictureSize::new(320, 224).expect("extent"));
    assert_eq!(frame.coded(), PictureSize::new(320, 240).expect("extent"));
    assert_eq!(frame.origin(), (0, 0));
    assert_eq!(frame.nv12().len(), 320 * 224 * 3 / 2);
}

#[cfg(target_os = "linux")]
/// Device frames wait cleanly and name their native surface.
#[test]
fn device_frames_wait_and_name_their_surface() {
    let Some(device) = device("surface device wait") else {
        return;
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_h264_packets();
    let packets = packets(&raws);
    let mut frames = Vec::new();
    for packet in &packets {
        frames.extend(decoder.decode_surface(packet).expect("synth decodes"));
    }
    frames.extend(decoder.flush_surface().expect("flush drains"));
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];
    assert_eq!(frame.slice(), 0);
    assert!(frame.surface().is_some());
    assert!(frame.wait().is_ok());
    let export = frame.export();
    assert!(
        matches!(export, GpuExport::DmaBuf { .. } | GpuExport::Opaque { .. }),
        "device export names planes or the backend: {export:?}"
    );
    assert_eq!(frame.nv12(), []);
    assert!(frame.to_cpu().is_none());
}

#[cfg(target_os = "linux")]
/// Held device frames pin their slots; exhaustion fails loud, never corrupts.
#[test]
fn exhausted_pool_fails_loud_with_no_free_slot() {
    let Some(device) = device("surface pool exhaustion") else {
        return;
    };
    let mut decoder = H264Decoder::open(&device);
    let raws = H264Synth::annex_b_packets();
    let packets = packets(&raws);
    let mut held = Vec::new();
    for _ in 0..32 {
        for packet in &packets {
            match decoder.decode_surface(packet) {
                Ok(frames) => held.extend(frames),
                Err(VaApiError::NoFreeSlot) => {
                    assert_eq!(VaApiError::NoFreeSlot.to_string(), "no free surface slot");
                    println!("vaapi pool exhausted with {} held frames", held.len());
                    return;
                }
                Err(error) => panic!("expected NoFreeSlot, got {error}"),
            }
        }
    }
    panic!("pool never exhausted with {} held frames", held.len());
}
