//! MPEG-2 decode probe on the D3D12 tier: profile check plus pending-builder handling.

#[path = "common/probe.rs"]
mod probe;

use rust_video_d3d12::{Codec, Mpeg2Decoder};

fn main() {
    let Some(device) = probe::device() else {
        return;
    };
    probe::run(
        &device,
        Codec::Mpeg2,
        "mpeg2",
        &[0, 0, 1, 0xB3],
        Mpeg2Decoder::open(&device),
    );
}
