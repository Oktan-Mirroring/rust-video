# Windows throughput vs FFmpeg

d3d11 h264 surface matches ffmpeg NVDEC frame for frame (1813 vs 1806 fps); vulkan sustained overlap beats it on identical bytes (2951 vs 1806). Sustained legs share gold bytes; sync and surface legs run legacy vectors against gold testsrc. Encode settings differ both sides: encode percents diagnose only.

Debug test profile, ITERS=30 sync legs, 120-frame IP sustained legs, median of 3, quiet box, run 2026-11-18.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| CPU | Ryzen 9 9950X3D, 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 30-32C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, never selected) |

## Method

- Sync legs run `just benches` (debug test profile, ITERS=30, committed noise-IDR/synth vectors). Sustained legs run one 120-frame IP pass per tier overlap pump, exact counts, zero drops.
- Gold decode: ffmpeg cuvid per codec on testsrc 1080p120 IP streams, gpu-resident (`-f null`) unless noted; one downloaded leg (d3d11va + hwdownload + rawvideo file).
- Gold encode: ffmpeg NVENC p1 from a raw YUV file (lavfi generation starves the encoder), intra CQP except AV1 IP g4 (all-intra refused).
- fps divides actual output frames by wall both sides; ffmpeg parses `speed=` against the 30fps input. Median of 3 each side, no warmup either side except a discarded first run per round.
- Devices: d3d11/d3d12 open the default hardware adapter (RTX 5080 here); rv-vulkan takes the first enumerated adapter passing the op (NVIDIA here); ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Compare against the Linux sheet on counting discipline only: build, corpus, and vectors differ.

## Software

### Decode, fps

Unmeasured: the ffmpeg build ships binaries only, with no linkable libav*.

### Encode, fps

No two-sided legs. OpenH264 runs rv-only; see One-sided reference.

## Hardware

### Decode, fps

Surface, gpu-resident both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1806 | 1813 | +0.4% |
| d3d12 h264 surface | 1806 | 2112 | +16.9% |
| d3d11 h265 surface | 1623 | 1182 | -27.2% |
| d3d12 h265 surface | 1623 | 1130 | -30.4% |
| d3d11 av1 surface | 1671 | 926 | -44.6% |
| d3d12 av1 surface | 1671 | 943 | -43.6% |
| d3d11 vp9 surface, MFT software | 1269 | 67 | -94.7% |

d3d12 h264 runs easier bytes (flat synth IDR vs gold testsrc IP). d3d11 vp9 is MFT software against NVDEC silicon.

Sustained overlap, 120-frame IP both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1806 | 2951 | +63.4% |
| vulkan h265 sustained | 1623 | 1744 | +7.5% |
| vulkan av1 sustained | 1671 | 1502 | -10.1% |
| d3d11 h264 sustained | 1806 | 1086 | -39.9% |
| d3d11 h265 sustained | 1623 | 957 | -41.0% |
| d3d11 av1 sustained | 1671 | 1108 | -33.7% |
| d3d11 vp9 sustained | 1269 | 883 | -30.4% |
| d3d12 h264 sustained | 1806 | 1029 | -43.0% |

Every leg shares gold's exact bytes; gold stays resident while rv legs hand CPU frames back: each percent favors gold. The downloaded h264 pair is the only shape-matched d3d leg: gold 723 with a 373 MB file write, rv 1086 with CPU frames in hand.

Sync, rv legs with gold alongside (shapes never match, no percents):

| Leg | FFmpeg resident | rv sync |
|---|---|---|
| d3d11 h264 | 1806 | 1452 |
| d3d11 h265 | 1623 | 845 |
| d3d11 av1 | 1671 | 673 |
| d3d11 vp9 | 1269 | 220 |
| d3d12 h264 | 1806 | 833 |
| d3d12 h265 | 1623 | 631 |
| d3d12 av1 | 1671 | 555 |
| vulkan h264 | 1806 | 1200 |
| vulkan h265 | 1623 | 760 |
| vulkan av1 | 1671 | 788 |

### Encode, fps

Gold runs intra CQP (AV1 IP g4); rv runs MFT/VkVideo/openh264 defaults on noise IDR. Settings differ: percents diagnose only.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 549 | 388 | -29.3% |
| d3d11 h265 | 459 | 666 | +45.1% |
| d3d11 av1 | 618 | 527 | -14.7% |
| d3d12 h264 | 549 | 98 | -82.1% |
| vulkan h264 | 549 | 488 | -11.1% |
| vulkan h265 | 459 | 116 | -74.7% |

d3d11 h265 over gold is faster per frame at unknown quality. d3d12 h264 and vulkan h265 trail on documented floors: noise entropy-coding physics (8 ms) and the NVIDIA VkVideo submit floor (4.3 ms, ffmpeg-confirmed).

## One-sided reference, fps

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 654, h265 916, av1 707.
- Transcode via rv, no ffmpeg counterpart: d3d11 421, d3d12 366.
- h264 openh264 via rv, absent from the ffmpeg build: 127.
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.

## Comparability notes

- Content: sustained legs share gold bytes; shapes match only for vulkan (resident both sides) and the downloaded h264 pair. Surface and sync legs run legacy IDR vectors against gold testsrc IP: adjacent, not identical.
- Settings: encode legs do not match settings or bitrate and carry no quality claim.
- Gaps: downloaded gold exists for h264 only (codec-independent NV12 path, one leg); AV1 all-intra NVENC refused; sw decode unmeasured (binaries-only ffmpeg); d3d12 h265 encode declines (driver refuses the HEVC encode config on this box); vulkan surface legs decline on Windows (no exportable planes).
- Cross-OS: the Linux sheet differs in build, corpus, and statistic: same counting discipline only.
