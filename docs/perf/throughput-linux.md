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

- VA-API: ffmpeg pins renderD128; rv opens the first render node in sorted path order that initializes with H.264 caps, renderD128 on this box.
- Vulkan: rv negotiates one device, the first enumerated adapter that passes the op; ffmpeg runs each GPU. Every rv Vulkan leg below ran on the RTX 5080: RADV rejects the combined usage the tier decodes with and its encode session creation fails ([vulkan](../vulkan.md)), leaving NVIDIA as the only adapter that can complete a leg. Percents compare against the RTX 5080 ffmpeg column.
- TIMEOUT marks an unfinished leg; ERR marks one that errored.
- Knobs are manual in the ffmpeg model; defaults hold landed behavior. See [tuning](../tuning.md).
- Compare against `just benches` or the Windows sheet on counting discipline only: builds, corpora, and vectors differ.

## Hardware

### Decode, fps

VA-API, Radeon

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 600f streams per codec; exact stream bytes unrecorded in pins (run predates the Nov-20 dump inventory; no vector list recorded). Surface rows gpu-resident; readback rows return CPU frames via a split-phase worker at depth 4. |
| Counting | fps = output frames / wall both sides (ffmpeg `-progress frame=` per run, rv decoded frames over fed frames). Median of 5 runs each side, no warmup either side. Quiet box (release, quiet, median of 5 per Gap 2). |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). VA devices per Method (renderD128 both sides). Box per Environment. |
| Gold | ffmpeg VA-API on renderD128 on identical streams per sheet (every percent compares identical streams on this table). Exact command lines unrecorded in pins. Shapes pair per row (surface/readback). |
| Verdict basis | head-to-head delta only; no per-leg budget recorded in pins; absolute bar 60fps (all rows clear). VA batch proof PENDING (Gap 1: 5 hunks, predicted-unmeasured). |

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
| Input | 600f streams per codec; exact stream bytes unrecorded in pins (same caveat as VA-API). h264 ships IPB and IP streams; h265 ships IPB; av1 runs one stream. No h264 surface leg ran on the IP stream. |
| Counting | fps = output frames / wall both sides (ffmpeg `-progress frame=` per run, rv decoded frames over fed frames). Median of 5 runs each side, no warmup either side. Quiet box (release, quiet, median of 5 per Gap 2). TIMEOUT rows: ffmpeg leg unfinished, row stays rv-only with reason. |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). rv on the RTX 5080 (first enumerated adapter passing the op; RADV rejects the combined decode usage). Box per Environment. |
| Gold | ffmpeg runs each GPU; percents compare against the RTX 5080 ffmpeg column. Exact command lines unrecorded in pins. FFmpeg on RADV for context, unpercented: h264 surface 579.2, readback 524.6; h265 surface TIMEOUT, readback 432.5; av1 surface 443.0, readback 373.4; both encodes ERR. RADV h264 surface varies run to run with no ffmpeg-side code change. |
| Verdict basis | head-to-head delta only; no per-leg budget recorded in pins; absolute bar 60fps (all rv rows clear). |

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
| Input | 300f yuv420p at 5 Mbps nominal, fastest sane settings both sides; exact input bytes unrecorded in pins (run predates the Nov-20 dump inventory). h264/h265 IP g60 and all-intra rows; transcode rows pair rv all-intra against ffmpeg IP except the two labeled IP-g60-both-sides rows. |
| Counting | fps = output frames / wall both sides (ffmpeg `-progress frame=` per run, rv counted frames). Median of 5 runs each side, no warmup either side. Quiet box (release, quiet, median of 5 per Gap 2). |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). VA devices per Method (renderD128 both sides). Box per Environment. |
| Gold | ffmpeg VA-API on renderD128 on identical streams per sheet except the two labeled transcode rows. Exact command lines unrecorded in pins. Settings differ (bitrate and QP unmatched), no quality claim; percents diagnose pace. |
| Verdict basis | head-to-head delta only; no per-leg budget recorded in pins; absolute bar 60fps (all rows clear). VA batch proof PENDING (Gap 1). |

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
| Input | 600f streams per codec; exact stream bytes unrecorded in pins (run predates the Nov-20 dump inventory). h264 ships IPB and IP streams; h265 ships IPB; av1, vp9, and vp8 run one stream each. Thread rows per labels. |
| Counting | fps = output frames / wall both sides (ffmpeg `-progress frame=` per run, rv decoded frames over fed frames). Median of 5 runs each side, no warmup either side. Quiet box (release, quiet, median of 5 per Gap 2). |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). CPU legs, no device. Box per Environment. |
| Gold | default SW decoders (ffmpeg 7.1.5 GPL) on identical streams per sheet. Exact command lines unrecorded in pins. Thread rows pair per labels; rv thread plumbing unrecorded in pins. |
| Verdict basis | head-to-head delta only; no per-leg budget recorded in pins; absolute bar 60fps (all rows clear). |

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
| Input | 300f yuv420p at 5 Mbps nominal, fastest sane settings both sides; exact input bytes unrecorded in pins (run predates the Nov-20 dump inventory). |
| Counting | fps = output frames / wall both sides (ffmpeg `-progress frame=` per run, rv counted frames). Median of 5 runs each side, no warmup either side. Quiet box (release, quiet, median of 5 per Gap 2). SVT legs match threads both sides: rv threads map to lp only (4t is lp4, 8t clamps to lp6), and ffmpeg `-threads` is inert for libsvtav1 (its legs set the same lp values). |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). CPU legs, no device. Box per Environment. |
| Gold | libx264/libx265 ultrafast zerolatency, vpx cpu8, aom cpu8, svt p13 la0 lp4/lp6, per row labels; identical streams per sheet. Exact command lines unrecorded in pins. Settings differ, no quality claim. aom cpu10: ffmpeg ERR (Numerical result out of range), no rv path (crate pins cpu8); row one-sided with reason. |
| Verdict basis | head-to-head delta only; no per-leg budget recorded in pins; absolute bar 60fps (paired rows clear). |

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
| Input | vk encodes: 300f yuv420p (exact bytes unrecorded, same caveat); rv actual 77.6/83.1 Mbps vs 75 nominal. NVENC: ffmpeg-side only. openh264: rv at 4t/8t. livepack: synthetic units, no codec bytes. RADV transcode: ffmpeg-side (h264 226.6, h265 ERR); RADV h265 transcode varies run to run with no ffmpeg-side code change. |
| Counting | fps = output frames / wall both sides. Median of 5 runs each side, no warmup either side, livepack included. Quiet box (release, quiet, median of 5 per Gap 2). |
| Build | release; run tree + capture command unrecorded in pins (Gap 2). vk encodes on the RTX 5080 (RADV encode session creation fails); CPU legs need no device. Box per Environment. |
| Gold | no pair per row, with reason: vk encodes (ffmpeg produced empty output); NVENC (no rv tier); openh264 (absent from the ffmpeg build, no libopenh264); livepack (no ffmpeg counterpart); RADV transcode (no rv leg). |
| Verdict basis | reference only; absolute bar 60fps (all values clear; h265 transcode ERR not applicable). |

