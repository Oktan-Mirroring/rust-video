# Windows throughput vs FFmpeg

d3d12 h265 surface matches ffmpeg NVDEC frame for frame (1300 vs 1290 fps); vulkan sustained overlap beats it on identical bytes (3047 vs 1458). Sustained legs share gold bytes, with a downloaded pair per codec; sync and surface legs run legacy vectors against gold testsrc. Software legs are ffmpeg one-sided. Encode settings differ both sides: encode percents diagnose only.

Release test profile, ITERS=30 sync legs, 120-frame IP sustained legs, rv median of 3 on d3d11 else mean of 2, gold rebaseline medians reused (vectors unchanged), quiet box, run 2026-11-20.

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

- Sync legs run `just benches-release` (release test profile, ITERS=30, committed noise-IDR/synth vectors). Sustained legs run one 120-frame IP pass per tier overlap pump, exact counts, zero drops.
- Gold decode: ffmpeg cuvid per codec on testsrc 1080p120 IP streams, gpu-resident (`-f null`) unless noted; downloaded legs per codec (d3d11va + hwdownload + rawvideo file).
- Gold encode: ffmpeg NVENC p1 from a raw YUV file (lavfi generation starves the encoder), intra CQP qp 28 except AV1 IP g4 (all-intra refused).
- fps divides actual output frames by wall both sides; ffmpeg parses `speed=` against the 30fps input. rv median of 3 on d3d11 else mean of 2; gold rebaseline medians reused (no vector changed since, one discarded warmup per leg then).
- Software legs run ffmpeg 9.0 gyan on 600-frame testsrc2 1080p30 streams (decode: h264/h265 ultrafast, svtav1 preset 8, vpx crf 30) and 300-frame raw YUV input (encode at 5 Mbps nominal, vpx realtime); fps divides frames by wall, median of 5.
- Devices: d3d11/d3d12 open the default hardware adapter (RTX 5080 here); rv-vulkan takes the first enumerated adapter passing the op (NVIDIA here); ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Compare against the Linux sheet on counting discipline only: build, corpus, and vectors differ.

## Software

### Decode, fps

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 IPB 1t | 789.0 | no leg | no comparison |
| h264 IPB 4t | 2355.0 | no leg | no comparison |
| h264 IPB 8t | 4020.0 | no leg | no comparison |
| h264 IPB auto | 4710.0 | no leg | no comparison |
| h264 IP 4t | 2745.0 | no leg | no comparison |
| h264 IP auto | 5430.0 | no leg | no comparison |
| h265 IPB 4t | 1218.0 | no leg | no comparison |
| h265 IPB auto | 3180.0 | no leg | no comparison |
| av1 4t | 804.0 | no leg | no comparison |
| av1 auto | 1356.0 | no leg | no comparison |
| vp9 1t | 384.0 | no leg | no comparison |
| vp9 4t | 687.0 | no leg | no comparison |
| vp9 8t | 777.0 | no leg | no comparison |
| vp9 auto | 1086.0 | no leg | no comparison |
| vp8 4t | 3090.0 | no leg | no comparison |
| vp8 auto | 7140.0 | no leg | no comparison |

rv has no legs: the ffmpeg/gpl backends need linkable libav*, absent from the binaries-only build.

### Encode, fps

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| x264 ultrafast zerolat 4t | 921.0 | no leg | no comparison |
| x264 ultrafast zerolat auto | 960.0 | no leg | no comparison |
| x265 ultrafast zerolat 4t | 159.0 | no leg | no comparison |
| x265 ultrafast zerolat auto | 171.9 | no leg | no comparison |
| vpx-vp8 cpu8 rt 4t | 168.9 | no leg | no comparison |
| vpx-vp8 cpu8 rt 8t | 176.4 | no leg | no comparison |
| vpx-vp9 cpu8 rt 4t | 345.0 | no leg | no comparison |
| vpx-vp9 cpu8 rt 8t | 342.0 | no leg | no comparison |
| aom cpu8 8t | 24.6 | no leg | no comparison |
| aom cpu10 8t | ERR Result too large | no leg, crate pins cpu8 | no comparison |
| svt p13 la0 lp4 | 441.0 | no leg | no comparison |
| svt p13 la0 lp6 | 444.0 | no leg | no comparison |

No two-sided legs. OpenH264 runs rv-only; see One-sided reference.

## Hardware

### Decode, fps

