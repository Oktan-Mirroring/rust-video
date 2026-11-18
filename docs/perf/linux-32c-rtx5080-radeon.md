# linux 32c, RTX 5080 + Radeon: golden throughput, 2026-11-18

rv-sw decode beat ffmpeg on all 16 software decode legs, +4.3% to +81.3%. rv-vaapi took h264/h265/av1/vp9 surfaces (+2.8% to +90.0%), lost h264/h265 readback (-11.2%, -5.0%), and closed the encode gap to -4.5% h264 and +2.6% h265. Vulkan h264/h265 decode and both encodes have no ffmpeg number: TIMEOUT or nothing written, rv-only rows.

Golden throughput legs only: 600f decode streams, 300f encode streams. Run 2026-11-18 on a 32-core LMDE 7 box; rv tree 017ab7b plus uncommitted changes, release build.

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
- Median of 5 runs each side, no warmup either side; livepack is a single run, rv-only.
- FFmpeg VA-API pins renderD128; rv-vaapi opens the first openable render node in readdir order (the same node on this box, logged).
- rv-vulkan uses one negotiated device; ffmpeg vulkan legs run per device.
- These numbers do not compare to `just benches`: debug test profile, ITERS=30, different corpus.

## Software decode, fps

| Leg | FFmpeg | rv-sw | Delta |
|---|---|---|---|
| h264 IPB 1t | 596.2 | 630.8 | +5.8% |
| h264 IPB 4t | 1702.7 | 2039.2 | +19.8% |
| h264 IPB 8t | 2296.5 | 2823.3 | +22.9% |
| h264 IPB auto | 3242.8 | 4998.8 | +54.2% |
| h264 IP 4t | 1727.3 | 2109.9 | +22.2% |
| h264 IP auto | 3027.2 | 5486.8 | +81.3% |
| h265 IPB 4t | 1019.2 | 1067.5 | +4.7% |
| h265 IPB auto | 1567.0 | 1973.1 | +25.9% |
| av1 4t | 1340.4 | 1466.1 | +9.4% |
| av1 auto | 1610.8 | 1902.1 | +18.1% |
| vp9 1t | 671.4 | 700.5 | +4.3% |
| vp9 4t | 1379.2 | 1524.1 | +10.5% |
| vp9 8t | 1368.6 | 1668.2 | +21.9% |
| vp9 auto | 1511.2 | 2626.0 | +73.8% |
| vp8 4t | 1776.8 | 2011.8 | +13.2% |
| vp8 auto | 2670.9 | 4276.1 | +60.1% |

## Software encode, fps

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| x264 ultrafast zerolat 4t | 821.6 | 956.9 | +16.5% |
| x264 ultrafast zerolat auto | 982.7 | 1242.6 | +26.4% |
| x265 ultrafast zerolat 4t | 295.9 | 319.2 | +7.9% |
| x265 ultrafast zerolat auto | 297.0 | 315.6 | +6.3% |
| vpx-vp8 cpu8 4t | 156.5 | 160.4 | +2.5% |
| vpx-vp8 cpu8 8t | 168.3 | 170.3 | +1.2% |
| vpx-vp9 cpu8 4t | 386.6 | 482.3 | +24.8% |
| vpx-vp9 cpu8 8t | 456.3 | 542.5 | +18.9% |
| aom cpu8 8t | 174.8 | 171.4 | -1.9% |
| aom cpu10 8t | ERR, Numerical result out of range | no leg, crate pins cpu8 | uncomparable, ffmpeg ERR |
| svt p13 la0, FF auto / rv 4t | 500.3 | 589.8 | +17.9%, threads differ |
| svt p13 la0, FF auto / rv 8t | 500.3 | 882.3 | +76.4%, threads differ |
| openh264 4t / 8t | no leg, encoder absent from build | 934.3 / 932.6 | rv-only |

## VA-API decode, fps, Radeon

