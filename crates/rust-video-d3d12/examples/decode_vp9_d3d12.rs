//! VP9 decode probe on the D3D12 tier: profile check plus pending-builder handling.

#[path = "common/probe.rs"]
#[cfg(target_os = "windows")]
mod probe;

#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, Vp9Decoder};

#[cfg(target_os = "windows")]
fn main() {
    let Some(device) = probe::device() else {
        return;
    };
    probe::run(
        &device,
        Codec::Vp9,
        "vp9",
        &[0x12, 0x00],
        Vp9Decoder::open(&device),
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {}
