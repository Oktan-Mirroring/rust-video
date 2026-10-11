# Backend matrix

The bar first: a software backend lands only when it sustains realtime
throughput (1080p60 class) on commodity hardware, threaded and SIMD where the
format allows, measured on the clock. Pure-Rust implementations that cannot
hold the frame budget stay out until they can. The facade carries the fast
implementation instead. Versions below were last verified against crates.io
where marked.

## H.264

- Decode: openh264 (Cisco, SIMD, threaded) or purpose-built minimal ffmpeg sys
  crate in the `ffmpeg-h264-sys` pattern. rusty_h264 is rejected for now:
  single-threaded and far off realtime.
- Encode: openh264 CBR path or purpose-built ffmpeg sys crate.

## H.265

- Decode: purpose-built minimal ffmpeg sys crate. rusty_h265 is rejected for
  now: decode-only, single-threaded, far off realtime.
- Encode: platform hardware tiers; software encode undecided.

## AV1

- Decode: ffmpeg `libdav1d` wrapper over the hand sys layer (assembly,
  threaded, fastest known path by measurement in `docs/sw-codecs.md`).
  rav1d 1.1.0 joins only if it clears the bar by measurement.
- Encode: rav1e where quality beats speed, platform hardware tiers where
  speed wins. SVT-AV1 binding to be evaluated by measurement.

## VP9

- Decode: native ffmpeg over the hand sys layer. Encode: ffmpeg
  `libvpx-vp9` wrapper over the same layer, same system libvpx. No
  direct libvpx bind in the tree.

## AV2

- Decode: rusty_av2d 0.2.11 gated behind measurement; no promise until it
  holds the budget. Fenced for untrusted input regardless.

## VP8

- Decode only, via the VA-API backend. No encoder effort, ever.

## Hardware tiers

Vulkan, VA-API, D3D11, and MediaCodec carry their platform's own
implementation. No software politics apply: the driver is the fast path.
