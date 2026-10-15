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

| Codec | Decode | Encode | Hardware | Packing |
|---|---|---|---|---|
| H.264 | native ffmpeg | openh264 | Vulkan, VA-API, MediaCodec, D3D11 | TS and fMP4 |
| H.265 | native ffmpeg | libx265 (`gpl` feature) | Vulkan, VA-API stub, MediaCodec, D3D11 stub | TS and fMP4 |
| AV1 | libdav1d wrapper | libaom-av1 wrapper | VA-API stub, MediaCodec, D3D11 stub | fMP4 only |
| VP9 | native ffmpeg | libvpx-vp9 wrapper | VA-API stub, MediaCodec, D3D11 stub | fMP4 only |
| VP8 | native ffmpeg | none | VA-API stub; MediaCodec, D3D11 absent | fMP4 only |

Backend policy lives in `docs/matrix.md`. Hardware detail lives in `docs/vaapi.md` and `docs/mediacodec.md`. Packing detail lives in `docs/livepack-ts-fmp4.md`. AV2 has no backend yet; the gating bar is in `docs/matrix.md`.

## Requirements

Rust 1.87 or later. `rust-video-sw` features select the software backends: `ffmpeg` decodes all five codecs and encodes VP9 and AV1 through the system libavcodec and libavutil (install the matching development headers); `h264-openh264` encodes H.264 through a vendored openh264; `gpl` encodes H.265 through libx265 and implies `ffmpeg`. On Windows, `FFMPEG_LIB_DIR` must point at `avcodec.lib` and `avutil.lib`.

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

## Testing

Each crate carries unit tests beside the modules they cover, run with `just test`. Pixel correctness is pinned by 427 golden vectors compared against ffmpeg output; the runner, generation rules, and coverage gate are documented in `docs/testing/goldens.md`.

## Layout

| Path | Holds |
|---|---|
| `crates/rust-video` | Facade: codec and tier enums, packet and frame types |
| `docs/runtime.md` | Runtime data-plane ownership and flow rules |
| `crates/rust-video-h264` … `-av2` | Codec backends, one crate each |
| `crates/rust-video-vulkan` … `-mediacodec` | Platform tiers, one crate each |
| `crates/rust-video-livepack` | Livepack frame packing |
| `crates/rust-video-harness` | Vector suites and golden oracles |
| `crates/rust-video-caps` | Codec profiles, levels, and caps as types |
| `docs/` | Agent routing and design docs |
| `scripts/` | Vector fetch and probes |
| `corpus/` | Vector corpus, gitignored, via `just vectors` |

## License

MIT or Apache 2.0, at your choice. See `LICENSE-MIT` and `LICENSE-APACHE`. Third-party attributions live beside the code that needs them.

## Further reading

`docs/` holds the canonical detail. Start with `docs/matrix.md` for backend policy, `docs/sw-codecs.md` for software codec notes, `docs/compat.md` for platform compatibility, and `docs/testing/goldens.md` for the test story.