| Leg | FFmpeg | rv-vaapi | Delta |
|---|---|---|---|
| h264 surface | 791.9 | 872.0 | +10.1% |
| h264 readback | 745.6 | 661.8 | -11.2% |
| h265 surface | 573.7 | 589.5 | +2.8% |
| h265 readback | 548.4 | 521.1 | -5.0% |
| av1 surface | 575.9 | 1094.1 | +90.0% |
| av1 readback | 427.0 | 734.4 | +72.0% |
| vp9 surface | 536.2 | 567.7 | +5.9% |
| vp9 readback | 483.1 | 494.7 | +2.4% |

## VA-API encode, fps, 300f

| Leg | FFmpeg | rv-vaapi | Delta |
|---|---|---|---|
| h264 IP g60 | 316.3 | 302.1 | -4.5% |
| h264 all-intra | 320.0 | 306.0 | -4.4% |
| h265 IP g60 | 325.3 | 331.4 | +1.9% |
| h265 all-intra | 324.5 | 332.8 | +2.6% |
| h264 transcode, rv all-intra | 234.2 IP | 224.2 | -4.3%, GOP shape differs |
| h264 transcode, rv IP g60 | 234.2 IP | 225.1 | -3.9%, same GOP |
| h265 transcode, rv all-intra | 235.0 IP | 225.2 | -4.2%, GOP shape differs |
| h265 transcode, rv IP g60 | 235.0 IP | 226.9 | -3.4%, same GOP |

## Vulkan, fps

One negotiated device rv-side against per-device ffmpeg legs; h264/h265 decode and both encodes have no ffmpeg number. Rows stand as recorded, percents only where ffmpeg finished on RTX 5080.

| Leg | FFmpeg rtx5080 | FFmpeg radv | rv, one device |
|---|---|---|---|
| h264 decode surface | TIMEOUT | 583.1 | 2468.0 IPB |
| h264 decode readback | TIMEOUT | 522.0 | 1917.4 IPB, 2653.8 IP |
| h265 decode surface | TIMEOUT | TIMEOUT | 2098.0 |
| h265 decode readback | TIMEOUT | 433.5 | 1722.0 |
| av1 decode surface | 438.9 | 444.1 | 1692.4, +285.6% vs rtx5080 |
| av1 decode readback | 426.7 | 370.5 | 1634.5, +283.1% vs rtx5080 |
| h264 encode | ERR, nothing written | ERR, thread return code | 543.4, 77.6 Mbps actual vs 75 nominal |
| h265 encode | ERR, nothing written | ERR, thread return code | 235.9, 83.1 Mbps actual vs 75 nominal |

## One-sided reference

- NVENC via ffmpeg, no rv tier: h264 504.2, h265 549.7, av1 542.8.
- Livepack via rv, no ffmpeg counterpart: TsMux 1957486.7, TsDemux 1271411.1, FragMux 395920.9.
- radv transcode via ffmpeg, no rv leg: h264 225.5, h265 ERR.

## Comparability notes

- Counting: fps uses actual output frames both sides. No nominal-divisor legs remain.
- Warmup: none either side. Cold-start bias is symmetric; the median of 5 absorbs it.
- Statistic: median of 5 each side except livepack, single run rv-only.
- Threads: svt legs compare ffmpeg auto threads against rv 4t/8t; the percent carries that difference.
- Devices: rv-vaapi picks the first openable render node (same node here, logged); ffmpeg pins renderD128. rv-vulkan uses one negotiated device; ffmpeg covers both GPUs.
- Bitrate: vulkan encode nominal 75 Mbps; rv actual 77.6 h264 and 83.1 h265 on golden content. FFmpeg vulkan encode wrote nothing, so no encode comparison exists.
- Gaps: aom cpu10 ffmpeg ERR Numerical-out-of-range with no rv cpu10 path; vulkan h264/h265 decode ffmpeg TIMEOUT; openh264 encoder absent from the ffmpeg build.
- radv rows are FF-only, never percent: h264 surface 583.1 and readback 522.0, h265 readback 433.5 with surface TIMEOUT, av1 surface 444.1 and readback 370.5, both encodes ERR thread-abort. radv h264 surface and h265 transcode vary run to run with no ffmpeg-side code change.
