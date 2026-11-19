//! Seam alloc cost: the per-frame export Box against frame time.
//!
//! Skips without hardware; fails when the Box stops being negligible.

#![cfg(target_os = "windows")]

use std::hint::black_box;
use std::time::Instant;

use bytes::Bytes;
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d11::{D3d11Device, H264Decoder};
use rust_video_harness::{decode_dir, AnnexB, CountingAlloc};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Frames decoded from `packets` plus the flush drain.
fn decode_all(decoder: &mut H264Decoder<'_>, packets: &[Packet]) -> usize {
    let mut frames = 0;
    for packet in packets {
        frames += decoder.decode(packet).expect("steady decode").len();
    }
    frames += decoder.flush().expect("flush drains").len();
    frames
}

/// Boxed export cost stays under one percent of frame decode time.
///
/// `emit_frame` boxes one 16-byte `SharedExport` (`Arc` plus handle)
/// per frame behind the `FrameExport` seam. The count below covers
/// Rust-side allocs only (COM and driver allocs bypass the counter);
/// the Box microbench sizes one 16-byte alloc-plus-free against one
/// frame decode on this hardware.
#[test]
fn seam_box_cost_is_negligible_per_frame() {
    /// 16-byte payload, the `SharedExport` size behind the seam.
    const BOX_PAYLOAD: [u64; 2] = [0x1234_5678, 0x9ABC_DEF0];
    /// Box alloc-plus-free iterations in the microbench.
    const BOX_ITERS: u32 = 200_000;
    let device = match D3d11Device::open() {
        Ok(device) => device,
        Err(error) => {
            println!("d3d11 skipped: {error}");
            return;
        }
    };
    let path = decode_dir().join("valid_320x240.h264");
    let raws = AnnexB::picture_packets(&path, AnnexB::is_h264_vcl);
    assert_eq!(raws.len(), 15, "vector holds one packet per picture");
    let packets: Vec<Packet> = raws
        .iter()
        .map(|raw| Packet::new(Bytes::copy_from_slice(raw)))
        .collect();
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(
        decode_all(&mut decoder, &packets),
        15,
        "warmup yields every picture"
    );
    CountingAlloc::reset();
    let start = Instant::now();
    let mut decoder = H264Decoder::open(&device);
    assert_eq!(
        decode_all(&mut decoder, &packets),
        15,
        "measured yields every picture"
    );
    let frame_ns = start.elapsed().as_nanos() / 15;
    let allocs = CountingAlloc::allocation_count();
    let start = Instant::now();
    for _ in 0..BOX_ITERS {
        drop(black_box(Box::new(BOX_PAYLOAD)));
    }
    let box_ns = start.elapsed().as_nanos() / u128::from(BOX_ITERS);
    let millipct = box_ns * 100_000 / frame_ns;
    println!(
        "seam allocs: {allocs} Rust-side allocs over 15 frames ({} per frame), \
         frame {frame_ns}ns, 16B box {box_ns}ns ({}.{:03}% of frame)",
        allocs / 15,
        millipct / 1000,
        millipct % 1000,
    );
    assert!(
        box_ns * 100 < frame_ns,
        "box {box_ns}ns exceeds 1% of frame {frame_ns}ns"
    );
}
