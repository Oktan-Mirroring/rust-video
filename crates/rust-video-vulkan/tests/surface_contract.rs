//! Vulkan surface contract: per-call emission plus slot pinning.
//!
//! Env-gated: every test declines unless `RUST_VIDEO_VULKAN_SURFACE_TESTS=1`.
//! Device-gated: each test declines when the tier skips the machine.
//! Vector-gated: H.265 and AV1 read no-reorder vectors from the environment.

use std::collections::HashSet;
use std::path::PathBuf;

use bytes::Bytes;
use rust_video_caps::frame::PixelFormat;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_harness::{bench_dir, AnnexB, Drive, H264Synth};
use rust_video_vulkan::{
    Av1Decoder, CodecOp, H264Decoder, H265Decoder, SkipReason, VulkanDevice, VulkanError,
    VulkanFrame,
};

/// Env gate: `RUST_VIDEO_VULKAN_SURFACE_TESTS=1` runs, else declines.
fn gated() -> bool {
    if std::env::var("RUST_VIDEO_VULKAN_SURFACE_TESTS").as_deref() != Ok("1") {
        println!("declined: surface tests need RUST_VIDEO_VULKAN_SURFACE_TESTS=1");
        return false;
    }
    true
}

/// Device for `op`, declining loudly when the tier skips.
fn device_or_decline(op: CodecOp) -> Option<VulkanDevice> {
    match VulkanDevice::open_or_skip(op) {
        Ok(device) => Some(device),
        Err(VulkanError::BackendAbsent(reason @ SkipReason::OutOfDeviceMemory { .. })) => {
            println!("declined:OOM {op:?} on this box: {reason}");
            None
        }
        Err(VulkanError::BackendAbsent(reason)) => {
            println!("declined: {op:?} on this box: {reason}");
            None
        }
        Err(error) if error.is_out_of_device_memory() => {
            println!("declined:OOM {op:?} on this box: {error}");
            None
        }
        Err(error) => panic!("negotiation failed: {error}"),
    }
}

/// Unwrap `result`, declining loud with budget numbers on VRAM exhaustion.
///
/// Panics on other errors.
fn unwrap_or_oom<T>(
    result: Result<T, VulkanError>,
    context: &str,
    device: &VulkanDevice,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) if error.is_out_of_device_memory() => {
            let (free_mb, total_mb) = device.memory_budget_mb();
            println!("declined:OOM {context}: {error} (free {free_mb}MB of {total_mb}MB)");
            None
        }
        Err(error) => panic!("{context}: {error}"),
    }
}

/// Surface invariants every decoded frame holds.
fn check_surface(frame: &VulkanFrame) {
    let size = frame.size();
    let coded = frame.coded();
    let (left, top) = frame.origin();
    assert_eq!(frame.format(), PixelFormat::Nv12);
    assert!(coded.width() >= size.width() && coded.height() >= size.height());
    assert!(left.saturating_add(size.width()) <= coded.width());
    assert!(top.saturating_add(size.height()) <= coded.height());
    frame.wait();
}

/// No-reorder H.264 pipelines one deep across `decode` calls.
///
/// The synth carries no VUI, so the reorder depth is zero and 64x64
/// uncropped; the parser-level shape is proven GPU-free in the crate.
/// Each picture completes on the following call while one stays held.
#[test]
fn h264_no_reorder_pipelines_one_deep() {
    let _guard = Drive::serial();
    if !gated() {
        return;
    }
    let Some(device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device)
    else {
        return;
    };
    let packets = H264Synth::annex_b_packets();
    assert_eq!(packets.len(), 2);
    let packet = Packet::new(Bytes::copy_from_slice(&packets[0]));
    let Some(first) = unwrap_or_oom(decoder.decode(&packet), "decode runs", &device) else {
        return;
    };
    assert!(first.is_empty(), "first call holds its picture back");
    let packet = Packet::new(Bytes::copy_from_slice(&packets[1]));
    let Some(second) = unwrap_or_oom(decoder.decode(&packet), "decode runs", &device) else {
        return;
    };
    assert_eq!(second.len(), 1, "IDR completes on the following call");
    let idr = &second[0];
    check_surface(idr);
    assert_eq!((idr.size().width(), idr.size().height()), (64, 64));
    assert_eq!((idr.coded().width(), idr.coded().height()), (64, 64));
    assert_eq!(idr.origin(), (0, 0));
    assert_eq!(idr.slice(), 0);
    let Some(flushed) = unwrap_or_oom(decoder.flush(), "flush runs", &device) else {
        return;
    };
    assert_eq!(flushed.len(), 1, "flush releases the held tail");
    check_surface(&flushed[0]);
}

