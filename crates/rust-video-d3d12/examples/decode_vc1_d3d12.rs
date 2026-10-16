//! VC-1 decode probe on the D3D12 tier: profile check plus pending-builder handling.

#[path = "common/probe.rs"]
#[cfg(target_os = "windows")]
mod probe;

#[cfg(target_os = "windows")]
use rust_video_d3d12::{Codec, Vc1Decoder};

#[cfg(target_os = "windows")]
fn main() {
    let Some(device) = probe::device() else {
        return;
    };
    probe::run(
        &device,
        Codec::Vc1,
        "vc1",
        &[0, 0, 1, 0x0F],
        Vc1Decoder::open(&device),
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {}
