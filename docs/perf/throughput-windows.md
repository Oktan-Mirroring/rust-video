# Windows throughput vs FFmpeg

Same-bytes gold inverts the encode table: all six rv legs beat NVENC on shared noise (d11 h264 388 vs 92 fps), pace only, settings and quality unmatched. Decode: d3d12 h265 surface +79% (2009 vs 1120), vulkan h264 sustained +116% (3021 vs 1398). Every pair shares bytes except h264 sync (gold cannot download the synth stream); software legs stay ffmpeg one-sided.

Release test profile, ITERS=30 sync legs, 120-frame IP sustained legs (d11 all four and d12 hevc/av1 prime 4 packets untimed, d12 h264 and vk time the cold pass; d12 sync legs prime 4 passes), rv median of 3 on d3d11 else mean of 2 (d11 av1 decode isolated: full-binary collapses runs and discounts clean ones), gold medians this run, single era (1 discarded warmup, 3 timed HW runs, 5 timed SW runs; _ip confirms within 6% of rebaseline), quiet box, run 2026-11-20.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| CPU | Ryzen 9 9950X3D, 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 31C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, unselected) |

## Method

- Sync legs run `just benches-release` (release test profile, ITERS=30, committed vectors). Sustained legs run one 120-frame IP pass per tier overlap pump, exact counts, zero drops. Our-side bytes are bit-identical to the builder era: the synth H.264 packet is a committed 30-byte blob (`sized_blob_matches_builder` pins it), rv numbers carried.
- Gold decode: cuvid resident (`-f null`) plus d3d11va downloaded (hwdownload + rawvideo file) per codec, all on our bytes: 120x single-frame concats for sync/surface shape, `_ip` 120f for sustained. H264 synth runs cuvid resident only: d3d11va reports a corrupt frame and writes nothing, cuvid plus hwdownload is unimplemented in this build.
- Gold encode: NVENC p1 intra CQP qp 28 (AV1 IP g4; all-intra refused) on per-tier 120f noise dumps (4x concat of each tier's timed frames; va native 120f). SW encode runs the 10x sw dump (300f yuv420p) at 5 Mbps nominal, vpx realtime.
- fps divides actual output frames by wall both sides; ffmpeg parses `speed=` against the 30fps input. Validity: the media clock implies 30fps output within 5% and frame counts are exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms). rv median of 3 on d3d11 else mean of 2.
- Software decode runs 600f our bytes: 5x `_ip` cats (vp8 600x single-frame, no `_ip` exists). The h264 IP-only legs are dropped: no our-file counterpart.
- Devices: d3d11/d3d12 open the default hardware adapter (RTX 5080 here); rv-vulkan takes the first enumerated adapter passing the op (NVIDIA here); ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Compare against the Linux sheet on counting discipline only: build, corpus, and vectors differ.

## Software

### Decode, fps

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

rv has no legs: the ffmpeg/gpl backends need linkable libav*, absent from the binaries-only build.

### Encode, fps

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

No two-sided legs. OpenH264 runs rv-only; see One-sided reference.

## Hardware

### Decode, fps

Surface, gpu-resident both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1461 | 3556 | +143.4% |
| d3d12 h264 surface | 1461 | 4955 | +239.2% |
| vulkan h264 surface | 1461 | 3724 | +154.9% |
| d3d11 h265 surface | 1120 | 1538 | +37.3% |
| d3d12 h265 surface | 1120 | 2009 | +79.4% |
| vulkan h265 surface | 1120 | 1479 | +32.1% |
| d3d11 av1 surface | 819 | 1091 | +33.2% |
| d3d12 av1 surface | 819 | 1330 | +62.4% |
| vulkan av1 surface | 819 | 1270 | +55.1% |
| d3d11 vp9 surface | 250.2 | 262 | +4.7% |

Every pair shares bytes: gold decodes the 120x single-frame concat. d3d11 vp9 decode is NVDEC both sides (ours via the D3D11 VP9 decoder profile); software MFT applies to the VP9 encoder only. Vulkan h264 spans 3660-3788 across its 2 runs; d3d11 h264 spans 3490-3657 across its 3 (median grades it). h265 gold 1120 pools two medians (1074/1167, 18% pooled spread).

