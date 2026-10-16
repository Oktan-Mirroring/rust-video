# VA-API tier

H.264 decode, H.264/H.265 encode, and GPU transcode over DRM render
nodes behind the facade shapes: `crates/rust-video-vaapi`. H.265,
VP8, VP9, and AV1 decode carry staged frontends: parser sources on
disk with typed decline until each is wired. The libva binding is a
minimal hand rebind; syntax and parameters meet in a portable layer.
The crate builds on every platform and runs on Linux where a render
node offers the matching profile into NV12.

## Module map

| Path | Owns |
|---|---|
| `lib.rs` | crate root, public decoders, encoder, scaler, transcoder, errors |
| `device.rs` | `VaApiDevice`, first capable render node, entrypoint/config/export probes |
| `backend.rs` | config, context, surface pool, pipelined submit, NV12 readback with crop |
| `decoder.rs` | `H264Decoder`: ingest → begin → submit → commit → readback; `VaApiFrame` with lazy DMA-BUF export |
| `encoder.rs` | `VaApiEncoder`: all-intra CQP sessions, hand-written packed headers |
| `scale.rs` | `Scaler`: on-device VPP scale |
| `transcode.rs` | `Transcoder`: decode surface to encoder input, CPU split on probe miss |
| `frontend.rs` | Annex B into ordered `Picture`s over DPB slots, POC, reference lists |
| `params.rs` | syntax onto VA-API picture/slice buffers |
| `buffer.rs` | owned data buffers: create, map-rewrite, destroy on drop |
| `ffi.rs` | hand rebind of the used libva subset, layouts pinned against `/usr/include/va/va.h` (libva 1.22.0) with size tests |
| `error.rs` | `VaApiError`/`SkipReason` |
| `bitstream.rs` | `BitReader`: bits, Exp-Golomb, emulation prevention, `more_rbsp_data` |
| `h264.rs` + `h264/` | wired H.264 syntax: `nalu`, `parser`, `dpb`, `picture` |
| `h265.rs` + `h265/` | staged H.265 syntax: `parser`, `dpb`, `picture` (dormant) |
| `vp8.rs` + `vp8/` | staged VP8 syntax: `parser`, `bool_decoder`, `probs` (dormant) |
| `vp9.rs` + `vp9/` | staged VP9 syntax: `parser`, `lookups` (dormant) |
| `av1.rs` + `av1/` | staged AV1 syntax: `parser`, `reader`, `helpers` (dormant) |

Dormant means the subdirectory files are not declared with `mod` and
do not join the build; the sibling shim (`h265.rs`, `vp8.rs`,
`vp9.rs`, `av1.rs`) exports the decoder handle whose `decode`
declines every packet with typed `Unsupported`, covered by unit
tests. Off Linux the crate compiles empty: every module is
Linux-gated at the crate root.

## Per-codec now / later

| Codec | Now | Later |
|---|---|---|
| H.264 | fully wired: Annex B parse, POC (8.2.1), first-VCL (7.4.1.2.4), list modification (8.2.4.3), marking (8.2.5.1), DPB, VA-API submit, NV12 readback | interlaced, FMO, separate planes, non-4:2:0, high bit depth stay typed `Unsupported` (no H.264 VLD profile covers them) |
| H.265 | parser + DPB + picture sources staged with attribution; shell declines typed | wire parser `mod`, frontend, params, e2e against ffmpeg oracle |
| VP8 | parser + boolean decoder + probability tables staged with attribution; shell declines typed | restore `position` on `BitReader`, wire parser `mod`, frontend, params, e2e |
| VP9 | parser + lookup tables staged with attribution; shell declines typed | restore `position` and `read_bits_signed` on `BitReader`, wire parser `mod`, frontend, params, e2e |
| AV1 | OBU parser + reader + helpers staged with attribution; shell declines typed | restore `position` and `read_bits_aligned` on `BitReader`, wire reader/parser `mod`, frontend, params, e2e |

