<!-- source: measured on this machine, 2026-10-11; tool: crates/rust-video-sw/examples/probe -->
<!-- date: 2026-10-11; author: sw-codecs, sw-consolidation -->
# Software codecs

Every figure below is Tier 1 (measured here, method recorded). Machine:
AMD Ryzen 9 9950X3D, 32 threads, 60 GB RAM, release profile, ffmpeg 7.1.5
(Debian 7.1.5-0+deb13u1). Rung 3 throughout: the real code on this machine.
System libs: dav1d 1.5.1, libvpx 1.15.0 (`libdav1d-dev`, `libvpx-dev`
installed for headers). Pinned versions live in the workspace root
`Cargo.lock`: openh264 0.9.8, openh264-sys2 0.9.8. Decode and VP9
encode reach the system libraries through one hand-written sys layer;
no crate in the tree binds dav1d or libvpx directly.

## Verdicts

One backend shape: every software decoder rides a single hand-written
libavcodec/libavutil sys layer (`ffmpeg_sys.rs`), with one thin decoder
type per codec over it. VP9 encode rides the same layer through ffmpeg's
`libvpx-vp9` wrapper, over the same system libvpx (`-encoders` shows no
native LGPL software encoder for the codec: only `libx264`/`libx265`
GPL, `libvpx*` external, and hardware wrappers).

| Candidate | Role | Verdict | Throughput, 1080p | Exactness |
|---|---|---|---|---|
| native ffmpeg H.264, hand sys | H.264 decode | **wins** (`H264Decoder`) | 588 fps, 4 threads | 60/60 `cmp`-clean vs ffmpeg |
| native ffmpeg HEVC, hand sys | H.265 decode | **wins** (`H265Decoder`) | 563 fps, 4 threads; 361 fps, 1 thread | 120/120 + 120/120 `cmp`-clean vs ffmpeg |
| native ffmpeg VP9, hand sys | VP9 decode | **wins** (`Vp9Decoder`) | 625 fps, 4 threads; 325 fps, 1 thread | 2000/2000 + 2000/2000 `cmp`-clean vs ffmpeg |
| native ffmpeg VP8, hand sys | VP8 decode | **wins** (`Vp8Decoder`) | 1336 fps, 4 threads; 444 fps, 1 thread | 1000/1000 + 1000/1000 `cmp`-clean vs ffmpeg |
| ffmpeg libdav1d wrapper, hand sys | AV1 decode | **wins** (`Av1Decoder`) | 604 fps, 4 threads; ~347 fps, 1 thread | 60/60 + 60/60 + 60/60 `cmp`-clean vs ffmpeg |
| openh264 0.9.8 | H.264 encode | **wins** (`H264Encoder`) | 272 fps, 4 threads | 120/120 frames ffmpeg-valid |
| libvpx 1.15.0 via ffmpeg `libvpx-vp9` wrapper, hand sys | VP9 encode | **wins** (`Vp9Encoder`) | 368 fps, 4 threads | 120/120 frames ffmpeg-valid |
| openh264 0.9.8 | H.264 decode | **loses** | 127 fps single-thread | 27/60 vs ffmpeg; B-frame trigger isolated |
| native `av1` decoder | AV1 decode | rejected, cited | n/a | hardware-only, fails without a HW pixel format |
| rusty_h264, rusty_h265 | H.264/H.265 | rejected, cited | n/a | single-threaded, far off realtime (`docs/matrix.md`) |
| vendored minimal ffmpeg | all decode | **loses** | 30 s clean `-j4` build, 310 TUs | redundant with the zero-cost system link, see below |
| ffmpeg-sys-next (crates.io) | all decode | **loses** | n/a | dependency gate condition 2 fails, see below |

Production scope is narrowed to what the backends emit: 8-bit 4:2:0
only, H.264 encode only. The facade also names planar 4:2:2 and 4:4:4
variants; decoders still emit 8-bit 4:2:0 and return `SwError` otherwise.

## How to reproduce

```bash
cd crates/rust-video-sw
CARGO_TARGET_DIR=~/Projects/sw-targ cargo build -j4 --release \
  --features ffmpeg,h264-openh264 --example probe
```

