# rust-video

[![Crates.io](https://img.shields.io/crates/v/rust-video)](https://crates.io/crates/rust-video)
[![Docs.rs](https://docs.rs/rust-video/badge.svg)](https://docs.rs/rust-video)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)

rust-video is a Rust library for realtime video encoding and decoding. It presents software and platform hardware codecs behind one API, for applications that send or receive live video.

## Scope

In: realtime frame encode and decode for H.264, H.265, AV1, VP9, VP8, the platform tiers that carry them, and livepack packing between the two.

Out: archival demuxing, subtitles, filters, audio.

## Codec support

### Decode

| Backend | H.264 | H.265 | AV1 | VP9 | VP8 |
|---|---|---|---|---|---|
| Software | ● | ● | ● | ● | ● |
| Vulkan | ● | ● | ● | – | – |
| VA-API | ● | ● | ● | ● | – |
| D3D11 | ● | ● | ● | ● | – |
| D3D12 | ● | ● | ● | – | – |
| MediaCodec | ●⁷ | ●⁷ | ●⁷ | ●⁷ | ●⁷ |

### Encode

| Backend | H.264 | H.265 | AV1 | VP9 | VP8 | Xcode |
|---|---|---|---|---|---|---|
| Software | ● | ●¹ | ● | ● | ● | – |
| Vulkan | ● | ● | – | – | – | ●² |
| VA-API | ● | ● | – | – | – | ●³ |
| D3D11 | ●⁴ | ●⁴ | ●⁴ | ●⁴ | – | ●³⁴ |
| D3D12 | ● | ● | – | – | – | ●⁶ |
| MediaCodec | ●⁹ | ●⁹ | ●⁹ | – | – | – |

| Mark | Means |
|---|---|
| ● | live |
| – | absent |

¹ `gpl` feature, libx265.
² `export()` is opaque where no DMA-BUF export exists.
³ H.264 source only.
⁴ Media Foundation MFT, present per machine.
⁶ H.264/H.265 in and out.
⁷ Decoder per device mime; needs the device codec.
⁹ Encoder per device mime; needs the device codec.

## Requirements

Rust 1.87 or later. `rust-video-sw` features select the software backends: `ffmpeg` decodes all five codecs and encodes VP8, VP9, and AV1 through the system libavcodec and libavutil (install the matching development headers); `h264-openh264` encodes H.264 through a vendored openh264; `gpl` encodes H.265 through libx265 and implies `ffmpeg`. On Windows, `FFMPEG_LIB_DIR` must point at `avcodec.lib` and `avutil.lib`.

## Quick start

```bash
just build
just test
```

The decode snippet needs `rust-video-sw` with the `ffmpeg` feature; default features expose no backend.

```rust
use rust_video::Decoder;
use rust_video::Packet;
use rust_video_sw::H264Decoder;
fn run(packet: &Packet) -> usize {
    let mut decoder = H264Decoder::open(0).unwrap();
    let mut total = 0;
    total += decoder.decode(packet).unwrap().len();
    total += decoder.flush().unwrap().len();
    total
}
```

## License

MIT or Apache 2.0, at your choice. See `LICENSE-MIT` and `LICENSE-APACHE`. Third-party attributions live beside the code that needs them.

## Further reading

`docs/architecture.md` describes the system shape.
