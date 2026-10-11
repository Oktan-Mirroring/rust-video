# VA-API decode tier

H.264 decode over DRM render nodes behind the facade `Decoder` shape:
`crates/rust-video-vaapi`. The frontend (Annex B parse, picture-order
counts, DPB, reference lists) reuses `cros-codecs` 0.0.6 with default
features; the libva binding is a minimal hand rebind; the two meet in a
portable parameter layer. The crate builds on every platform and decodes
on Linux where a render node offers H.264 VLD into NV12.

## Binding decision

Three candidates, measured on this box (AMD Granite Ridge + NVIDIA RTX
5080, libva 1.22.0, rustc 1.96.1, `-j4`):

| Candidate | Check time | Normal tree | Verdict |
|---|---|---|---|
| `cros-codecs` 0.0.6, default features | 2.35 s | 1 crate (`log`) | **taken**, H.264 frontend only |
| `cros-codecs` 0.0.6, `vaapi` feature | 7.4 s | 127 nodes (`drm`, `gbm`, `nix`, `zerocopy`, …) | rejected |
| `cros-libva` 0.0.12 standalone | 12.95 s | `bindgen` + `libclang` at build time | rejected |

The full `vaapi` feature was rejected on three independent grounds: its
transitive tree breaks the small-tree rule, its `drm`/`gbm`/`nix`
backends are Linux-only (the tier must still compile on Windows and
Android, declining at runtime), and its C2-wrapper API is `Rc`-based and
not shaped for the facade. `cros-libva` alone was rejected because its
`bindgen` build step needs `libclang` plus libva headers on every build
machine, while the tier touches about twenty entry points whose total
rebind is smaller than the build dependency. Both rejects were measured,
not assumed: the numbers above are `cargo check` wall time on a warm
registry.

What shipped instead: `cros-codecs` default features (pure-Rust parser
plus DPB, portable, one `log` dependency) for the H.264 frontend, and a
hand rebind of the used libva subset (`ffi.rs`, layouts pinned against
`/usr/include/va/va.h` from libva 1.22.0 with size tests). The audited
sources are exactly the dependency: `cros-codecs-0.0.6/src/codec/h264`
is byte-identical to the depth-1 clone of `chromeos/cros-codecs` at
`71387be` (`diff -rq` clean), so the audit below covers both.

Annex B assembly, picture-order counts (8.2.1), first-VCL detection
(7.4.1.2.4), list modification (8.2.4.3), and the marking sequence
(8.2.5.1) are implemented in this crate against the spec; the reused
pieces are the NAL/SPS/PPS/slice-header parser and the DPB primitives
(picture numbers, initial lists, MMCO 1–6, sliding window, store/bump).

## RPS audit

Audited against ITU-T H.264 (01/2012), Tier 1, fetched from the ITU for
this task. Cross-checks: FFmpeg `libavcodec` in-tree and the
backend-gated half of `cros-codecs` 0.0.6 (Tier 2, read-only reference).

Reused code, verified clause by clause: frame/field picture numbers
(8.2.4.1) match; P-frame (8.2.4.2.1) and B-frame (8.2.4.2.3) initial
lists match, including the B1 identity swap, the poc-type-0
non-existing exclusion, and POC — not FrameNumWrap — ordering for every
poc type (an early FrameNumWrap suspicion died on the spec text);
sliding window (8.2.5.3) and MMCO 1–6 (8.2.5.4) match, including the
MMCO 5 order-count reset arithmetic.

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

## Layout

`device.rs` opens the first H.264-capable render node (`VaApiDevice`,
facade `Device`). `frontend.rs` turns Annex B into ordered `Picture`s
over DPB slots. `params.rs` maps syntax onto VA-API buffers.
`backend.rs` owns the config, context, surface pool, submit, and NV12
readback with crop. `decoder.rs` (`H264Decoder`, facade `Decoder`)
drives ingest → begin → submit → commit → readback; `VaApiFrame`
carries NV12 bytes, identity, and a lease. `ffi.rs` is the rebind;
`error.rs` holds `VaApiError`/`SkipReason`. Off Linux the crate
compiles and every entry point declines with typed `Skipped`.

Budgets: output frames are readback copies, so they lease their byte
count against an unbounded readback budget that documents exactly that;
the fixed surface pool cannot overrun, and pool-sized accounting would
spuriously trip on streams longer than the pool.

## References

- ITU-T H.264 (01/2012), clauses 7.4.1.2.4, 8.2.1, 8.2.4, 8.2.5:
  `https://www.itu.int/rec/T-REC-H.264-201201-S`
- `cros-codecs` 0.0.6 (audited sources = dependency):
  `https://docs.rs/cros-codecs/0.0.6/`,
  `https://github.com/chromeos/cros-codecs`
- libva 1.22.0 `va.h` (FFI ground truth, profiles, H.264 layouts):
  `/usr/include/va/va.h`
- FFmpeg `h264_refs.c`, `h264_slice.c`, `vaapi_decode.c` (Tier-2
  cross-checks for list order, POC shape, Baseline→Constrained
  Baseline profile mapping)