The probe decodes elementary streams (Annex B H.264/H.265, IVF for the
rest; extract once with `ffmpeg -c:v copy`) and dumps packed raw frames:

```bash
probe ffdec <h264|h265|vp8|vp9|av1> <in> [max] [threads] [dump.yuv]
probe vpxenc <in.yuv> <w> <h> <frames> <out.ivf>
probe h264dec <in.h264> [max] [dump.yuv]        # openh264 decode probe
probe h264enc <in.yuv> <w> <h> <frames> <out.h264>
```

Exactness is `cmp` of the dump against the ffmpeg oracle:

```bash
ffmpeg -v error -i <vector> -map 0:v:0 -vsync 0 -pix_fmt yuv420p -f rawvideo oracle.yuv
cmp prod.yuv oracle.yuv
```

`-vsync 0` is load-bearing for IVF inputs: without it the muxer pads
irregular timestamps up to CFR (`vp9-q00.ivf`, 2 packets, came out as
34 frames), which reads as a decode mismatch but is a muxer artifact.

Results: `vp9-bbb1080` 2000/2000 at 4 threads and 1 thread,
`tos_vp8` (1920x800) 1000/1000 at both counts, `vp9-q00` 2/2,
`vp80-00-comprehensive-001` 29/29, `h265-1080p` (x265-medium synth
with B-frames, below) 120/120 at both counts, `h265-main` 60/60,
`av1-1080p` 60/60, `av1-main` 60/60, `av1-grain` 60/60 (film grain
applied identically), `h264-high` 60/60. Encoder outputs check with
`ffprobe -count_frames`: 120/120 each at 1920x1080.

## Consolidation: ffmpeg-native VP9, HEVC, and AV1

Head-to-head, same session, same vectors, every path `cmp`-clean:

| Codec, vector | ffmpeg path | Throughput |
|---|---|---|
| VP9, `vp9-bbb1080` 2000f | native, hand sys | 625 fps 4t / 325 fps 1t |
| VP8, `tos_vp8` 1000f | native, hand sys | 1336 fps 4t / 444 fps 1t |
| HEVC, `h265-1080p` 120f | native, hand sys | 563 fps 4t / 361 fps 1t |
| AV1, `av1-1080p` 480f | `libdav1d` wrapper, hand sys | 604 fps 4t / ~347 fps 1t |

The ffmpeg paths gain 1.6-1.9x from 1 to 4 threads. The H.265 vector
is our own synth: 120 frames of 1080p I420 through `libx265 -preset
medium -x265-params bframes=3`, used as a test vector only (no GPL
code linked).

## AV1 rides the libdav1d wrapper

AV1 decode goes through the ffmpeg `libdav1d` wrapper over dav1d
1.5.1, reached by decoder name: 604 fps at 4 threads over 480 frames
of 1080p, ~347 fps at 1 thread, `cmp`-clean against the oracle
including film grain. The wrapper keeps dav1d's default frame
pipelining, which is where the 4-thread scaling comes from.

Two traps were found and are encoded in the code. First, the true
native `av1` decoder is hardware-only: opening it succeeds, then
every decode fails with "Your platform doesn't support hardware
accelerated AV1 decoding" / "Failed to get pixel format". There is
no native software AV1 decoder in ffmpeg 7.1. Second, id lookup is
order-dependent: `avcodec_find_decoder(AV_CODEC_ID_AV1)` returns
whichever AV1 decoder registered first (libdav1d on this build), so
`Av1Decoder` selects by name (`libdav1d`), deterministically, and
fails closed when the build lacks it.

## System vs vendored, relitigated across all codecs

The H.264 verdict picked the system link; the question was whether
five codecs change the answer. They don't. A full-set minimal build
from the 7.1.5 tarball:

```bash
./configure --disable-everything --disable-programs --disable-doc \
  --enable-decoder=h264,hevc,vp8,vp9,av1 \
  --enable-parser=h264,hevc,vp8,vp9,av1 \
  --enable-libdav1d --disable-autodetect --disable-debug
make -j4   # 30 s clean, 310 translation units
```

