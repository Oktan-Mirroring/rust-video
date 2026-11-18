# Bench vectors

One all-intra 1080p frame per codec, generated 2026-11-14 with
`ffmpeg version 9.0-full_build-www.gyan.dev` from
`testsrc2=size=1920x1080:rate=30,noise=alls=8:allf=t` (one frame each).
Every file below decodes to null with exit 0.

| File | Command |
|---|---|
| `h265-1080p-nvenc.h265` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v hevc_nvenc -preset p1 -g 1 -f hevc` (RTX 5080, driver 32.0.16.2114). Main profile; carries reference sets. Shared by every HEVC bench. |
| `av1-1080p.obu` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libsvtav1 -g 1 -f obu` |
| `vp9-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx-vp9 -b:v 0 -crf 30 -g 1 -f ivf` |
| `vp8-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx -b:v 0 -crf 30 -g 1 -f ivf` |

`SRC` is the source above. H.264 needs no blob:
`H264Synth::annex_b_h264_packets_cropped_sized` builds 1080p
vectors in code. Benches resolve this directory through
`rust_video_harness::bench_dir`.

## Inter-picture 1080p streams

120 frames of `testsrc=size=1920x1080:rate=30:duration=4`,
generated 2026-11-18 with the same ffmpeg build for sustained
overlap benches (IDR-only content serializes the pumps; these
carry P-frames). Every file below decodes to null with exit 0.

| File | Command |
|---|---|
| `h264-1080p_ip.h264` | `ffmpeg -f lavfi -i TS -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -f h264` |
| `h265-1080p_ip.h265` | `ffmpeg -f lavfi -i TS -c:v libx265 -preset ultrafast -g 30 -pix_fmt yuv420p -f hevc` |
| `av1-1080p_ip.ivf` | `ffmpeg -f lavfi -i TS -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p -f ivf` |
| `vp9-1080p_ip.ivf` | `ffmpeg -f lavfi -i TS -c:v libvpx-vp9 -speed 8 -g 30 -pix_fmt yuv420p -f ivf` |

`TS` is the testsrc above (4s, 120 frames). Throughput suites in
every tier resolve these through `bench_dir`; no tier-local
copies.
