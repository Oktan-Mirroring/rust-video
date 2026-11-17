//! Surface contract: per-call parity, geometry, pinning, waits.
//!
//! Skips without hardware.

#![cfg(target_os = "windows")]

#[path = "../examples/common/hevc.rs"]
mod hevc;

use std::path::PathBuf;

use rust_video_d3d12::{D3d12Device, D3d12Error, H264Decoder, HevcDecoder};

use bytes::Bytes;
use rust_video_caps::frame::{GpuExport, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Device};
use rust_video_harness::{AnnexB, H264Synth};

/// Open device, loud skip without hardware.
fn device() -> Option<D3d12Device> {
    match D3d12Device::open() {
        Ok(device) => {
            println!("adapter: {}", device.name());
            Some(device)
        }
        Err(error) => {
            println!("SKIP: no d3d12 device ({error})");
            None
        }
    }
}

/// Committed vector path.
fn vector() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/valid_320x240.h264")
}

/// No-reorder vector emits one frame per decode call.
#[test]
fn h264_no_reorder_emits_per_call() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let frames = decoder.decode(&packet).expect("steady decode");
        assert_eq!(frames.len(), 1, "packet {index} emits on its call");
        let frame = frames.first().expect("frame");
        assert_eq!(frame.identity().size().width(), 320);
        assert_eq!(frame.identity().size().height(), 240);
    }
    assert!(decoder.flush().expect("flush drains").is_empty());
}

/// Stream without a reorder guarantee holds its first picture back.
#[test]
fn h264_without_guarantee_holds_back() {
    let Some(device) = device() else {
        return;
    };
    let raws = H264Synth::annex_b_packets();
    assert_eq!(raws.len(), 2);
    let mut decoder = H264Decoder::open(&device);
    let first = Packet::new(Bytes::copy_from_slice(&raws[0]));
    assert!(decoder.decode(&first).expect("idr decodes").is_empty());
    let second = Packet::new(Bytes::copy_from_slice(&raws[1]));
    let frames = decoder.decode(&second).expect("p decodes");
    assert_eq!(frames.len(), 1);
    let drained = decoder.flush().expect("flush drains");
    assert_eq!(drained.len(), 1);
}

/// Surfaces carry visible size, format, array layer, coded extent, and origin.
#[test]
fn surface_geometry_matches_stream() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let surfaces = decoder.decode_surface(&packet).expect("steady decode");
        assert_eq!(surfaces.len(), 1, "packet {index} emits on its call");
        let surface = surfaces.first().expect("surface");
        assert_eq!(surface.size().width(), 320);
        assert_eq!(surface.size().height(), 240);
        assert_eq!(surface.format(), PixelFormat::Nv12);
        assert_eq!(surface.slice(), 0, "single-layer array layer");
        assert_eq!(surface.coded().width(), 320);
        assert_eq!(surface.coded().height(), 240);
        assert_eq!(surface.origin(), (0, 0));
        surface.wait().expect("surface complete");
        match surface.export() {
            GpuExport::NtHandle { handle } => assert_ne!(handle, 0),
            GpuExport::Opaque { name } => assert_eq!(name, "d3d12"),
            _ => panic!("unproven export shape"),
        }
        let _ = surface.texture();
    }
    assert!(decoder.flush_surface().expect("flush drains").is_empty());
}

/// Cropped surfaces separate visible size from coded extent.
#[test]
fn cropped_surface_separates_visible_from_coded() {
    let Some(device) = device() else {
        return;
    };
    let raws = H264Synth::annex_b_h264_packets_cropped();
    assert_eq!(raws.len(), 1);
    let mut decoder = H264Decoder::open(&device);
    let packet = Packet::new(Bytes::copy_from_slice(&raws[0]));
    let surfaces = decoder.decode_surface(&packet).expect("idr decodes");
    assert!(surfaces.is_empty(), "synth without VUI holds back");
    let surfaces = decoder.flush_surface().expect("flush drains");
    assert_eq!(surfaces.len(), 1);
    let surface = surfaces.first().expect("surface");
    assert_eq!(surface.size().width(), 320);
    assert_eq!(surface.size().height(), 224);
    assert_eq!(surface.coded().width(), 320);
    assert_eq!(surface.coded().height(), 240);
    assert_eq!(surface.origin(), (0, 0));
    assert_eq!(surface.format(), PixelFormat::Nv12);
    surface.wait().expect("surface complete");
}

/// Held HEVC surface pins the single pool slot with a loud typed error.
#[test]
fn hevc_held_surface_pins_single_slot() {
    let Some(device) = device() else {
        return;
    };
    let mut raws = hevc::annex_b_packets().into_iter();
    let first = Packet::new(Bytes::from(raws.next().expect("sets plus IDR")));
    let second = Packet::new(Bytes::from(raws.next().expect("second IDR")));
    let mut decoder = HevcDecoder::open(&device);
    let held = decoder.decode_surface(&first).expect("first surface");
    assert_eq!(held.len(), 1);
    let Err(error) = decoder.decode_surface(&second) else {
        panic!("pinned slot fails loud")
    };
    assert!(
        matches!(error, D3d12Error::NoFreeSlot),
        "typed pin error, got {error}"
    );
    assert_eq!(error.to_string(), "no free surface slot");
    let Err(error) = decoder.decode(&second) else {
        panic!("readback honors the pin")
    };
    assert!(matches!(error, D3d12Error::NoFreeSlot));
    drop(held);
    let surfaces = decoder.decode_surface(&second).expect("slot frees on drop");
    assert_eq!(surfaces.len(), 1);
}

/// Held H264 surface pins its slot while other slots keep decoding.
#[test]
fn h264_held_surface_leaves_other_slots_open() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    assert!(decoder.video_queue().is_none());
    assert!(decoder.copy_queue().is_none());
    let first = Packet::new(Bytes::copy_from_slice(&raws[0]));
    let held = decoder.decode_surface(&first).expect("first surface");
    assert_eq!(held.len(), 1);
    assert!(decoder.video_queue().is_some());
    assert!(decoder.copy_queue().is_some());
    for raw in raws.iter().skip(1).take(3) {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let surfaces = decoder.decode_surface(&packet).expect("other slots decode");
        assert_eq!(surfaces.len(), 1);
    }
    drop(held);
}

/// Device exposes its native handles by reference.
#[test]
fn device_exposes_native_handles() {
    let Some(device) = device() else {
        return;
    };
    let native = std::ptr::from_ref(device.device());
    let video = std::ptr::from_ref(device.video());
    assert!(!native.is_null());
    assert!(!video.is_null());
}
