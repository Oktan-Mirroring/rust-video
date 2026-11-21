# Windows throughput vs FFmpeg

d11 h264 surface medians 5421 fps against gold 1461 (+271%) on identical bytes; d12 h264 sustained reads 2435 (+74%) on the primed steady method. Encode inverts 6/6 on shared noise (d11 h264 393 vs 92), pace only, settings and quality unmatched. F7 is dead: the 96 fps floor came from a C-only openh264 link, the nasm build runs 256.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| CPU | Ryzen 9 9950X3D, 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 31C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, unselected) |
| rv tree | 1c3a1e8 |

## Method

- Release benches (`just benches-release`), then per-tier `--nocapture` captures.
- fps = output frames / wall, both sides; ffmpeg `speed=` x input rate.
- rv d11 median-of-3, d12/vk mean-of-2; gold median of 3 after 1 discarded warmup.
- Quiet box for every graduating sample; spiked samples discarded and re-run.
- Budgets: 1.5x rebaseline best per leg; absolute bar 60fps.
- Devices: d3d11/d3d12 default adapter; vk first passing adapter; ffmpeg pins cuvid/nvenc/d3d11va.
- Knobs manual (ffmpeg model); defaults hold. See [tuning](../tuning.md).
- Cross-OS: counting discipline only; builds, corpora, vectors differ.

## Hardware

### Decode, fps

Surface

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 1 heavy intra frame x 30 sequential passes<br>h264 30B synth, h265 21KB, av1 45KB, vp9 783KB; hashes: PROVENANCE |
| Counting | fps = frames/wall; d11 median-of-3, d12/vk mean-of-2; quiet box |
| Build | release (x86-64-v3, thin LTO); tree 1c3a1e8; box per Environment |
| Gold | cuvid resident on 120x same-blob concat; gpu-resident both sides |
| Verdict basis | budget 1.5x rebaseline; bar 60; misses exempt: F20, F21, F22 |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1461 | 5421 | +271.0% |
| d3d12 h264 surface | 1461 | 4724 | +223.3% |
| vulkan h264 surface | 1461 | 3724 | +154.9% |
| d3d11 h265 surface | 1120 | 1822 | +62.7% |
| d3d12 h265 surface | 1120 | 1958 | +74.8% |
| vulkan h265 surface | 1120 | 1479 | +32.1% |
| d3d11 av1 surface | 819 | 1280 | +56.3% |
| d3d12 av1 surface | 819 | 1310 | +60.0% |
| vulkan av1 surface | 819 | 1270 | +55.1% |
| d3d11 vp9 surface | 250.2 | 270 | +7.9% |

Sustained overlap

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 120 light inter frames, 1 overlapped pass<br>committed _ip streams; d11/d12 prime 4 packets untimed, vk times cold |
| Counting | fps = frames/wall; exact counts, zero drops; quiet box |
| Build | release (x86-64-v3, thin LTO); tree 1c3a1e8; box per Environment |
| Gold | cuvid resident on same _ip bytes; downloaded shape-matched pair per codec |
| Verdict basis | budget 1.5x rebaseline; bar 60; all GREEN |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1398 | 3021 | +116.1% |
| vulkan h265 sustained | 1293 | 1769 | +36.8% |
| vulkan av1 sustained | 1197 | 2014 | +68.3% |
| d3d11 h264 sustained | 1398 | 1954 | +39.8% |
| d3d11 h265 sustained | 1293 | 1719 | +32.9% |
| d3d11 av1 sustained | 1197 | 2095 | +75.0% |
| d3d11 vp9 sustained | 1017 | 1501 | +47.6% |
| d3d12 h264 sustained | 1398 | 2435 | +74.2% |
| d3d12 h265 sustained | 1293 | 2064 | +59.6% |
| d3d12 av1 sustained | 1197 | 2305 | +92.6% |