The missing `BitReader` surface (`position`, `read_bits_aligned`,
`read_bits_signed`, `read_le`) was cut with the old `vendor/`
enclave, which only H.264 callers needed. Each staged codec's later
column names exactly which pieces its parser calls, so the restore
happens once per wiring, not up front.

## Provenance

Syntax sources: `cros-codecs` 0.0.6, BSD-3-Clause, Copyright
2022–2024 The ChromiumOS Authors. License text kept at
`crates/rust-video-vaapi/LICENSE.cros-codecs`; every staged file
keeps its upstream copyright header. No `cros-codecs` dependency.
The H.264 tree is restructured from upstream (grouped parameter
structs, safe index math where the workspace denies `unsafe_code`);
the staged trees are byte-close to upstream pending their wiring.

Slice tail order follows ITU-T H.264 (01/2012) 7.3.3: list
modification, weight table, marking, cabac init, qp delta. A
restructure once read marking before the tail and shifted every
later field on P/B slices; the call site in `parse_slice_tail`
pins the order.

## Binding decision

Three candidates, measured on this box (AMD Granite Ridge + NVIDIA RTX
5080, libva 1.22.0, rustc 1.96.1, `-j4`):

| Candidate | Check time | Normal tree | Verdict |
|---|---|---|---|
| `cros-codecs` 0.0.6, default features | 2.35 s | 1 crate (`log`) | **taken, then staged**, H.264 wired |
| `cros-codecs` 0.0.6, `vaapi` feature | 7.4 s | 127 nodes (`drm`, `gbm`, `nix`, `zerocopy`, …) | rejected |
| `cros-libva` 0.0.12 standalone | 12.95 s | `bindgen` + `libclang` at build time | rejected |

The full `vaapi` feature was rejected on three independent grounds: its
transitive tree breaks the small-tree rule, its `drm`/`gbm`/`nix`
backends are Linux-only (the tier must still compile on Windows and
Android, declining at runtime), and its C2-wrapper API is `Rc`-based and
not shaped for the facade. `cros-libva` alone was rejected because its
`bindgen` build step needs `libclang` plus libva headers on every build
machine, while the tier touches about twenty entry points whose total
rebind is smaller than the build dependency.

## RPS audit

Audited against ITU-T H.264 (01/2012), Tier 1, fetched from the ITU for
this task. Cross-checks: FFmpeg `libavcodec` in-tree and the
backend-gated half of `cros-codecs` 0.0.6 (Tier 2, read-only reference).

Staged code, verified clause by clause: frame/field picture numbers
(8.2.4.1) match; P-frame (8.2.4.2.1) and B-frame (8.2.4.2.3) initial
lists match, including the B1 identity swap, the poc-type-0
non-existing exclusion, and POC — not FrameNumWrap — ordering for every
poc type; sliding window (8.2.5.3) and MMCO 1–6 (8.2.5.4) match,
including the MMCO 5 order-count reset arithmetic.

Holes found and how each ships:

1. `cros-codecs` type-1 POC sums the whole
   `offset_for_ref_frame` cycle instead of entries `0..=position`
   (spec 8-9). Its `frame_num_in_pic_order_cnt_cycle` is computed and
   never used. Fixed by implementation: this crate's `PocTracker`
   follows 8-9 exactly, with a regression test (`poc_type1_sums...`)
   that fails against the upstream shape.
2. `cros-codecs` type-0 POC mixes adjusted and unadjusted previous-lsb
   values across the two 8-3 comparisons in the MMCO 5 corner. Fixed
   the same way: both comparisons use the adjusted value.
3. `cros-codecs` gap handling never advances previous-picture state
   through inserted non-existing frames, so a wrap spanning a gap
   mis-chains the next offset (8.2.1.2 NOTE). This crate records gap
   pictures via `note_stored`.
4. `cros-codecs` field P-list long-term ordering sorts by
   `LongTermPicNum` where 8.2.4.2.2 says `LongTermFrameIdx`.
   Not fixed here: interlaced input is declined with typed
   `Unsupported`, so the field path never runs. Recorded so the next
   pass re-checks it instead of re-finding it.

