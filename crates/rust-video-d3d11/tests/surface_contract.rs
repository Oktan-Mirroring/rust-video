//! Surface contract: per-call parity, reorder holdback, pinning, metadata.
//!
//! Skips without hardware; fails when the contract breaks.

#![cfg(target_os = "windows")]

use bytes::Bytes;
use rust_video_caps::frame::{GpuExport, PixelFormat};
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::{Decoder, Encoder};
use rust_video_d3d11::{
    Av1Decoder, Codec, D3d11Device, D3d11Encoder, D3d11Error, GpuSurface, H264Decoder, HevcDecoder,
};
use rust_video_harness::{decode_dir, AnnexB, H264Synth, Obu, Synth};

/// Open device, loud skip without hardware.
fn device() -> Option<D3d11Device> {
    match D3d11Device::open() {
        Ok(device) => Some(device),
        Err(error) => {
            println!("d3d11 skipped: {error}");
            None
        }
    }
}

/// Open device with `codec` profile, loud skip without.
fn device_for(codec: Codec) -> Option<D3d11Device> {
    let device = device()?;
    if !device.supports(codec) {
        println!("d3d11 skipped: no {codec} decoder profile");
        return None;
    }
    Some(device)
}

/// Shared H.264 vector path.
fn h264_vector() -> std::path::PathBuf {
    decode_dir().join("valid_320x240.h264")
}

/// Contract metadata on a `width` x `height` surface.
fn assert_surface_metadata(surface: &GpuSurface, width: u32, height: u32) {
    let size = surface.size();
    assert_eq!((size.width(), size.height()), (width, height));
    assert_eq!(surface.format(), PixelFormat::Nv12);
    assert!(surface.slice() < 16, "slice inside the views");
    let coded = surface.coded();
    assert!(coded.width() >= width && coded.height() >= height);
    let (left, top) = surface.origin();
    assert!(left + width <= coded.width() && top + height <= coded.height());
    assert!(matches!(surface.export(), GpuExport::NtHandle { .. }));
}

/// No-reorder H.264 pipelines two deep across decode calls.
#[test]
fn h264_no_reorder_pipelines_two_deep() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&h264_vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let mut count = 0;
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let frames = decoder.decode(&packet).expect("steady decode");
        if index < 2 {
            assert!(frames.is_empty(), "call {index} holds its picture back");
        } else {
            assert_eq!(frames.len(), 1, "picture completes two calls late");
        }
        count += frames.len();
    }
    let flushed = decoder.flush().expect("flush drains");
    assert_eq!(flushed.len(), 2, "flush releases the held tail");
    assert_eq!(count + flushed.len(), 15, "every picture completes once");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// No-reorder HEVC emits each picture on its completing call.
#[test]
fn hevc_loopback_no_reorder_returns_per_call() {
    let Some(device) = device_for(Codec::H265) else {
        return;
    };
    let config = D3d11Encoder::auto_config(Codec::H265).expect("hevc auto");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("hevc skipped: {error}");
            return;
        }
    };
    let mut decoder = HevcDecoder::open(&device);
    let mut total = 0;
    for seed in 0..4u64 {
        let frame = Synth::noise(320, 240, seed);
        for packet in encoder.encode(&frame).expect("encode runs") {
            let frames = decoder.decode(&packet).expect("decode runs");
            assert!(frames.len() <= 1, "no bunching on a no-reorder stream");
            total += frames.len();
        }
    }
    for packet in encoder.flush().expect("encode drains") {
        let frames = decoder.decode(&packet).expect("decode runs");
        assert!(frames.len() <= 1, "no bunching on a no-reorder stream");
        total += frames.len();
    }
    assert_eq!(total, 4, "every picture completes");
    assert!(
        decoder.flush().expect("decode drains").is_empty(),
        "nothing held back"
    );
    assert_eq!(decoder.resets(), 0, "clean stream never resets");
}

/// No-reorder AV1 emits each frame on its completing call.
#[test]
fn av1_loopback_no_reorder_returns_per_call() {
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let config = D3d11Encoder::auto_config(Codec::Av1).expect("av1 auto");
    let mut encoder = match D3d11Encoder::open(&device, config) {
        Ok(encoder) => encoder,
        Err(error) => {
            println!("av1 skipped: {error}");
            return;
        }
    };
    let mut decoder = Av1Decoder::open(&device);
    let mut total = 0;
    for seed in 0..4u64 {
        let frame = Synth::noise(320, 240, seed);
        for packet in encoder.encode(&frame).expect("encode runs") {
            let frames = decoder.decode(&packet).expect("decode runs");
            assert!(frames.len() <= 1, "no bunching on a no-reorder stream");
            total += frames.len();
        }
    }
    for packet in encoder.flush().expect("encode drains") {
        let frames = decoder.decode(&packet).expect("decode runs");
        assert!(frames.len() <= 1, "no bunching on a no-reorder stream");
        total += frames.len();
    }
    assert_eq!(total, 4, "every frame completes");
    assert!(
        decoder.flush().expect("decode drains").is_empty(),
        "nothing held back"
    );
    assert_eq!(decoder.resets(), 0, "clean stream never resets");
}