Sync

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 30 passes over the single-frame blob; downloaded frames both sides<br>h264 rows (†) adjacent, unpercented: gold shows _ip (synth undownloadable) |
| Counting | fps = frames/wall; isolated grading only on full-binary collapse |
| Build | release (x86-64-v3, thin LTO); tree 1c3a1e8; box per Environment |
| Gold | d3d11va downloaded on 120x concat; same bytes except h264 † |
| Verdict basis | budgets GREEN; vp9 trails gold -6% on identical bytes |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 † | 810 | 3764 | adjacent |
| d3d11 h265 | 873 | 1321 | +51.3% |
| d3d11 av1 | 837 | 1038 | +24.0% |
| d3d11 vp9 | 262.8 | 247 | -6.0% |
| d3d12 h264 † | 810 | 2495 | adjacent |
| d3d12 h265 | 873 | 1481 | +69.6% |
| d3d12 av1 | 837 | 1074 | +28.3% |
| vulkan h264 † | 810 | 2853 | adjacent |
| vulkan h265 | 873 | 1431 | +63.9% |
| vulkan av1 | 837 | 1226 | +46.5% |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 30 timed noise frames per leg (seeds alias pairwise; both sides share dumps) |
| Counting | fps = frames/wall; quiet box |
| Build | release (x86-64-v3, thin LTO); tree 1c3a1e8; box per Environment |
| Gold | NVENC p1 intra CQP on 4x concat of each tier's frames; settings unmatched, pace only |
| Verdict basis | floor-bound throughout: F14 (d11), F3 (d12), F8/F4 (vk) |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 92.1 | 393 | +326.7% |
| d3d11 h265 | 141.6 | 686 | +384.5% |
| d3d11 av1 | 157.8 | 538 | +240.9% |
| d3d12 h264 | 92.4 | 104 | +12.6% |
| vulkan h264 | 92.4 | 498 | +439.0% |
| vulkan h265 | 141.6 | 229 | +61.7% |

## Engine occupancy

**Fact sheet**

| Field | Fact |
|---|---|
| Input | same 1080p legs at 1440p/4K resynth (scratch vectors, recipes in ladder record) |
| Counting | fps + NVDEC% via dmon, 3 runs per cell; quiet box |
| Build | base tree audited; box per Environment |
| Gold | none: rv-only probe |
| Verdict basis | saturation unobserved ≤4K; host/parse/cap walls arrive first |

| Tier | Engine signal | Wall that arrives first |
|---|---|---|
| d11 | vp9 surface tracks pixels to 4K (~30% NVDEC) | av1 host cliff; h265 readback floor; NVENC capped or software |
| d12 | av1 linear to 4K at low duty | h265 4K parse wall; NVENC physics (F3); h265 encode no HW path |
| vk | all legs 2.8-3.7x for 4x pixels | NVENC floors (F4, F8); no wall in sight at 4K |

## Software

### Decode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | gold 600f our bytes per codec; thread rows per labels |
| Counting | fps = frames/wall; gold median of 5 after 1 warmup |
| Build | gold side only: ffmpeg 9.0 gyan binaries on Environment box |
| Gold | this table is the gold reference; default SW decoders |
| Verdict basis | one-sided: rv needs linkable libav*, absent here |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 IPB 1t | 2625.0 | no leg | no comparison |
| h264 IPB 4t | 5760.0 | no leg | no comparison |
| h264 IPB 8t | 7050.0 | no leg | no comparison |
| h264 IPB auto | 6450.0 | no leg | no comparison |
| h265 IPB 4t | 2778.0 | no leg | no comparison |
| h265 IPB auto | 4950.0 | no leg | no comparison |
| av1 4t | 3030.0 | no leg | no comparison |
| av1 auto | 3360.0 | no leg | no comparison |
| vp9 1t | 1566.0 | no leg | no comparison |
| vp9 4t | 3810.0 | no leg | no comparison |
| vp9 8t | 5190.0 | no leg | no comparison |
| vp9 auto | 5280.0 | no leg | no comparison |
| vp8 4t | 1107.0 | no leg | no comparison |
| vp8 auto | 3840.0 | no leg | no comparison |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 300f yuv420p, 5 Mbps nominal, fastest sane settings both sides |
| Counting | fps = frames/wall; gold median of 5 after 1 warmup |
| Build | gold side only: ffmpeg 9.0 gyan binaries on Environment box |
| Gold | this table is the gold reference; settings per row labels |
| Verdict basis | one-sided: rv needs linkable libav*, absent here |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| x264 ultrafast zerolat 4t | 720.0 | no leg | no comparison |
| x264 ultrafast zerolat auto | 930.0 | no leg | no comparison |
| x265 ultrafast zerolat 4t | 108.9 | no leg | no comparison |
| x265 ultrafast zerolat auto | 108.6 | no leg | no comparison |
| vpx-vp8 cpu8 rt 4t | 88.2 | no leg | no comparison |
| vpx-vp8 cpu8 rt 8t | 108.3 | no leg | no comparison |
| vpx-vp9 cpu8 rt 4t | 120.0 | no leg | no comparison |
| vpx-vp9 cpu8 rt 8t | 122.7 | no leg | no comparison |
| aom cpu8 8t | 19.1 | no leg | no comparison |
| aom cpu10 8t | ERR Result too large | no leg, crate pins cpu8 | no comparison |
| svt p13 la0 lp4 | 209.1 | no leg | no comparison |
| svt p13 la0 lp6 | 267.3 | no leg | no comparison |