Out: 6.2 MB `libavcodec.a` + 1.4 MB `libavutil.a`, and configure's
own summary line reads "License: LGPL version 2.1 or later" with no
`--enable-gpl`. Honest cost, but it still buys nothing over the
system link on Unix: same sys layer either way, plus re-vendoring on
every security update and a `configure` step alien to the cargo
build. Rejected on maintenance, not on build minutes. The one place
vendoring stays live is Windows, where there is no system library;
see below.

## Windows story

Windows has no system libavcodec, so the crate selects per OS in
`build.rs`: Unix links the system libraries with no configuration,
and a Windows target reads `FFMPEG_LIB_DIR` for the directory
holding `avcodec.lib`/`avutil.lib`, panicking with the install
command when it is unset. Verified: targeting
`x86_64-pc-windows-msvc` without the variable fails with that
message, and with it set `cargo check` for the `ffmpeg` feature is
green (hand sys, no bindgen, nothing host-specific in the surface).

The concrete install is vcpkg, read from the port manifests
(2026-10-11): `ffmpeg` 9.0.2 builds LGPL by default (GPL and nonfree
are opt-in features), ships the native decoders with no
`--disable-decoder` in the default set, and carries a `dav1d`
feature for the AV1 wrapper; `libvpx` 1.16.0 is BSD-3-Clause AND
ISC. Recipe:

```powershell
vcpkg install ffmpeg[dav1d] libvpx --triplet x64-windows
$env:FFMPEG_LIB_DIR = "<vcpkg>\installed\x64-windows\lib"
```

libvpx in the install line feeds ffmpeg's own wrapper build; the crate
links avcodec/avutil only.

Two honest boundaries. First, vcpkg's ffmpeg 9.x is a newer ABI than
the bound major 61, so the major guard fails closed on it until the
sys layer is re-read against the newer headers (same ~23-function
surface, offsets re-probed); that is the guard working as designed,
not a hole. Second, only `cargo check` for the `ffmpeg` feature is
verified on this machine; a real Windows link is unverified here.
H.264 encode needs nothing: openh264 builds vendored from the crate.

## Version drift: one guard pattern per linked lib

Every `open` fails closed unless the linked library reports the
bound major; there is no silent fallback to a drifted ABI:

| Linked lib | Guard | Bound | Binds it protects |
|---|---|---|---|
| libavcodec | `avcodec_version() >> 16` | 61 | all five decoders, VP9 encode |
| libavutil | `avutil_version() >> 16` | 59 | frame layout, VP9 encode |
| libvpx | none direct; `libvpx-vp9` by-name lookup + avcodec guard | n/a | VP9 encode via wrapper |
| libdav1d | none direct; by-name lookup + avcodec guard | n/a | AV1 decode via wrapper |
| openh264 | none needed; vendored static, pinned in `Cargo.lock` | 0.9.8 | H.264 encode |

dav1d and libvpx are reached only through libavcodec, so a build
missing either wrapper fails its by-name lookup into `SwError::Init`,
and a drifted libavcodec fails the major guard first.

## License, per path

