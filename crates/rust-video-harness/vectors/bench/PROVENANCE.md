# Bench vectors

One all-intra 1080p frame per codec, generated 2026-11-14 with
`ffmpeg version 9.0-full_build-www.gyan.dev` from
`testsrc2=size=1920x1080:rate=30,noise=alls=8:allf=t` (one frame each).
Every file below decodes to null with exit 0.

| File | Command |
|---|---|
| `h265-1080p-nvenc.h265` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v hevc_nvenc -preset p1 -g 1 -f hevc` (RTX 5080, driver 32.0.16.2114). Main profile; carries reference sets. |
| `h265-1080p-x265.h265` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libx265 -preset ultrafast -x265-params keyint=1 -f hevc`. Rext-signaled here; d3d11 parses it, Main-only tiers decline. |
| `av1-1080p.obu` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libsvtav1 -g 1 -f obu` |
| `vp9-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx-vp9 -b:v 0 -crf 30 -g 1 -f ivf` |
| `vp8-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx -b:v 0 -crf 30 -g 1 -f ivf` |

`SRC` is the source above. H.264 needs no blob:
`H264Synth::annex_b_h264_packets_cropped_sized` builds 1080p
vectors in code. Benches resolve this directory through
`rust_video_harness::bench_dir`.