/// Reorder HEVC holds back exactly as before.
#[test]
fn hevc_reorder_holds_back_unchanged() {
    let Some(device) = device_for(Codec::H265) else {
        return;
    };
    let data = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let mut counts = Vec::new();
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        counts.push(decoder.decode(&packet).expect("steady decode").len());
    }
    counts.push(decoder.flush().expect("flush drains").len());
    assert_eq!(counts[0], 0, "first picture held back");
    assert_eq!(
        counts.as_slice(),
        [0, 0, 1, 1, 1, 1, 1, 2, 0, 1, 1, 1, 1, 1, 1, 2],
        "reorder pattern unchanged"
    );
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// Show-existing AV1 displays exactly as before.
#[test]
fn av1_reorder_holds_back_unchanged() {
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let data = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let raws = Obu::tu_packets(&data);
    assert_eq!(raws.len(), 16, "vector holds one packet per unit");
    let mut decoder = Av1Decoder::open(&device);
    let mut counts = Vec::new();
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        counts.push(decoder.decode(&packet).expect("steady decode").len());
    }
    counts.push(decoder.flush().expect("flush drains").len());
    assert_eq!(
        counts.as_slice(),
        [0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0],
        "display pattern unchanged"
    );
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// H.264 surfaces match frames with contract metadata.
#[test]
fn h264_surfaces_match_frames() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&h264_vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let mut total = 0;
    for (index, raw) in raws.iter().enumerate() {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let surfaces = decoder.decode_surface(&packet).expect("steady decode");
        assert_eq!(surfaces.len(), 1, "picture {index} completes on its call");
        for surface in &surfaces {
            surface.try_wait().expect("surface completes");
            if total == 0 {
                assert_surface_metadata(surface, 320, 240);
                assert_eq!(surface.slice(), 0, "first picture takes slot zero");
                let coded = surface.coded();
                assert_eq!((coded.width(), coded.height()), (320, 240));
                assert_eq!(surface.origin(), (0, 0));
            }
            total += 1;
        }
    }
    assert!(
        decoder.flush_surface().expect("flush drains").is_empty(),
        "nothing held back"
    );
    assert_eq!(total, 15, "every picture surfaces");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// HEVC surfaces match frames with contract metadata.
#[test]
fn hevc_surfaces_match_frames() {
    let Some(device) = device_for(Codec::H265) else {
        return;
    };
    let data = std::fs::read(decode_dir().join("inter_320x240.hevc")).expect("vector readable");
    let raws = AnnexB::group_pictures(&data, AnnexB::is_hevc_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = HevcDecoder::open(&device);
    let mut total = 0;
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        for surface in decoder.decode_surface(&packet).expect("steady decode") {
            surface.try_wait().expect("surface completes");
            if total == 0 {
                assert_surface_metadata(&surface, 320, 240);
            }
            total += 1;
        }
    }
    for surface in decoder.flush_surface().expect("flush drains") {
        surface.try_wait().expect("surface completes");
        total += 1;
    }
    assert_eq!(total, 15, "every picture surfaces");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// AV1 surfaces match frames with contract metadata.
#[test]
fn av1_surfaces_match_frames() {
    let Some(device) = device_for(Codec::Av1) else {
        return;
    };
    let data = std::fs::read(decode_dir().join("valid_320x240.obu")).expect("vector readable");
    let raws = Obu::tu_packets(&data);
    assert_eq!(raws.len(), 16, "vector holds one packet per unit");
    let mut decoder = Av1Decoder::open(&device);
    let mut total = 0;
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        for surface in decoder.decode_surface(&packet).expect("steady decode") {
            surface.try_wait().expect("surface completes");
            if total == 0 {
                assert_surface_metadata(&surface, 320, 240);
            }
            total += 1;
        }
    }
    for surface in decoder.flush_surface().expect("flush drains") {
        surface.try_wait().expect("surface completes");
        total += 1;
    }
    assert_eq!(total, 15, "every frame surfaces");
    assert_eq!(decoder.resets(), 0, "clean vector never resets");
}

/// Cropped surfaces name the coded extent and crop origin.
#[test]
fn surface_metadata_names_crop() {
    let Some(device) = device() else {
        return;
    };
    let mut decoder = H264Decoder::open(&device);
    let mut surfaces = Vec::new();
    for raw in H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40) {
        surfaces.extend(
            decoder
                .decode_surface(&Packet::new(Bytes::from(raw)))
                .expect("steady decode"),
        );
    }
    surfaces.extend(decoder.flush_surface().expect("flush drains"));
    assert_eq!(surfaces.len(), 1, "synth set holds one picture");
    let surface = &surfaces[0];
    assert_surface_metadata(surface, 1920, 1080);
    let coded = surface.coded();
    assert_eq!((coded.width(), coded.height()), (1920, 1088));
    assert_eq!(surface.origin(), (0, 0));
    surface.try_wait().expect("surface completes");
}

/// Held surfaces pin slots; exhaustion is loud, recovery clean.
#[test]
fn held_surfaces_pin_slots_until_dropped() {
    let Some(device) = device() else {
        return;
    };
    let raws = AnnexB::picture_packets(&h264_vector(), AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let mut decoder = H264Decoder::open(&device);
    let mut held = Vec::new();
    let mut passes = 0;
    for _ in 0..4 {
        passes += 1;
        let mut exhausted = false;
        for raw in &raws {
            match decoder.decode_surface(&Packet::new(Bytes::copy_from_slice(raw))) {
                Ok(surfaces) => held.extend(surfaces),
                Err(error) => {
                    assert!(
                        matches!(error, D3d11Error::NoFreeSlot),
                        "loud exhaustion, got {error}"
                    );
                    assert_eq!(error.to_string(), "no free surface slot");
                    exhausted = true;
                    break;
                }
            }
        }
        if exhausted {
            break;
        }
    }
    assert_eq!(passes, 2, "second pass exhausts the views");
    assert_eq!(held.len(), 16, "one surface per view");
    drop(held);
    let mut total = 0;
    for raw in &raws {
        total += decoder
            .decode_surface(&Packet::new(Bytes::copy_from_slice(raw)))
            .expect("recovery decodes")
            .len();
    }
    total += decoder.flush_surface().expect("recovery drains").len();
    assert_eq!(total, 15, "recovery yields every picture");
}
