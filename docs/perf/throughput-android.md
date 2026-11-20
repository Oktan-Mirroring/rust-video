# Android throughput vs FFmpeg

rv trails gold MediaCodec decode on four codecs (vp9 widest: 44 vs 150 fps) and beats it on vp8 sync (95 vs 83) plus h264/h265 noise encode (+25.9%, +21.0%). Sync legs download frames both sides; surface legs are rv-only and software decode is gold-only. Encode settings differ: percents diagnose only.

On-device run 2026-11-20, POCO F5 Pro, rv debug test profile single 30-iter legs, gold median of 3 after a discarded warmup.

## Environment

| Item | Value |
|---|---|
| Phone | POCO F5 Pro (23013PC75G), Android 15, Snapdragon 8+ Gen 1 (SM8475) |
| FFmpeg gold | 9.0 static aarch64, NDK clang 21, API 21, mediacodec + SW decoders, no SW av1 decoder |
| FFmpeg enc-helper | Gold config + rawvideo decoder + dump_extradata/metadata bsfs |
| rv tree | 83b9284 (2026-11-20) plus uncommitted changes |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |

## Method

- rv legs run on-device (adb shell, vectors at /data/local/tmp/vectors), one 30-iteration run per leg; fps comes from the harness print, 10 of 13 legs clear the 60fps bar.
- Gold decode: static gold binary, `-benchmark`, `-f null -`; SW decoders on 30f concats, MediaCodec decoders on 120f _ip inputs (vp8 30f).
- Gold encode: enc-helper sibling binary on noise-30.mkv (30f 1080p yuv420p), `-g 30`, one MediaCodec encoder per codec.
- fps divides output frames by wall both sides; gold takes the median of 3 timed runs (warmup discarded, 1s settle after warmup, 2s between runs), rv reports one run each.
- Short phone runs vary with DVFS; medians and settles contain it, they do not remove it.

## Hardware

### Decode, fps

Sync legs, downloaded frames both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 145.8 | 80.2 | -45.0% |
| h265 | 135.4 | 102.5 | -24.3% |
| vp8 | 83.3 | 94.8 | +13.8% |
| vp9 | 150.2 | 44.2 | -70.6% |
| av1 | 167.4 | 152.2 | -9.1% |

Gold HW inputs are 120f _ip (vp8 30f); rv submits its own vectors 30x: adjacent, not identical. rv vp9 sync misses the 60 bar at 44.2 while gold holds 150.2.

### Encode, fps

Noise input both sides, settings differ, percents diagnose only:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 57.5 | 72.4 | +25.9% |
| h265 | 57.7 | 69.8 | +21.0% |
| av1 | 5.4 | 11.4 | +111.1% |

Both av1 legs miss 60 by distance at software speed. rv ships no vp8/vp9 encode legs.

## One-sided reference, fps

- rv surface decode, no gold counterpart: h264 87.7, h265 103.6, vp8 78.5, vp9 44.6, av1 129.5.
- Gold SW decode, no rv counterpart: h264 103.8, h265 159.6, vp8 94.9, vp9 11.5; gold ships no SW av1 decoder.
- Gold vp8/vp9 encode, no rv legs: 21.2, 12.1.

## Comparability notes

- Content: gold SW legs run 30f concats of the vectors rv submits 30x, identical bytes. HW legs are _ip-adjacent: gold needs multi-frame _ip (its single-frame HW flush asserts), rv submits its own vectors.
- Shape: gold HW to null downloads frames, which matches the rv sync leg; surface legs stay one-sided.
- Gaps: round-1 single-frame runs discarded (setup noise, HW flush-assert edge); rv has no vp8/vp9 encode legs; gold has no SW av1 decoder.
- Cross-OS: the Windows sheet differs in build, corpus, and vectors: counting discipline only.
