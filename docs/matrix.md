# Backend matrix

The bar first: a software backend lands only when it sustains realtime
throughput (1080p60 class) on commodity hardware, threaded and SIMD where the
format allows, measured on the clock. Pure-Rust implementations that cannot
hold the frame budget stay out until they can. The facade carries the fast
implementation instead. Versions below were last verified against crates.io
where marked.

## H.264

- Decode: native ffmpeg over the hand sys layer (`H264Decoder`).
  openh264 decode loses: 27/60 vs ffmpeg on B-frame content.
  rusty_h264 is rejected for now: single-threaded and far off realtime.
- Encode: openh264 CBR path (`H264Encoder`), behind the `h264-openh264`
  feature.

## H.265

- Decode: native ffmpeg over the hand sys layer (`H265Decoder`).
  rusty_h265 is rejected for now: decode-only, single-threaded, far off
  realtime.
- Encode: ffmpeg `libx265` wrapper (`H265Encoder`), behind the `gpl`
  feature.

## AV1

- Decode: ffmpeg `libdav1d` wrapper over the hand sys layer (assembly,
  threaded, fastest known path by measurement in `docs/sw-codecs.md`).
  rav1d 1.1.0 joins only if it clears the bar by measurement.
- Encode: ffmpeg `libaom-av1` wrapper over the hand sys layer
  (`Av1Encoder`).

## VP9

- Decode: native ffmpeg over the hand sys layer. Encode: ffmpeg
  `libvpx-vp9` wrapper over the same layer, same system libvpx. No
  direct libvpx bind in the tree.

## AV2

- Decode: no backend ships. rusty_av2d 0.2.11 is the candidate,
  gated behind measurement; no promise until it holds the budget.
  Fenced for untrusted input regardless.

## VP8

- Decode only, native ffmpeg over the hand sys layer (`Vp8Decoder`).
  No encoder ships.

## Hardware tiers

Vulkan, VA-API, D3D11, D3D12, and MediaCodec carry their platform's own
implementation. No software politics apply: the driver is the fast path.

Live now: Vulkan decodes and encodes H.264/H.265 with GPU transcode
between them; VA-API decodes H.264, encodes H.264/H.265, and transcodes
from H.264; D3D11 decodes H.264 and encodes H.264/H.265/VP9/AV1 over
Media Foundation MFTs; D3D12 decodes H.264 plus IDR-only H.265 and
encodes both; MediaCodec decodes all five codecs. The README matrix
names every cell.