LGPL-only throughout, each link verified on disk: the invoked
decoder files in the 7.1.5 source all carry LGPL-2.1-or-later
headers (`libavcodec/h264dec.c`, `libavcodec/hevc/hevcdec.c`,
`libavcodec/vp9.c`, `libavcodec/vp8.c`, `libavcodec/libdav1d.c`),
the full-set configure above reports LGPL 2.1+, and the three
external libraries are BSD (`libdav1d7/copyright` BSD-2-Clause,
`libvpx9/copyright` BSD-3-Clause, openh264 Cisco BSD). The
[FFmpeg legal page](https://ffmpeg.org/legal.html) keeps the base
LGPL with GPL triggered only by optional parts, and every decoder
here is native (no `lib*` external; `ffmpeg -h decoder=X` shows
frame and slice threading) except AV1, which is the BSD dav1d.
Caveat, unchanged: the Debian system library itself is an
`--enable-gpl` build; this path invokes no GPL component, but a
distributable LGPL product links an LGPL-built libavcodec (vcpkg
default, or the minimal configure above), not Debian's.

## The sys layer

`FfmpegDecoder` (`crates/rust-video-sw/src/ffmpeg.rs`, feature
`ffmpeg`) drives libavcodec 61 over
`crates/rust-video-sw/src/ffmpeg_sys.rs`: 23 declared functions,
opaque context and parser, `repr(C)` packet and frame views carrying
only the fields the wrapper reads. Configuration passes through
`av_opt_set` (`threads`, `thread_type=frame+slice`), so no
`AVCodecContext` layout is encoded. `open` fails closed unless both
`avcodec_version()` and `avutil_version()` report their bound
majors, and every encoded offset carries a compile-time
`offset_of`/`size_of` assertion. Byte-stream codecs (H.264, H.265)
run through the libavcodec parser; framed codecs (VP8, VP9, AV1)
send one IVF frame per packet, exactly as ffmpeg's own demuxers do.

The layouts were read from the installed headers with a C probe, not
copied from memory: `AV_CODEC_ID_H264` 27, `AV_CODEC_ID_VP8` 139,
`AV_CODEC_ID_VP9` 167, `AV_CODEC_ID_HEVC` 173, `AV_CODEC_ID_AV1`
225, `AV_PIX_FMT_YUV420P` 0, packet `data`@24 `size`@32 size 104,
frame `data`@0 `linesize`@64 `width`@104 `height`@108 `format`@116
size 440, `EAGAIN` -11, `AVERROR_EOF` -541478725. A second C probe
pinned the parser contract the wrapper relies on: emitting a
delimited frame consumes zero new bytes, so zero-consumed output
re-feeds the same input instead of dropping it.

**Dependency gate.** No crate was added, so `Cargo.lock` is untouched.
The alternative was gated out: `ffmpeg-sys-next` fails condition 2
(the hand surface took under a day, ~200 lines for five codecs, with
no ongoing maintenance beyond the major-version guard) and drags a
build script plus pkg-config coupling. Default-no held.

**Scope.** 8-bit 4:2:0 only: `h264-high422` and `h264-high444`
decode inside libavcodec but the wrapper returns `SwError::BadFrame`.
The facade names planar 4:2:2/4:4:4; the `convert` gate admits
`PIX_FMT_YUV420P` only.

## Losers, with reasons

**openh264 decode: wrong pixels on B-frame content.** `h264-high.mp4`
(x264 High, `bframes=3`) decodes 27/60 frames `cmp`-clean; the other 33
differ in ~1% of bytes each (localized, not a reorder: set-unequal).
The stream is clean (`-err_detect` silent, ffmpeg concealment-off equals
default, ffmpeg 1-thread equals N-thread). Trigger split on fresh
synth streams: B-frames on diverges, B-frames off is exact, weighted
prediction is innocent either way. Single-threaded, and the crate's own
docs warn the thread knob "will probably segfault". It also cannot
decode the 4:2:2/4:4:4 goldens at all (I420 output only). Throughput
(127 fps) clears the bar; correctness fails it.

## Golden findings (harness-owned, not fixed here)

- `golden_gen.rs` hashes MJPEG bytes, not raw pixels: it passes `-f
  image2` without `-c:v rawvideo`, so image2's default mjpeg encoder
  transcodes every frame first. Frame `.bin` files are JPEGs
  (`Lavc61.19.101` comment).
- The MJPEG link is color-metadata sensitive: raw bytes `cmp`-clean
  against ffmpeg still hash differently after re-transcode because the
  range tag (`tv` vs `unknown`) changes encoder behavior/headers.
  Primary evidence above is therefore raw `cmp`, which is stronger.
- The `vp9/vp80-00-comprehensive-001.sha256` header names
  `corpus/vp9/...`, but the vector lives at `corpus/vp8/...`.
- The `goldens/` tree was being regenerated live during this evaluation
  (files appearing, disappearing, and moving between `vp8/` and `vp9/`
  mid-session); treat any single read as a snapshot.

## Follow-ups

1. Widen the ffmpeg decoders past 8-bit 4:2:0 (`h264-high422`,
   `h264-high444`, `av1-main10`, `av1-high444`, `av1-pro42212`); the
   wrapper already decodes them and the facade names planar 4:2:2/4:4:4,
   only the `convert` gate is missing.
2. Harness: hash rawvideo bytes in golden_gen (one `-c:v rawvideo`),
   fix the vp80 vector path in the golden header.
3. Re-read the sys layer against libavcodec 62/63 headers when the
   Windows vcpkg build lands (same surface, offsets re-probed,
   guard extended).
