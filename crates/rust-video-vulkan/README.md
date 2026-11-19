# rust-video-vulkan

Vulkan video: H.264, H.265, and AV1 decode with H.264 and H.265 encode, plus GPU transcode. Each operation opens a negotiated device for its codec, then decodes packets into frames.

## Usage

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{CodecOp, H264Decoder, VulkanDevice};

fn run(packet: &Packet) -> usize {
    let device = VulkanDevice::open(CodecOp::H264Decode).unwrap().unwrap();
    let mut decoder = H264Decoder::open(&device).unwrap();
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later, with a Vulkan 1.3 driver carrying video for the operation.

Tier notes live in `../../docs/vulkan.md`.
