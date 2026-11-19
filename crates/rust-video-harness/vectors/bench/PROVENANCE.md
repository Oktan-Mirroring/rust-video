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

## Shared 320x240 decode vectors

Hoisted 2026-11-19 from tier-local `tests/vectors/` dirs into
`vectors/decode/` (single copy, SHA256-verified byte-identical
at move). Every tier resolves these through `decode_dir`.
Rename map: d3d11 `valid_320x240.hevc` and d3d12
`inter_320x240.hevc` were byte-identical (B-reorder) and share
`inter_320x240.hevc`; d3d12 `valid_320x240.hevc` held distinct
all-intra bytes and is `intra_320x240.hevc`. All other names
unchanged. `SRC` below is
`testsrc2=size=320x240:rate=30:duration=0.5` (15 frames) on
`ffmpeg version 9.0-full_build-www.gyan.dev`; regen commands
reproduce codec, profile, dims, frame count, and pict-type
shape, not byte-identity.

| File | Contents | Command |
|---|---|---|
| `valid_320x240.h264` | Constrained Baseline, 2 I + 13 P | `ffmpeg -f lavfi -i SRC -c:v libx264 -pix_fmt yuv420p -profile:v baseline -g 8 -bf 0 -f h264` (regen-verified) |
| `bframes_320x240.h264` | High, 2 I + 9 B + 4 P | `ffmpeg -f lavfi -i SRC -c:v libx264 -pix_fmt yuv420p -profile:v high -g 8 -bf 3 -f h264` (new 2026-11-19; B-frames verified by ffprobe pict_type counts, null-decodes exit 0) |
| `inter_320x240.hevc` | Main, 2 I + 9 B + 4 P | `ffmpeg -f lavfi -i SRC -c:v libx265 -pix_fmt yuv420p -g 8 -x265-params repeat-headers=1 -f hevc` (original d3d12 command, regen-verified) |
| `intra_320x240.hevc` | Main, 15 I | `ffmpeg -f lavfi -i SRC -c:v libx265 -pix_fmt yuv420p -x265-params keyint=1:min-keyint=1:repeat-headers=1 -f hevc`, then relabel Rext to Main (see d3d12 e2e docs) |
| `valid_320x240.obu` | AV1 Main, 2 I + 13 P | `ffmpeg -f lavfi -i SRC -c:v libsvtav1 -pix_fmt yuv420p -g 8 -f obu` (regen-verified) |
| `tiled_320x240.obu` | AV1 Main tiled, 12 frames 2 I + 10 P | `ffmpeg -f lavfi -i TS4 -c:v libsvtav1 -pix_fmt yuv420p -g 8 -tiles 2x2 -f obu` with `TS4` at `duration=0.4` (regen-verified) |
| `grain_320x240.obu` | AV1 Main film grain, 2 I + 13 P | `ffmpeg -f lavfi -i SRC -c:v libsvtav1 -pix_fmt yuv420p -g 8 -svtav1-params film-grain=8 -f obu` (regen-verified; encoder log confirms grain synth) |
| `mslice_320x240.hevc` | Main multi-slice, 12 frames 2 I + 8 B + 2 P | `ffmpeg -f lavfi -i TS4 -c:v libx265 -pix_fmt yuv420p -g 8 -slices 4 -x265-params repeat-headers=1 -f hevc` (approximate: frame count matches, pict mix off by one B/P) |
| `sl_320x240.hevc` | Main scaling lists (`scaling_list_enabled_flag=1`), 2 I + 10 B + 3 P | Original command unrecorded; regen needs the x265 scaling-lists file |
| `valid_320x240.ivf` | VP9 Profile 0, 1 I + 14 P | `ffmpeg -f lavfi -i SRC -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 0 -crf 30 -f ivf` (regen-verified) |
