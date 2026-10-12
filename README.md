# rust-video

[![Crates.io](https://img.shields.io/crates/v/rust-video)](https://crates.io/crates/rust-video)
[![Docs.rs](https://docs.rs/rust-video/badge.svg)](https://docs.rs/rust-video)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE-APACHE)

Fourteen crates ship one hand-written ffmpeg sys layer that every software decoder rides. Direct dav1d and libvpx binds measured 1.4 to 1.9x slower at 4 threads and were deleted.

## Scope

In: realtime frame encode and decode for H.264, H.265, AV1, VP9, VP8, the platform tiers that carry them, and livepack packing between the two.

Out: archival demuxing, subtitles, filters, audio.

`docs/` holds the canonical detail. This file summarizes and links.

## Codec support

| Codec | SW decode | SW encode | HW tiers | Packing | Notes |
|---|---|---|---|---|---|
| H.264 | native ffmpeg, 588 fps 4t | openh264, 272 fps 4t | VA-API full, 60/60 vs ffmpeg; MediaCodec avc, min API 21; D3D11 stub | TS + fMP4 | 8-bit 4:2:0 only; openh264 decode rejected on B-frame content, 27/60 |
| H.265 | native ffmpeg, 563 fps 4t, 361 fps 1t | libx265 wrapper, `gpl` feature, ~290 fps 8t real footage | VA-API stub, declines Unsupported; MediaCodec hevc, min API 21; D3D11 stub | TS + fMP4 | 8-bit 4:2:0 only |
| AV1 | libdav1d wrapper, 604 fps 4t, ~347 fps 1t | libaom-av1 realtime wrapper, ~136 fps 8t real footage | VA-API stub, declines Unsupported; MediaCodec av01, min API 29; D3D11 stub | fMP4 only | TS refused, no registered stream_type; native av1 decoder is hardware-only |
| VP9 | native ffmpeg, 625 fps 4t, 325 fps 1t | libvpx-vp9 wrapper, 368 fps 4t | VA-API stub, declines Unsupported; MediaCodec vp9, min API 21; D3D11 stub | unpacked | same system libvpx under the wrapper |
| VP8 | native ffmpeg, 1336 fps 4t, 444 fps 1t | never | VA-API stub, declines Unsupported; no MediaCodec; D3D11 stub | unpacked | decode only, by design |
| AV2 | none | none | none | unpacked | gated behind measurement |

All fps figures are 1080p on the rig named in `docs/sw-codecs.md`. Hardware detail lives in `docs/vaapi.md` and `docs/mediacodec.md`. Packing detail lives in `docs/livepack-ts-fmp4.md`. Backend policy lives in `docs/matrix.md`.

## Test status

Unit tests run per crate. Current counts: vulkan 120, livepack 74, vaapi 54, caps 47, facade 43, mediacodec 37, sw 30, harness 13.

Goldens: 423 total. VP9 353, VP8 58, AV1 6, H.264 3, H.265 3. Runner and generation rules are in `docs/testing/goldens.md`.

`fmt`, `clippy`, and tests are green on master.

## Limits

Software paths emit 8-bit 4:2:0 only. Anything else returns `SwError`.

MediaCodec: no EOS API, no PTS out, no surface path yet, one copy per plane.

Windows link is unverified here and fails closed without `FFMPEG_LIB_DIR`. See `docs/sw-codecs.md`.

Unmeasured: encode quality (PSNR/bitrate) oracles for AV1 and H.265, RV3/RV4/RV5 oracles.

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

## Build

```bash
just build    # cargo build --workspace
just check    # clippy, warnings denied
just test     # cargo test --workspace
just doc      # API docs, no dependency docs
just ci       # format check plus all of the above
```

Windows compiles in CI only; see `.github/workflows/ci.yml`.

## References

- ITU-T H.264 (01/2012) and H.265 V10 (07/2024): profiles, tiers, levels.
- AOMedia AV1 specification: profiles, levels, OBU, decoder model.
- AOMedia AV2 specification v1.0.0: <https://av2.aomedia.org/v1.0.0/>
- VP9 v0.7 specification and levels: <https://www.webmproject.org/vp9/levels/>
- RFC 6386: VP8 data format.
- bytes 1.12.1: <https://docs.rs/bytes/1.12.1/> (refcounted packet and plane backing).

## License

MIT or Apache 2.0, at your choice. See `LICENSE-MIT` and `LICENSE-APACHE`.