- Vulkan h264 encode via rv, ffmpeg produced empty output: 543.9 at 77.6 Mbps actual vs 75 nominal.
- Vulkan h265 encode via rv, ffmpeg produced empty output: 236.8 at 83.1 Mbps actual vs 75 nominal.
- NVENC via ffmpeg, no rv tier: h264 499.2, h265 557.3, av1 511.5.
- h264 openh264 via rv, absent from the ffmpeg build: 857.6 at 4t, 846.7 at 8t.
- Livepack via rv, no ffmpeg counterpart: TsMux 2039204.4, TsDemux 1552162.4, FragMux 428935.8.
- RADV transcode via ffmpeg, no rv leg: h264 226.6, h265 ERR.

## Gaps

1. PENDING va vk-template batch lab proof: 5 hunks (surface-encode hold-back, sharded decode readback, sharded encode upload, zero-alloc stage_emit, depth ladder), all PREDICTED-UNMEASURED, uncommitted in tree. Legs per `.tmp/va-opt-report.md`: leg 0 gate (`cargo test -p rust-video-vaapi --lib` plus e2e/overlap/surface/async green; any failure kills the batch, bisect biggest-first); leg 1 transcode IP g60 (baselines h264 220.7, h265 225.6); leg 2 readback rows plus `phase_stats().copy_us`; leg 3 encode rows (h264 301.2/306.0, h265 331.7/332.8); leg 4 stage allocs (noise-band expected); leg 5 depth ladder 1-4 on one readback + one surface leg; leg 6 thread cost (+8 steady threads). In-repo legs: `cargo test -p rust-video-vaapi --test throughput` (`--release` for the headline). Kill rules per leg in the report.
2. HEAD re-grade rides the lab session: published numbers are Nov-18 era (run tree unrecorded) on the doc's box/method (release, quiet, median of 5; decode 600 frames, encode 300).
