# Bench vectors

One all-intra 1080p frame per codec, generated 2026-11-14
(h264 blob 2026-11-20, see row) with
`ffmpeg version 9.0-full_build-www.gyan.dev` from
`testsrc2=size=1920x1080:rate=30,noise=alls=8:allf=t` (one frame each).
Every file below decodes to null with exit 0.

| File | Command |
|---|---|
| `h264-1080p.h264` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libx264 -preset ultrafast -g 1 -f h264` (generated 2026-11-20; device tiers use the blob, PC bars use the synth blob below) |
| `h264-1080p-synth.h264` | `dump_vectors` (30 bytes, sha256 `623ffdac...0fdb1`; builder bytes copied here, pin test `sized_blob_matches_builder`) |
| `h264-1080p-sync.h264` | `ffmpeg -f lavfi -i testsrc=size=1920x1080:rate=30 -frames:v 1 -c:v libx264 -preset ultrafast -g 30 -pix_fmt yuv420p -f h264` (25346 bytes, sha256 `4F08E04C...460B`; Constrained Baseline, 1 I frame; recipe reproduces the bytes exactly; shared by the d11/d12/vk H.264 sync legs and the d3d11va downloaded gold) |
| `h265-1080p-nvenc.h265` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v hevc_nvenc -preset p1 -g 1 -f hevc` (RTX 5080, driver 32.0.16.2114). Main profile; carries reference sets. Shared by every HEVC bench. |
| `av1-1080p.obu` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libsvtav1 -g 1 -f obu` |
| `vp9-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx-vp9 -b:v 0 -crf 30 -g 1 -f ivf` |
| `vp8-1080p.ivf` | `ffmpeg -f lavfi -i SRC -frames:v 1 -pix_fmt yuv420p -c:v libvpx -b:v 0 -crf 30 -g 1 -f ivf` |

`SRC` is the source above. The synth blob holds one
`H264Synth::annex_b_h264_packets_cropped_sized(119, 67, 4, 40)`
packet (SPS + PPS + header-only IDR slice, no residual); every
PC-tier H.264 surface/transcode leg reads it. ffmpeg exits 0
on it with concealment warnings; GPUs decode one grey 1080p frame.
The sync legs read `h264-1080p-sync.h264` above instead (GPUs
cannot download the synth blob, so it never had a gold pair).
Benches resolve this directory through
`rust_video_harness::bench_dir`.

## Inter-picture 1080p streams

120 frames of `testsrc=size=1920x1080:rate=30:duration=4`,
generated 2026-11-18 with the same ffmpeg build for sustained
overlap benches (IDR-only content serializes the pumps; these
carry P-frames). The h264/h265 rows were replaced 2026-11-21
with B-carrying encodes (`-g 60 -bf 3`); every file below
decodes to null with exit 0.

| File | Command |
|---|---|
| `h264-1080p_ip.h264` | `ffmpeg -f lavfi -i TS -c:v libx264 -preset ultrafast -g 60 -bf 3 -pix_fmt yuv420p -f h264` (2 I + 30 P + 88 B, 234681 bytes, sha256 `838c85d5...9cd169`; replaces the 2026-11-18 `-g 30` encode, 4 I + 116 P, sha256 `ba8b833c...12ebf0`) |
| `h265-1080p_ip.h265` | `ffmpeg -f lavfi -i TS -c:v libx265 -preset ultrafast -g 60 -bf 3 -pix_fmt yuv420p -f hevc` (2 I + 29 P + 89 B, 55745 bytes, sha256 `06d7b82d...1561e2`; replaces the 2026-11-18 `-g 30` encode, 4 I + 29 P + 87 B, sha256 `6b00d2e0...675538`) |
| `av1-1080p_ip.ivf` | `ffmpeg -f lavfi -i TS -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p -f ivf` (unchanged: 4 I + 116 P; no B encodable, see below) |
| `vp9-1080p_ip.ivf` | `ffmpeg -f lavfi -i TS -c:v libvpx-vp9 -speed 8 -g 30 -pix_fmt yuv420p -f ivf` (unchanged: 4 I + 116 P; no B encodable, see below) |

`TS` is the testsrc above (4s, 120 frames). Throughput suites in
every tier resolve these through `bench_dir`; no tier-local
copies. AV1 keeps P-only bytes: libsvtav1 (preset 8 `-g 60`,
preset 6 `-g 60` with `hierarchical-levels=4`) and libaom-av1
(`-cpu-used 8 -g 60 -bf 3`) all emit 2 I + 118 P with zero
PTS/DTS inversions. VP9 keeps P-only bytes: the codec has no
B-frame syntax (libvpx ignores `-bf 3`; alt-ref only, no
reorder), the same reason no `vp8-1080p_ip.ivf` exists.

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

## Derived gold inputs (uncommitted, `.tmp/dump-vectors/`)

Gold runs these, never regenerated testsrc: same bytes the legs run.
Regenerate with `cargo run -p rust-video-harness --bin dump_vectors --
.tmp/dump-vectors` (byte-compares file against in-process build at
write time); `--check` re-verifies. Nothing here is committed (all
dumps exceed the 1MB cap except the synth blob, which lives above).

Encode dumps (per-tier timed frames; NV12 unless noted):

| File | Seeds | sha256 |
|---|---|---|
| `encode-d11-nv12-30f.yuv` | 4..34 | `f234c8b1...36e2837` |
| `encode-d12-nv12-30f.yuv` | 0..30 | `7c21a501...49e811d3` |
| `encode-vk-nv12-30f.yuv` | 0..4 cycled x30 | `9f8242ef...d02cf79` |
| `encode-mc-nv12-30f.yuv` | 1..=4 cycled x30 | `15f868ac...64ca384` |
| `encode-va-nv12-120f.yuv` | 1..=4 x30 | `41527e2c...e49b4c` |
| `encode-sw-yuv420p-30f.yuv` | 0..4 cycled x30, planar | `4cd405cd...afd5fec` |

Seed columns match each tier's `throughput.rs` timed encode frames
(d11 warmup 0..4, d12 warmup 30, vk/mc/sw warmup untimed over the same
seeds, va times 120). Full committed-file and encode-dump hashes in
`.tmp/vectors-inventory.md`; timing-cat and decode-concat full hashes
unrecorded in pins.

Timing cats (byte-repeat the dump N times; raw has no headers):

| File | Recipe | sha256 |
|---|---|---|
| `enc-d11-nv12-120f.yuv` | d11 x4 | `608a5759...a25f3` |
| `enc-d12-nv12-120f.yuv` | d12 x4 | `996a24a0...ef1d22` |
| `enc-vk-nv12-120f.yuv` | vk x4 | `a143a19b...3166451` |
| `enc-sw-yuv420p-300f.yuv` | sw x10 | `ca7fa70b...cfb94727` |

Decode concats (120x single-frame for surface/sync-shape gold, 600f
for software gold). Annex B, OBU, and raw cats are byte-repeats;
IVF goes through `ffmpeg -stream_loop (N-1) -i in.ivf -c copy`
(header-correct). Frame counts verified by `ffprobe -count_frames`.

| File | Recipe | Frames | sha256 |
|---|---|---|---|
| `h264-synth-120x.h264` | synth x120 | 120 | `d0aa1721...bd2afc8` |
| `h265-120x.h265` | nvenc blob x120 | 120 | `bbe541ef...e2ba6558` |
| `av1-120x.obu` | obu blob x120 | 120 | `15c24e8d...2e7ba3f` |
| `vp9-120x.ivf` | vp9 ivf x120 copy | 120 | `529fe913...fff0729` |
| `sw-h264-600f.h264` | h264 _ip x5 | 600 | `3462a611...8636c10` |
| `sw-h265-600f.h265` | h265 _ip x5 | 600 | `1d9d9a4b...77a808d` |
| `sw-av1-600f.ivf` | av1 _ip x5 copy | 600 | `dde34fdd...761f859` |
| `sw-vp9-600f.ivf` | vp9 _ip x5 copy | 600 | `a712bd2a...12fcc07` |
| `sw-vp8-600f.ivf` | vp8 ivf x600 copy | 600 | `333aba5b...7436e2025` |
