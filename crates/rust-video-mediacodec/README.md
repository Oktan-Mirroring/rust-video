# rust-video-mediacodec

Android platform video: H.264, H.265, AV1, VP9, and VP8 decode with H.264, H.265, and AV1 encode, each answered by the device codec behind its mime. Decoders open from a format spec; the snippet runs on Android.

## Usage

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_mediacodec::{FormatSpec, MediaCodecDecoder};

fn run(packet: &Packet) -> usize {
    let spec = FormatSpec::h264(64, 64).unwrap();
    let mut decoder = MediaCodecDecoder::open(&spec).unwrap();
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later, building for Android with NDK API 28; devices run API 21 and up.

Tier notes live in `../../docs/mediacodec.md`.
