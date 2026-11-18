# windows 9950X3D, RTX 5080: throughput vs ffmpeg gold, 2026-11-18

d3d11 H.264 surface matches ffmpeg NVDEC frame for frame (1813 vs 1806 fps); vulkan sustained overlap beats it on identical bytes (2047 vs 1806). Eight sustained-overlap benches with bars from quiet medians are committed; this file is their gold comparison and the Windows counterpart to the Linux sheet. Numbers below are medians of 3, debug test profile, quiet box.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| Box | Ryzen 9 9950X3D 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 30-32C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, never selected; first-green-adapter wins NVIDIA) |

## Method

- Sync legs: `just benches` debug test profile, ITERS=30, committed vectors (noise-IDR/synth per codec). Sustained legs: one 120-frame IP pass through each tier's overlap pump, exact counts, zero drops.
- Gold decode: ffmpeg cuvid per codec on testsrc 1080p120 IP streams, gpu-resident (`-f null`) unless noted; one downloaded leg (d3d11va + hwdownload + rawvideo file).
- Gold encode: ffmpeg NVENC p1 from a raw YUV file (lavfi generation starves the encoder), intra CQP except AV1 IP g4 (all-intra refused).
- fps divides actual output frames by wall both sides; ffmpeg parses `speed=` against the 30fps input. Median of 3 each side, no warmup either side except a discarded first run per round.
- These numbers do not compare to the Linux sheet: debug vs release, ITERS=30/IP-120 vs 600f/300f golden corpus, different vectors. Same counting discipline, different loads.

## Decode surface vs gold, fps, gpu-resident both sides

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1806 | 1813 | +0.4% |
| d3d12 h264 surface | 1806 | 2112 | +16.9%, easier bytes |
| d3d11 hevc surface | 1623 | 1182 | -27.2% |
| d3d12 hevc surface | 1623 | 1130 | -30.4% |
| d3d11 av1 surface | 1671 | 926 | -44.6% |
| d3d12 av1 surface | 1671 | 943 | -43.6% |
| d3d11 vp9 surface | 1269 | 67 | -94.7%, MFT software vs NVDEC silicon |

Gold runs testsrc IP; rv runs flat synth IDR (h264) to 271 KB intra (av1). Shape matches, content adjacent; percents read with that gap.

## Decode sustained overlap, fps, 120-frame IP both sides

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1806 | 2047 | +13.4% |
| vulkan h265 sustained | 1623 | 1744 | +7.5% |
| vulkan av1 sustained | 1671 | 1502 | -10.1% |
| d3d11 h264 sustained | 1806 | 1086 | -39.9% |
| d3d11 hevc sustained | 1623 | 957 | -41.0% |
| d3d11 av1 sustained | 1671 | 1108 | -33.7% |
| d3d11 vp9 sustained | 1269 | 883 | -30.4% |
| d3d12 h264 sustained | 1806 | 677 | -62.5% |

All legs share gold's exact bytes (committed IP vectors). Gold excludes the CPU readback our legs include, so every percent below favors gold; d3d11 h264 also sits above downloaded gold (723, includes a 373 MB file write). Downloaded gold exists for h264 only; the readback path is codec-independent NV12, so one leg represents it.

## Decode sync, fps, rv legs with gold alongside

No percents: sync shapes never match a gold shape (CPU-in-hand IDR vs gpu-resident IP).

| Leg | FFmpeg resident | rv sync |
|---|---|---|
| d3d11 h264 / hevc / av1 / vp9 | 1806 / 1623 / 1671 / 1269 | 1452 / 845 / 673 / 220 |
| d3d12 h264 / hevc / av1 | 1806 / 1623 / 1671 | 833 / 631 / 555 |
| vulkan h264 / h265 / av1 | 1806 / 1623 / 1671 | 1200 / 760 / 788 |

## Encode, fps, quality unmatched

Gold is intra CQP (AV1 IP g4); rv runs MFT/VkVideo/openh264 defaults on noise IDR. Settings differ, so percents diagnose only.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 / hevc / av1 | 549 / 459 / 618 | 388 / 666 / 527 | -29.3% / +45.1% / -14.7% |
| d3d12 h264 | 549 | 98 | -82.1%, 8 ms noise-IDR NVENC physics |
| vulkan h264 / h265 | 549 / 459 | 488 / 116 | -11.1% / -74.7%, 4.3 ms driver floor |
| sw h264 openh264+nasm | no leg, encoder absent from build | 127 | rv-only |
| d3d11 vp9 software | no NVENC VP9 | 69 | rv-only |

d3d11 HEVC over gold means faster per frame at unknown quality, not a win claim. d3d12 H.264 and vulkan H.265 trail on documented floors (noise entropy-coding physics; NVIDIA VkVideo submit floor, ffmpeg-confirmed).

## One-sided reference

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 654, hevc 916, av1 707.
- d3d11 xcode via rv, no ffmpeg counterpart: 421 hops. d3d12 xcode: 366 hops.
- d3d12 HEVC encode declines on this adapter (no hw); vulkan surface legs decline on Windows (no exportable planes; Linux DMA-BUF owns them).

## Comparability notes

- Counting: fps uses actual output frames both sides. Sustained legs assert exact frame counts and zero drops.
- Warmup: none either side except a discarded first run per round. Median of 3 each side.
- Content: gold decode always testsrc IP; rv surface/sync legs run legacy IDR vectors, sustained legs run the committed IP vectors. Same-content percents exist only in the sustained vulkan rows and the downloaded h264 pair.
- Devices: first-green-adapter wins NVIDIA everywhere; ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Bitrate: encode legs do not match bitrate or QP; the table carries no quality claim.
- Gaps: per-codec downloaded gold (one h264 leg represents the codec-independent path); AV1 all-intra NVENC refused; SW decode legs unmeasured on Windows (ffmpeg-libavcodec linking unavailable, WinGet ships binaries only).
- Cross-OS: Linux sheet numbers do not compare (release, golden corpus, median of 5). Same counting discipline only.
