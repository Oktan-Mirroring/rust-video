# rust-video-vaapi

VA-API video over DRM render nodes: H.264, H.265, AV1, and VP9 decode with H.264 and H.265 encode, plus GPU transcode. Each decoder opens over a probed device.

## Usage

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vaapi::{H264Decoder, VaApiDevice};

fn run(packet: &Packet) -> usize {
    let device = VaApiDevice::open().unwrap();
    let mut decoder = H264Decoder::open(&device);
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later, on Linux, with a DRM render node offering the codec profile.

Tier notes live in `../../docs/vaapi.md`.