/// No-reorder H.265 pipelines one deep across `decode` calls.
///
/// `RUST_VIDEO_VULKAN_H265_NOREORDER` points at a stream whose sequence
/// sets declare zero reorder pictures with one slice per picture.
#[test]
fn h265_no_reorder_pipelines_one_deep() {
    let _guard = Drive::serial();
    if !gated() {
        return;
    }
    let Some(device) = device_or_decline(CodecOp::H265Decode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(H265Decoder::open(&device), "engine starts", &device)
    else {
        return;
    };
    let Some(path) = std::env::var_os("RUST_VIDEO_VULKAN_H265_NOREORDER") else {
        println!("declined: h265 per-call without RUST_VIDEO_VULKAN_H265_NOREORDER");
        return;
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path).expect("vector reads");
    let raws = AnnexB::group_pictures(&bytes, AnnexB::is_hevc_vcl);
    assert!(!raws.is_empty(), "no pictures in {}", path.display());
    let mut count = 0;
    for raw in &raws {
        let packet = Packet::new(Bytes::copy_from_slice(raw));
        let Some(frames) = unwrap_or_oom(decoder.decode(&packet), "decode runs", &device) else {
            return;
        };
        for frame in &frames {
            check_surface(frame);
        }
        count += frames.len();
    }
    let Some(flushed) = unwrap_or_oom(decoder.flush(), "flush runs", &device) else {
        return;
    };
    for frame in &flushed {
        check_surface(frame);
    }
    count += flushed.len();
    assert_eq!(count, raws.len(), "every picture completes once");
}

/// AV1 emits every shown picture across `decode` plus `flush`.
///
/// The reorder depth is always zero, so any vector proves it;
/// `RUST_VIDEO_VULKAN_AV1_NOREORDER` points at the `.obu` file.
#[test]
fn av1_no_reorder_pipelines_one_deep() {
    let _guard = Drive::serial();
    if !gated() {
        return;
    }
    let Some(device) = device_or_decline(CodecOp::Av1Decode) else {
        return;
    };
    let Some(mut decoder) = unwrap_or_oom(Av1Decoder::open(&device), "engine starts", &device)
    else {
        return;
    };
    let Some(path) = std::env::var_os("RUST_VIDEO_VULKAN_AV1_NOREORDER") else {
        println!("declined: av1 per-call without RUST_VIDEO_VULKAN_AV1_NOREORDER");
        return;
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path).expect("vector reads");
    let packet = Packet::new(Bytes::from(bytes));
    let Some(frames) = unwrap_or_oom(decoder.decode(&packet), "decode runs", &device) else {
        return;
    };
    let Some(flushed) = unwrap_or_oom(decoder.flush(), "flush runs", &device) else {
        return;
    };
    assert!(
        !frames.is_empty() || !flushed.is_empty(),
        "no pictures in {}",
        path.display()
    );
    for frame in frames.iter().chain(flushed.iter()) {
        check_surface(frame);
    }
}

/// Surfaces decoded from `raw`, declining loud on missing export or VRAM.
///
/// Panics on other errors.
fn decode_surfaces(
    device: &VulkanDevice,
    raw: &[u8],
    decode: impl FnOnce(&Packet) -> Result<Vec<VulkanFrame>, VulkanError>,
) -> Option<Vec<VulkanFrame>> {
    match decode(&Packet::new(Bytes::copy_from_slice(raw))) {
        Ok(frames) => Some(frames),
        Err(VulkanError::Unsupported { .. }) => {
            println!("declined: surface proof without exportable planes");
            None
        }
        Err(error) if error.is_out_of_device_memory() => {
            let (free_mb, total_mb) = device.memory_budget_mb();
            println!("declined:OOM surface proof: {error} (free {free_mb}MB of {total_mb}MB)");
            None
        }
        Err(error) => panic!("decode runs: {error}"),
    }
}

/// Held GPU surfaces pin distinct export slots; freed slots recycle.
///
/// Needs platform export behind the device; declines without it.
#[test]
fn gpu_surfaces_pin_export_slots() {
    let _guard = Drive::serial();
    if !gated() {
        return;
    }
    let Some(device) = device_or_decline(CodecOp::H264Decode) else {
        return;
    };
    if device.share_mechanism().is_none() {
        println!("declined: pinning proof needs platform export");
        return;
    }
    let Some(mut decoder) = unwrap_or_oom(H264Decoder::open(&device), "engine starts", &device)
    else {
        return;
    };
    let packets = H264Synth::annex_b_packets();
    let Some(first) = decode_surfaces(&device, &packets[0], |packet| {
        decoder.decode_surface(packet)
    }) else {
        return;
    };
    assert!(first.is_empty(), "first call holds its picture back");
    let Some(second) = decode_surfaces(&device, &packets[1], |packet| {
        decoder.decode_surface(packet)
    }) else {
        return;
    };
    assert_eq!(second.len(), 1, "first surface completes next call");
    let Some(flushed) = unwrap_or_oom(decoder.flush_surface(), "flush runs", &device) else {
        return;
    };
    assert_eq!(flushed.len(), 1, "flush releases the held tail");
    let held_a = second[0].planes().expect("gpu planes");
    let held_b = flushed[0].planes().expect("gpu planes");
    assert_ne!(held_a.y_image(), held_a.uv_image());
    assert_ne!(held_b.y_image(), held_b.uv_image());
    assert_ne!(
        held_a.y_image(),
        held_b.y_image(),
        "held surfaces share no slot"
    );
    assert_ne!(
        held_a.uv_image(),
        held_b.uv_image(),
        "held surfaces share no slot"
    );
    let freed = held_a.y_image();
    drop(second);
    drop(flushed);
    let Some(third) = decode_surfaces(&device, &packets[0], |packet| {
        decoder.decode_surface(packet)
    }) else {
        return;
    };
    assert!(third.is_empty(), "resubmitted picture stays held");
    let Some(recycled) = unwrap_or_oom(decoder.flush_surface(), "flush runs", &device) else {
        return;
    };
    assert_eq!(recycled.len(), 1);
    assert_eq!(
        recycled[0].planes().expect("gpu planes").y_image(),
        freed,
        "freed slot recycles"
    );
}

/// AV1 surface decodes recycle export slots instead of growing the pool.
///
/// Repeated decodes of one packet reuse a bounded set of export pairs;
/// distinct plane handles stay flat no matter how many frames decode.
#[test]
fn av1_surface_recycles_export_slots() {
    let _guard = Drive::serial();
    if !gated() {
        return;
    }
    let Some(device) = device_or_decline(CodecOp::Av1Decode) else {
        return;
    };
    if device.share_mechanism().is_none() {
        println!("declined: recycling proof needs platform export");
        return;
    }
    let Some(mut decoder) = unwrap_or_oom(Av1Decoder::open(&device), "engine starts", &device)
    else {
        return;
    };
    let raw = std::fs::read(bench_dir().join("av1-1080p.obu")).expect("vector readable");
    let mut slots = HashSet::new();
    for _ in 0..64 {
        let Some(frames) = decode_surfaces(&device, &raw, |packet| decoder.decode_surface(packet))
        else {
            return;
        };
        for frame in &frames {
            check_surface(frame);
            slots.insert(frame.planes().expect("gpu planes").y_image());
        }
        drop(frames);
    }
    let Some(flushed) = unwrap_or_oom(decoder.flush_surface(), "flush runs", &device) else {
        return;
    };
    for frame in &flushed {
        slots.insert(frame.planes().expect("gpu planes").y_image());
    }
    println!(
        "av1 surface export slots: {} distinct over 64 decodes",
        slots.len()
    );
    assert!(
        slots.len() <= 8,
        "export pool grows per frame: {} distinct slots over 64 decodes",
        slots.len()
    );
}
