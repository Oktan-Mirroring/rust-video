# rust-video-d3d12

Direct3D 12 video: H.264, H.265, and AV1 decode with H.264 and H.265 encode through the video encoder, plus GPU transcode. Each decoder opens over a probed device.

## Usage

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d12::{D3d12Device, H264Decoder};

fn run(packet: &Packet) -> usize {
    let device = D3d12Device::open().unwrap();
    let mut decoder = H264Decoder::open(&device);
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later, on Windows, with a D3D12 video driver on the machine.

Tier notes live in `../../docs/d3d12-arch.md`.