Sustained overlap, 120-frame IP both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1398 | 3021 | +116.1% |
| vulkan h265 sustained | 1293 | 1769 | +36.8% |
| vulkan av1 sustained | 1197 | 2014 | +68.3% |
| d3d11 h264 sustained | 1398 | 1909 | +36.6% |
| d3d11 h265 sustained | 1293 | 1712 | +32.4% |
| d3d11 av1 sustained | 1197 | 2060 | +72.1% |
| d3d11 vp9 sustained | 1017 | 1486 | +46.1% |
| d3d12 h264 sustained | 1398 | 1017 | -27.3% |
| d3d12 h265 sustained | 1293 | 2078 | +60.7% |
| d3d12 av1 sustained | 1197 | 1322 | +10.4% |

Every leg shares gold's exact bytes; gold stays resident while rv legs hand CPU frames back: each percent favors gold. Downloaded pairs per codec are the shape-matched d3d legs: gold 810/765/825/792 against rv d3d11 sustained 1909/1712/2060/1486 (h264/h265/av1/vp9), gold with a 373 MB file write, rv with CPU frames in hand.

Sync, downloaded both sides (`†` = adjacent bytes, unpercented):

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 † | 810 | 2294 | adjacent |
| d3d11 h265 | 873 | 1161 | +33.0% |
| d3d11 av1 | 837 | 904 | +8.0% |
| d3d11 vp9 | 262.8 | 115 | -56.2% |
| d3d12 h264 † | 810 | 2343 | adjacent |
| d3d12 h265 | 873 | 1409 | +61.4% |
| d3d12 av1 | 837 | 1010 | +20.7% |
| vulkan h264 † | 810 | 2853 | adjacent |
| vulkan h265 | 873 | 1431 | +63.9% |
| vulkan av1 | 837 | 1226 | +46.5% |

Gold downloads the 120x single-frame concat to a file; h264 rows show the `_ip` downloaded number (gold cannot download synth bytes). d11 av1 decode reports the isolated-leg median (904): full-binary runs collapse it to ~100fps and discount clean runs ~20% (contention, reproduced across rounds).

### Encode, fps

Gold runs intra CQP qp 28 (AV1 IP g4) on each tier's own noise dump; rv runs MFT/Video-Encode/VkVideo defaults on the same frames. Settings differ: percents diagnose only, no quality claim.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 92.1 | 388 | +321.3% |
| d3d11 h265 | 141.6 | 690 | +387.3% |
| d3d11 av1 | 157.8 | 547 | +246.6% |
| d3d12 h264 | 92.4 | 103 | +11.5% |
| vulkan h264 | 92.4 | 498 | +439.0% |
| vulkan h265 | 141.6 | 229 | +61.7% |

Noise intra is brutal for CQP: gold falls from 549/456/618 on testsrc to 92/142/158 on noise, and every rv leg outpaces it per frame at unknown quality. d3d12 h264 clears gold by 11.5% near the noise entropy-coding floor (~125); vulkan h265 clears it by 61.7% above the NVIDIA VkVideo submit floor (4.4 ms, ffmpeg-confirmed).

## One-sided reference, fps

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 686, h265 957, av1 724.
- Transcode via rv, no ffmpeg counterpart: d3d11 424, d3d12 622.
- h264 openh264 via rv, absent from the ffmpeg build: 96.2.
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.

## Comparability notes

- Content: every pair shares bytes except h264 sync/downloaded (gold `_ip` vs rv synth: adjacent) and dropped h264-IP SW legs. Resident pairs match shape (surface) and downloaded pairs match shape (sync, sustained-vs-downloaded); sustained-vs-resident favors gold (rv hands CPU frames back).
- Settings: encode legs do not match settings or bitrate and carry no quality claim.
- Gaps: AV1 all-intra NVENC refused; sw legs are ffmpeg one-sided (rv needs linkable libav*, absent here); d3d12 h265 encode declines (driver refuses the HEVC encode config on this box); d3d12 sustained has no vp9 leg (pending picture builder).
- Cross-OS: the Linux sheet differs in build, corpus, and statistic: same counting discipline only.
