# Linux throughput vs FFmpeg

rv-sw decode beat ffmpeg on all 16 software decode legs, +4.9% to +80.8%; sw encode took all 11 comparable legs, +2.4% to +80.4%. rv-vaapi on the Radeon took all four surfaces (+2.9% to +88.6%) plus h264 readback (+2.9%); h265 readback trails 5.2%. VA-API encode lands at h264 -4.7% to -4.3% and h265 +1.9% to +2.0%. Vulkan ran on the RTX 5080; most ffmpeg Vulkan legs timed out or produced empty output, leaving rv-only rows.

## Environment

| Item | Value |
|---|---|
| OS | LMDE 7, Debian Trixie base |
| CPU | 32-core |
| FFmpeg | 7.1.5-0+deb13u1, gcc 14, GPL; +libx264 +libx265 +libvpx +libaom +libsvtav1 +libdav1d +nvenc +vaapi +vulkan; no libopenh264 |
| rustc | 1.96.1 (31fca3adb 2026-06-26) |
| NVIDIA GPU | GeForce RTX 5080, driver 615.71.10 |
| AMD GPU | Radeon RAPHAEL_MENDOCINO, Mesa radeonsi 26.1.6, DRM 3.61 |
| VA-API | libva 1.22.0 / 2.22.0 on renderD128 |
| Vulkan loader order | GPU0 RTX 5080, GPU1 Radeon (RADV), GPU2 llvmpipe |

## Method

- fps = output frames / wall, both sides; ffmpeg `-progress frame=` per run.
- Median of 5 runs each side, no warmup either side; quiet box.
- Budgets: none recorded; absolute bar 60fps.
- VA-API: both sides on renderD128. Vulkan: rv first passing adapter (RTX 5080 here); ffmpeg runs each GPU, percents vs RTX 5080.
- TIMEOUT = unfinished leg; ERR = errored leg.
- Knobs manual (ffmpeg model); defaults hold. See [tuning](../tuning.md).
- Run tree, capture commands, and exact stream bytes unrecorded (Nov-18 era, predates the dump inventory).

## Hardware

### Decode, fps

VA-API, Radeon

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 600f streams per codec (bytes unrecorded); surface rows resident, readback rows via split-phase worker |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; renderD128 both sides; box per Environment |
| Gold | ffmpeg VA-API on renderD128, identical streams; shapes pair per row |
| Verdict basis | head-to-head delta only; bar 60, all rows clear |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 surface | 791.1 | 871.7 | +10.2% |
| h264 readback | 729.7 | 750.5 | +2.9% |
| h265 surface | 573.9 | 590.8 | +2.9% |
| h265 readback | 541.4 | 513.3 | -5.2% |
| av1 surface | 575.9 | 1086.3 | +88.6% |
| av1 readback | 423.8 | 673.7 | +59.0% |
| vp9 surface | 538.0 | 567.3 | +5.4% |
| vp9 readback | 464.3 | 543.8 | +17.1% |

Vulkan, RTX 5080

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 600f streams (bytes unrecorded); h264 ships IPB + IP, rest one stream each |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; rv RTX 5080 (RADV rejects the decode usage); box per Environment |
| Gold | ffmpeg per GPU; percents vs RTX 5080; TIMEOUT rows stay rv-only with reason |
| Verdict basis | head-to-head delta only; bar 60, all rv rows clear |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 surface, IPB stream | TIMEOUT | 2481.9 | no comparison |
| h264 readback, IPB stream | TIMEOUT | 1939.4 | no comparison |
| h264 readback, IP stream | TIMEOUT | 2640.3 | no comparison |
| h265 surface | TIMEOUT | 2108.3 | no comparison |
| h265 readback | TIMEOUT | 1713.2 | no comparison |
| av1 surface | 446.3 | 1708.3 | +282.8% |
| av1 readback | 433.6 | 1638.1 | +277.8% |

### Encode, fps

VA-API, Radeon

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 300f yuv420p, 5 Mbps nominal, fastest sane settings both sides |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; renderD128 both sides; box per Environment |
| Gold | ffmpeg VA-API; settings/bitrate unmatched, pace only; transcode rows labeled per shape |
| Verdict basis | head-to-head delta only; bar 60, all rows clear |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 IP g60 | 316.1 | 301.2 | -4.7% |
| h264 all-intra | 319.7 | 306.0 | -4.3% |
| h265 IP g60 | 325.4 | 331.7 | +1.9% |
| h265 all-intra | 326.2 | 332.8 | +2.0% |
| h264 transcode, rv all-intra vs ffmpeg IP | 234.5 | 221.5 | -5.5% |
| h264 transcode, IP g60 both sides | 234.5 | 220.7 | -5.9% |
| h265 transcode, rv all-intra vs ffmpeg IP | 235.0 | 224.7 | -4.4% |
| h265 transcode, IP g60 both sides | 235.0 | 225.6 | -4.0% |

