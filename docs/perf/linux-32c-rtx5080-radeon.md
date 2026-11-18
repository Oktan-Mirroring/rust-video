# linux 32c, RTX 5080 + Radeon: golden throughput, 2026-11-18

rv-sw decode beat ffmpeg on all 16 software decode legs, +4.9% to +80.8%. rv-vaapi took h264/h265/av1/vp9 surfaces (+3.5% to +88.3%) and h264 readback (+2.9%, gap closed); h265 readback still trails (-5.2%). Encode held at -3.6% h264 and +3.7% h265. Vulkan h264/h265 decode and both encodes have no ffmpeg number: TIMEOUT or nothing written, rv-only rows.

Golden throughput legs only: 600f decode streams, 300f encode streams. Run 2026-11-18 on a 32-core LMDE 7 box, release build.

## Environment

| Item | Value |
|---|---|
| OS | LMDE 7, debian trixie base |
| Box | 32-core |
| FFmpeg | 7.1.5-0+deb13u1, gcc 14, gpl; +libx264 +libx265 +libvpx +libaom +libsvtav1 +libdav1d +nvenc +vaapi +vulkan; no libopenh264 |
| rustc | 1.96.1 (31fca3adb 2026-06-26) |
| NVIDIA GPU | GeForce RTX 5080, driver 615.71.10 |
| AMD GPU | Radeon RAPHAEL_MENDOCINO, Mesa radeonsi 26.1.6, DRM 3.61 |
| VA-API | libva 1.22.0 / 2.22.0 on renderD128 |
| Vulkan | GPU0 NVIDIA RTX 5080, GPU1 AMD Radeon (RADV), GPU2 llvmpipe |

## Method

- Decode legs run 600f actual; encode legs run 300f yuv420p at 5M nominal, fastest sane settings both sides.
- fps divides actual output frames by wall both sides: ffmpeg parses `-progress frame=` per run, rv counts decoded frames over fed frames.
- Median of 5 runs each side, no warmup either side, livepack included.
- FFmpeg VA-API pins renderD128; rv-vaapi opens the first openable render node in readdir order (the same node on this box, logged).
- rv-vulkan uses one negotiated device; ffmpeg vulkan legs run per device.
- These numbers do not compare to `just benches`: debug test profile, ITERS=30, different corpus.

## Software decode, fps

| Leg | FFmpeg | rv-sw | Delta |
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

## Software encode, fps

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
| aom cpu10 8t | ERR, Numerical result out of range | no leg, crate pins cpu8 | uncomparable, ffmpeg ERR |
| svt p13 la0 lp4, matched | 424.8 | 572.1 | +34.7% |
| svt p13 la0 lp6 vs rv 8t, matched | 484.0 | 873.2 | +80.4% |
| openh264 4t / 8t | no leg, encoder absent from build | 857.6 / 846.7 | rv-only |

SVT legs match thread settings both sides: rv threads map to lp only (4t is lp4, 8t clamps to lp6), ffmpeg `-threads` is inert for libsvtav1, so the ffmpeg legs use the same lp values. Superseded v3 rows compared ffmpeg auto 500.3 against rv 4t 589.8 (+17.9%) and rv 8t 882.3 (+76.4%) with threads differing.

## VA-API decode, fps, Radeon

| Leg | FFmpeg | rv-vaapi | Delta |
|---|---|---|---|
| h264 surface | 791.1 | 871.7 | +10.2% |
| h264 readback | 729.7 | 750.5 | +2.9% |
| h265 surface | 573.9 | 590.8 | +2.9% |
| h265 readback | 541.4 | 513.3 | -5.2% |
| av1 surface | 575.9 | 1086.3 | +88.6% |
| av1 readback | 423.8 | 673.7 | +59.0% |
| vp9 surface | 538.0 | 567.3 | +5.4% |
| vp9 readback | 464.3 | 543.8 | +17.1% |

Readback path: split-phase worker, DEFAULT_DEPTH 4. Same-content before/after: h264 +16.1%, vp9 +12.8%, h265 -0.9%, av1 -5.4% across 4 interleaved runs. Surface, encode, and transcode legs hold within 0.5%.

## VA-API encode, fps, 300f

| Leg | FFmpeg | rv-vaapi | Delta |
|---|---|---|---|
| h264 IP g60 | 316.1 | 301.2 | -4.7% |
| h264 all-intra | 319.7 | 306.0 | -4.3% |
| h265 IP g60 | 325.4 | 331.7 | +1.9% |
| h265 all-intra | 326.2 | 332.8 | +2.0% |
| h264 transcode, rv all-intra | 234.5 IP | 221.5 | -5.5%, GOP shape differs |
| h264 transcode, rv IP g60 | 234.5 IP | 220.7 | -5.9%, same GOP |
| h265 transcode, rv all-intra | 235.0 IP | 224.7 | -4.4%, GOP shape differs |
| h265 transcode, rv IP g60 | 235.0 IP | 225.6 | -4.0%, same GOP |

## Vulkan, fps

One negotiated device rv-side against per-device ffmpeg legs; h264/h265 decode and both encodes have no ffmpeg number. Rows stand as recorded, percents only where ffmpeg finished on RTX 5080.

| Leg | FFmpeg rtx5080 | FFmpeg radv | rv, one device |
|---|---|---|---|
| h264 decode surface | TIMEOUT | 579.2 | 2481.9 IPB |
| h264 decode readback | TIMEOUT | 524.6 | 1939.4 IPB, 2640.3 IP |
| h265 decode surface | TIMEOUT | TIMEOUT | 2108.3 |
| h265 decode readback | TIMEOUT | 432.5 | 1713.2 |
| av1 decode surface | 446.3 | 443.0 | 1708.3, +282.8% vs rtx5080 |
| av1 decode readback | 433.6 | 373.4 | 1638.1, +277.8% vs rtx5080 |
| h264 encode | ERR, nothing written | ERR | 543.9, 77.6 Mbps actual vs 75 nominal |
| h265 encode | ERR, nothing written | ERR | 236.8, 83.1 Mbps actual vs 75 nominal |

## One-sided reference

- NVENC via ffmpeg, no rv tier: h264 499.2, h265 557.3, av1 511.5.
- Livepack via rv, no ffmpeg counterpart, median of 5: TsMux 2039204.4, TsDemux 1552162.4, FragMux 428935.8.
- radv transcode via ffmpeg, no rv leg: h264 226.6, h265 ERR.

## Comparability notes

- Counting: fps uses actual output frames both sides. No nominal-divisor legs remain.
- Warmup: none either side. Cold-start bias is symmetric; the median of 5 absorbs it.
- Statistic: median of 5 each side including livepack.
- Devices: rv-vaapi picks the first openable render node (same node here, logged); ffmpeg pins renderD128. rv-vulkan uses one negotiated device; ffmpeg covers both GPUs.
- Bitrate: vulkan encode nominal 75 Mbps; rv actual 77.6 h264 and 83.1 h265 on golden content. FFmpeg vulkan encode wrote nothing, so no encode comparison exists.
- Gaps: aom cpu10 ffmpeg ERR Numerical-out-of-range with no rv cpu10 path; vulkan h264/h265 decode ffmpeg TIMEOUT; openh264 encoder absent from the ffmpeg build.
- radv rows are FF-only, never percent: h264 surface 579.2 and readback 524.6, h265 readback 432.5 with surface TIMEOUT, av1 surface 443.0 and readback 373.4, both encodes ERR. radv h264 surface and h265 transcode vary run to run with no ffmpeg-side code change.
