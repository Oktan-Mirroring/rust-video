//! AV1 decode probe on the D3D12 tier: profile check plus pending-builder handling.

#[path = "common/probe.rs"]
mod probe;

use rust_video_d3d12::{Av1Decoder, Codec};

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