## Software

### Decode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 600f streams per codec; thread rows per labels |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; CPU legs, no device; box per Environment |
| Gold | default SW decoders, identical streams; thread rows pair per labels |
| Verdict basis | head-to-head delta only; bar 60, all rows clear |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 IPB 1t | 596.2 | 630.4 | +5.7% |
| h264 IPB 4t | 1766.6 | 2033.8 | +15.1% |
| h264 IPB 8t | 2308.4 | 2967.8 | +28.6% |
| h264 IPB auto | 3231.5 | 4757.5 | +47.2% |
| h264 IP 4t | 1710.2 | 2059.4 | +20.4% |
| h264 IP auto | 2969.1 | 5367.7 | +80.8% |
| h265 IPB 4t | 1003.0 | 1051.7 | +4.9% |
| h265 IPB auto | 1499.0 | 1923.7 | +28.3% |
| av1 4t | 1352.0 | 1496.8 | +10.7% |
| av1 auto | 1611.4 | 1879.8 | +16.7% |
| vp9 1t | 670.6 | 708.0 | +5.6% |
| vp9 4t | 1349.5 | 1583.2 | +17.3% |
| vp9 8t | 1364.9 | 1664.7 | +22.0% |
| vp9 auto | 1549.7 | 2538.0 | +63.8% |
| vp8 4t | 1774.7 | 2045.0 | +15.2% |
| vp8 auto | 2741.3 | 4199.2 | +53.2% |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 300f yuv420p, 5 Mbps nominal; SVT legs match threads both sides (rv threads map to lp) |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; CPU legs, no device; box per Environment |
| Gold | libs per row labels, identical streams; settings differ, no quality claim |
| Verdict basis | head-to-head delta only; bar 60, paired rows clear |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| x264 ultrafast zerolat 4t | 773.0 | 926.6 | +19.9% |
| x264 ultrafast zerolat auto | 1055.5 | 1264.0 | +19.8% |
| x265 ultrafast zerolat 4t | 298.3 | 317.6 | +6.5% |
| x265 ultrafast zerolat auto | 294.3 | 321.7 | +9.3% |
| vpx-vp8 cpu8 4t | 154.9 | 160.7 | +3.7% |
| vpx-vp8 cpu8 8t | 166.5 | 170.5 | +2.4% |
| vpx-vp9 cpu8 4t | 378.0 | 498.0 | +31.7% |
| vpx-vp9 cpu8 8t | 420.1 | 530.3 | +26.2% |
| aom cpu8 8t | 172.1 | 176.5 | +2.6% |
| aom cpu10 8t | ERR, Numerical result out of range | no leg, crate pins cpu8 | no comparison |
| svt p13 la0 lp4, matched | 424.8 | 572.1 | +34.7% |
| svt p13 la0 lp6 vs rv 8t, matched | 484.0 | 873.2 | +80.4% |

## One-sided reference, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rows as labeled; vk encodes 300f yuv420p; livepack synthetic units |
| Counting | fps = frames/wall; median of 5, no warmup; quiet box |
| Build | release; vk on RTX 5080; CPU legs need no device; box per Environment |
| Gold | no pair per row, with reason (see rows) |
| Verdict basis | reference only; bar 60, values clear, ERR not applicable |

- Vulkan h264 encode via rv, ffmpeg produced empty output: 543.9 at 77.6 Mbps actual vs 75 nominal.
- Vulkan h265 encode via rv, ffmpeg produced empty output: 236.8 at 83.1 Mbps actual vs 75 nominal.
- NVENC via ffmpeg, no rv tier: h264 499.2, h265 557.3, av1 511.5.
- h264 openh264 via rv, absent from the ffmpeg build: 857.6 at 4t, 846.7 at 8t.
- Livepack via rv CPU, no ffmpeg counterpart: TsMux 2039204.4, TsDemux 1552162.4, FragMux 428935.8.
- RADV transcode via ffmpeg, no rv leg: h264 226.6, h265 ERR.

## Gaps

1. va batch lab proof: 5 hunks predicted-unmeasured, uncommitted in tree; unblocked by lab run; legs per va-opt-report.
2. HEAD re-grade rides the lab session (Nov-18 era numbers; method per this sheet).