New code in this crate follows the same clauses and carries unit tests
with hand-computed vectors: type-0/1/2 counts, an msb wrap, first-VCL
transitions, modification insert/dedup, sliding window, and bump order.

## Proof

Decoded on this box over `/dev/dri/renderD128` (Mesa radeonsi,
`VAProfileH264High` + VLD + NV12): `corpus/synth/h264/h264-high.mp4`,
instance `67ffe251…613050` (1,365,514 bytes, 2026-10-11), one facade
packet per MP4 sample plus flush. All 60 output frames match an
independent ffmpeg software decode of the same file byte for byte
(YUV420P planar SHA-256 per frame, 60/60). The committed e2e test
repeats this comparison against a live ffmpeg oracle whenever ffmpeg is
present, and the test-support MP4 demux is itself proven NAL-for-NAL
against `ffmpeg -c:v copy`.

The committed golden `goldens/h264/h264-high.sha256` does NOT match the
current vector: the golden generator's own ffmpeg command reproduces
this crate's hashes (60/60), not the golden's, in either planar or NV12
byte order. The corpus is untracked and was regenerated after the
golden commit, so the golden names a different vector instance. No test
here asserts against it until the harness regenerates coherent goldens;
that is harness-area work, not a tier gap. High 4:2:2 and High 4:4:4
vectors decline with typed `Unsupported`: VA-API defines no H.264 VLD
profile outside 8-bit 4:2:0, so there is nothing to fall back to.

Budgets: output frames are readback copies, so they lease their byte
count against an unbounded readback budget that documents exactly that;
the fixed surface pool cannot overrun, and pool-sized accounting would
spuriously trip on streams longer than the pool.

Decode submits without stalling: each picture renders on a rotating
buffer set (async depth four) and the sync moved to consumption
(readback, DMA-BUF export), so reorder depth overlaps submit with
the GPU. Parameter buffers rewrite in place across pictures; a
buffer recreates only when a payload overflows it, so steady-state
pictures never create or destroy buffers. Slice payloads sub-slice
the packet bytes without copying, and reference lists rebuild in
decoder scratch. `decode` copies pixels and releases
its pool slot at once; `decode_surface` hands out the surface with
the slot leased until the frame drops, which is what makes the lazy
export sound. `export()` syncs and calls `vaExportSurfaceHandle`
per call, two NV12 planes, opaque without a surface or when the
driver refuses.

Encode sessions open `EntrypointEncSlice` for H.264 High and H.265
Main with CQP rate control and sequence/picture/slice packed
headers, all probed before the config. Every picture is an IDR at
CQP 26: no reference management, no reorder delay, every packet a
keyframe. The picture header packs once per quantization and the
upload image is created once per session. The headers are
hand-written and verified three ways: the H.264 writer round-trips
through this crate's own parser, the H.265 writer pins its layout
byte offsets, and both streams decode under ffmpeg. Transcode keeps decode
surfaces on the device into the encoder, scales through a VPP
pipeline on miss, and splits through the CPU loudly (logged
`FallbackCause` plus `Plan::cpu`) only when the export or encode
probe fails.

## References

- ITU-T H.264 (01/2012), clauses 7.4.1.2.4, 8.2.1, 8.2.4, 8.2.5:
  `https://www.itu.int/rec/T-REC-H.264-201201-S`
- `cros-codecs` 0.0.6 (staged sources, see above):
  `https://docs.rs/cros-codecs/0.0.6/`,
  `https://github.com/chromeos/cros-codecs`
- libva 1.22.0 `va.h` (FFI ground truth, profiles, H.264 layouts):
  `/usr/include/va/va.h`
- FFmpeg `h264_refs.c`, `h264_slice.c`, `vaapi_decode.c` (Tier-2
  cross-checks for list order, POC shape, Baseline→Constrained
  Baseline profile mapping)
