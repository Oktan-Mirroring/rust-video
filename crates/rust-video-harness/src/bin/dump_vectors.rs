//! Shared-bytes dumps: synth H.264 blob plus per-tier encode inputs.
//!
//! Recipe (repo root): `cargo run -p rust-video-harness --bin dump_vectors -- <out-dir>`
//! Check: append `--check` to byte-compare `<out-dir>` against regeneration.
//! Hashes land in `vectors/bench/PROVENANCE.md`; dumps stay in `.tmp`.

use std::fs;
use std::path::Path;

use rust_video_harness::{FrameHash, H264Synth, Synth};

/// Bench frame extent behind every dump.
const WIDTH: u32 = 1920;
/// Bench frame extent behind every dump.
const HEIGHT: u32 = 1080;

/// NV12 frame bytes: luma plane then interleaved chroma plane.
fn nv12_frame(seed: u64) -> Vec<u8> {
    let frame = Synth::noise(WIDTH, HEIGHT, seed);
    let planes = frame.planes();
    let mut bytes = Vec::with_capacity(planes[0].data().len() + planes[1].data().len());
    bytes.extend_from_slice(planes[0].data());
    bytes.extend_from_slice(planes[1].data());
    bytes
}

/// YUV420P frame bytes: luma then deinterleaved chroma, mirroring
/// `yuv420p_from_noise` in `crates/rust-video-sw/tests/throughput.rs`.
fn yuv420p_frame(seed: u64) -> Vec<u8> {
    let frame = Synth::noise(WIDTH, HEIGHT, seed);
    let planes = frame.planes();
    let mut bytes = Vec::with_capacity(planes[0].data().len() + planes[1].data().len());
    bytes.extend_from_slice(planes[0].data());
    let mut u = Vec::with_capacity(planes[1].data().len() / 2);
    let mut v = Vec::with_capacity(planes[1].data().len() / 2);
    for pair in planes[1].data().chunks_exact(2) {
        u.push(pair[0]);
        v.push(pair[1]);
    }
    bytes.extend_from_slice(&u);
    bytes.extend_from_slice(&v);
    bytes
}

/// Timed-frame seeds per tier, matching each `throughput.rs` encode leg.
fn encode_seeds(tier: &str) -> Vec<u64> {
    match tier {
        "d11" => (4..34u64).collect(),
        "d12" => (0..30u64).collect(),
        "vk" => (0..30u64).map(|i| i % 4).collect(),
        "mc" => (0..30u64).map(|i| i % 4 + 1).collect(),
        "va" => (0..120u64).map(|i| i % 4 + 1).collect(),
        "sw" => (0..30u64).map(|i| i % 4).collect(),
        _ => unreachable!("known tier"),
    }
}

/// Dump bytes for `name`: synth blob or one tier's timed frames.
fn build(name: &str) -> Vec<u8> {
    if name == "h264-1080p-synth.h264" {
        let packets = H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40);
        assert_eq!(packets.len(), 1, "synth holds one packet");
        return packets.into_iter().next().expect("one packet");
    }
    let tier = name
        .strip_prefix("encode-")
        .and_then(|rest| rest.split('-').next())
        .expect("encode-<tier>-...");
    let seeds = encode_seeds(tier);
    let mut bytes = Vec::new();
    for seed in seeds {
        if tier == "sw" {
            bytes.extend_from_slice(&yuv420p_frame(seed));
        } else {
            bytes.extend_from_slice(&nv12_frame(seed));
        }
    }
    bytes
}

/// Artifact filenames in write order.
fn artifacts() -> [&'static str; 7] {
    [
        "h264-1080p-synth.h264",
        "encode-d11-nv12-30f.yuv",
        "encode-d12-nv12-30f.yuv",
        "encode-vk-nv12-30f.yuv",
        "encode-mc-nv12-30f.yuv",
        "encode-va-nv12-120f.yuv",
        "encode-sw-yuv420p-30f.yuv",
    ]
}

/// Usage line for wrong arity.
fn usage() {
    println!("usage: dump_vectors <out-dir> [--check]");
}

/// Write-or-check every artifact under `dir`.
fn run(dir: &str, check: bool) {
    if !check {
        fs::create_dir_all(dir).expect("out dir writable");
    }
    for name in artifacts() {
        let built = build(name);
        let path = Path::new(dir).join(name);
        if check {
            let on_disk = fs::read(&path).expect("dump present");
            assert_eq!(on_disk, built, "{name} matches regeneration");
            println!("{} ok sha256={}", name, FrameHash::of(&built).hex());
        } else {
            fs::write(&path, &built).expect("dump writable");
            let on_disk = fs::read(&path).expect("dump readable");
            assert_eq!(on_disk, built, "{name} round-trips");
            println!(
                "{} {} bytes sha256={}",
                name,
                built.len(),
                FrameHash::of(&built).hex()
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        usage();
        return;
    };
    run(dir, args.get(1).is_some_and(|flag| flag == "--check"));
}
