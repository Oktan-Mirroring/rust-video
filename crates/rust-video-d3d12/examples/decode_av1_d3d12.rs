//! AV1 decode probe on the D3D12 tier: profile check plus pending-builder handling.

#[path = "common/probe.rs"]
#[cfg(target_os = "windows")]
mod probe;

#[cfg(target_os = "windows")]
use rust_video_d3d12::{Av1Decoder, Codec};

#[cfg(target_os = "windows")]
fn main() {
    let Some(device) = probe::device() else {
        return;
    };
    probe::run(
        &device,
        Codec::Av1,
        "av1",
        &[0x12, 0x00],
        Av1Decoder::open(&device),
    );
}

#[cfg(not(target_os = "windows"))]
fn main() {}
