# rust-video-d3d11

Direct3D 11 video: H.264, H.265, AV1, and VP9 decode with matching encode through the machine's Media Foundation transforms, plus GPU transcode. Each decoder opens over a probed device.

## Usage

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d11::{D3d11Device, H264Decoder};

fn run(packet: &Packet) -> usize {
    let device = D3d11Device::open().unwrap();
    let mut decoder = H264Decoder::open(&device);
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later, on Windows, with a D3D11 video driver on the machine.

Tier notes live in `../../docs/d3d11-arch.md`.