## One-sided reference, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rows as labeled; no shared input across rows |
| Counting | fps = output frames / wall; quiet box |
| Build | release (x86-64-v3, thin LTO); tree 1c3a1e8; box per Environment |
| Gold | no ffmpeg counterpart on any row |
| Verdict basis | reference only; bar 60, all rows clear |

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 653, h265 922, av1 722.
- Transcode via rv, no ffmpeg counterpart: d3d11 454, d3d12 622.
- h264 openh264 via rv, absent from the ffmpeg build: 256.
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.
- livepack via rv, no ffmpeg counterpart (isolated singles): TsMux 502432, TsDemux 502015, FragMux 166447.

## Floors

Proof records live in `.tmp/` (perf-final, perf-verify, perf-verify2, handoff, ladder, roofline); bare hashes are commits.

| # | Claim | Proof | Status |
|---|---|---|---|
| F1 | d11 h264 surface pace ~285us | perf-final §F1; e93b61d | VACATED, leg GREEN |
| F2 | d12 h264 surface 2112 silicon max | perf-final §F2 (B-sample 2243 beats it) | MOOT, leg GREEN |
| F3 | d12 h264 encode 9.65ms NVENC physics | roofline; verify2 spot | CONFIRMED |
| F4 | vk h265 encode 4.3ms driver floor | roofline; verify2 spot; ladder | CONFIRMED |
| F5 | d11 vp9 encode software MFT floor | roofline; verify2 spot | CONFIRMED |
| F6 | d11 h264 sustained 1103 walk | roofline; VACATED (leg 1430+); ada9dea | VACATED, leg GREEN |
| F7 | sw openh264 10.8ms parallel floor | DEAD: C-only link artifact; nasm build 256 | DEAD, leg GREEN |
| F8 | vk h264 encode ladder cap, API floor 1.22ms | perf-final §F8; verify2 spot | CONFIRMED |
| F9-11 | d12 sync floors (poisoned inputs) | VACATED-MOOT (prime+pool legs GREEN) | VACATED, legs GREEN |
| F12 | d12 h264 sustained engine 707us | SUPERSEDED by 5c0eaeb (2503); span-vs-service correction on record | VACATED, leg GREEN |
| F13 | vk av1 sustained ~2100 asymptote | perf-final §F13; verify2 spot | CONFIRMED |
| F14 | d11 MFT Auto cap, byte-exact legacy | perf-final §F14; verify2 spot | CONFIRMED |
| F15 | vk h264 surface 239-244us vs 217.4 budget | verify §F15; verify2 spot | CONFIRMED |
| F16 | vk h265 surface 605-614us vs 505.1 budget | verify §F16; verify2 spot | CONFIRMED |
| F17 | vk h264 sustained gaps 280-286us vs 247.8 | verify §F17; verify2 spot | CONFIRMED |
| F18 | vk h265 sustained gaps 510-517us vs 380.1 | verify §F18; verify2 spot | CONFIRMED |
| F19 | d11 h265 decode additive floor | verify §F19; VACATED (leg GREEN full) | VACATED, leg GREEN |
| F20 | d11 h265 surface submit+wait vs 510.2 budget | handoff direct split (1c3a1e8 print) | CONFIRMED |
| F21 | d11 av1 surface submit+wait vs 695.4 budget | handoff direct split; tail cannot reach | CONFIRMED |
| F22 | d12 av1 surface engine 691-694us vs 677.1 budget | verify §F22; verify2 spot | CONFIRMED |

## Gaps

1. d12 h265 encode: no HW path on this adapter.
2. d12 VP9: no picture builder (no sync/surface/sustained legs).
3. d11 vp9 surface-encode: no surface import.
4. d12 h265 4K: parse fails on ffmpeg-valid stream (ladder BLOCKED).
5. d11 av1 4K host cliff: 14-16x collapse at idle engine.
6. d11 h265 surface 4K non-monotonic: 1686 beats 1440p 1070.
7. sw ffmpeg/gpl legs on Windows: need linkable libav*.
8. va batch lab proof: 5 hunks predicted-unmeasured; legs per va-opt-report.
9. mc phone proof: held diffs; legs per mc-opt-report; emulator-directional only so far.