Surface, gpu-resident both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1458 | 2431 | +66.7% |
| d3d12 h264 surface | 1458 | 2193 | +50.4% |
| vulkan h264 surface | 1458 | 3713 | +154.7% |
| d3d11 h265 surface | 1290 | 1504 | +16.6% |
| d3d12 h265 surface | 1290 | 1300 | +0.8% |
| vulkan h265 surface | 1290 | 1551 | +20.2% |
| d3d11 av1 surface | 1242 | 921 | -25.8% |
| d3d12 av1 surface | 1242 | 986 | -20.6% |
| vulkan av1 surface | 1242 | 1275 | +2.7% |
| d3d11 vp9 surface, MFT software | 1080 | 267 | -75.3% |

d3d12 h264 runs easier bytes (flat synth IDR vs gold testsrc IP). d3d11 vp9 is MFT software against NVDEC silicon. Vulkan h264 spans 3676-3750 across its 2 runs; d3d11 h264 spans 1896-3508 across its 3 (median grades it).

Sustained overlap, 120-frame IP both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1458 | 3047 | +109.0% |
| vulkan h265 sustained | 1290 | 1749 | +35.6% |
| vulkan av1 sustained | 1242 | 1969 | +58.5% |
| d3d11 h264 sustained | 1458 | 1048 | -28.1% |
| d3d11 h265 sustained | 1290 | 1021 | -20.9% |
| d3d11 av1 sustained | 1242 | 1153 | -7.2% |
| d3d11 vp9 sustained | 1080 | 941 | -12.9% |
| d3d12 h264 sustained | 1458 | 1028 | -29.5% |
| d3d12 h265 sustained | 1290 | 957 | -25.8% |
| d3d12 av1 sustained | 1242 | 734 | -40.9% |

Every leg shares gold's exact bytes; gold stays resident while rv legs hand CPU frames back: each percent favors gold. Downloaded pairs per codec are the shape-matched d3d legs: gold 825/789/822/846 against rv d3d11 sustained 1048/1021/1153/941 (h264/h265/av1/vp9), gold with a 373 MB file write, rv with CPU frames in hand.

Sync, rv legs with gold alongside (shapes differ; unpercented):

| Leg | FFmpeg resident | rv sync |
|---|---|---|
| d3d11 h264 | 1458 | 1524 |
| d3d11 h265 | 1290 | 834 |
| d3d11 av1 | 1242 | 595 |
| d3d11 vp9 | 1080 | 144 |
| d3d12 h264 | 1458 | 1109 |
| d3d12 h265 | 1290 | 831 |
| d3d12 av1 | 1242 | 670 |
| vulkan h264 | 1458 | 2821 |
| vulkan h265 | 1290 | 1458 |
| vulkan av1 | 1242 | 1229 |

### Encode, fps

Gold runs intra CQP qp 28 (AV1 IP g4); rv runs MFT/VkVideo/openh264 defaults on noise IDR. Settings differ: percents diagnose only.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 549 | 387 | -29.5% |
| d3d11 h265 | 456 | 694 | +52.2% |
| d3d11 av1 | 618 | 527 | -14.7% |
| d3d12 h264 | 549 | 104 | -81.1% |
| vulkan h264 | 549 | 502 | -8.6% |
| vulkan h265 | 456 | 231 | -49.3% |

Only d3d11 h265 beats gold, faster per frame at unknown quality. d3d12 h264 and vulkan h265 trail on documented floors: noise entropy-coding physics (10 ms) and the NVIDIA VkVideo submit floor (4.4 ms, ffmpeg-confirmed).

## One-sided reference, fps

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 669, h265 948, av1 737.
- Transcode via rv, no ffmpeg counterpart: d3d11 461, d3d12 377.
- h264 openh264 via rv, absent from the ffmpeg build: 92.0.
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.

## Comparability notes

- Content: sustained legs share gold bytes; shapes match only for vulkan surface (resident both sides) and the downloaded pairs. Surface and sync legs run legacy IDR vectors against gold testsrc IP: adjacent, not identical.
- Settings: encode legs do not match settings or bitrate and carry no quality claim.
- Gaps: AV1 all-intra NVENC refused; sw legs are ffmpeg one-sided (rv needs linkable libav*, absent here); d3d12 h265 encode declines (driver refuses the HEVC encode config on this box); d3d12 sustained has no vp9 leg (pending picture builder).
- Cross-OS: the Linux sheet differs in build, corpus, and statistic: same counting discipline only.
