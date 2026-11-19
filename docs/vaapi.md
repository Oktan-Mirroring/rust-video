# VA-API tier

H.264, H.265, VP9, and AV1 decode, H.264/H.265 encode, and GPU transcode over DRM render nodes behind the facade shapes. VP8 carries a staged frontend: parser sources on disk with typed decline until it is wired. The libva binding is a minimal hand rebind. The crate builds on every platform and runs on Linux where a render node offers the matching profile into NV12.

## Per-codec scope

| Codec | Wired | Typed decline |
|---|---|---|
| H.264 | Annex B parse, POC (8.2.1), first-VCL (7.4.1.2.4), list modification (8.2.4.3), marking (8.2.5.1), DPB, submit, NV12 readback | interlaced, FMO, separate planes, non-4:2:0, high bit depth |
| H.265 | Annex B parse, reference sets (8.3.2/8.3.4), DPB, submit, NV12 readback, e2e against ffmpeg oracle | multilayer, interlaced, high bit depth, range and screen-content extensions |
| VP9 | superframe/chunk parse, eight reference slots, show-existing frames, submit, NV12 readback, 600-frame e2e against ffmpeg oracle | profiles 1-3, high bit depth, non-4:2:0 |
| AV1 | OBU parse, reference slots, film-grain display slot, submit, NV12 readback | large-scale tiles, high bit depth, monochrome, non-4:2:0, scalable |

## Provenance

Syntax sources: `cros-codecs` 0.0.6, BSD-3-Clause, Copyright 2022-2024 The ChromiumOS Authors. License text kept at `crates/rust-video-vaapi/LICENSE.cros-codecs`; every staged file keeps its upstream copyright header. No `cros-codecs` dependency. The H.264 tree is restructured from upstream (grouped parameter structs, safe index math where the workspace denies `unsafe_code`); the staged VP8 tree stays byte-close to upstream pending its wiring.

Slice tail order follows ITU-T H.264 (01/2012) 7.3.3: list modification, weight table, marking, cabac init, qp delta. A restructure once read marking before the tail and shifted every later field on P/B slices; the call site in the slice-tail parser pins the order.

## Binding decision

Three candidates, measured on this box (AMD Granite Ridge + NVIDIA RTX 5080, libva 1.22.0, rustc 1.96.1, `-j4`):

| Candidate | Check time | Normal tree | Verdict |
|---|---|---|---|
| `cros-codecs` 0.0.6, default features | 2.35 s | 1 crate (`log`) | taken, then staged |
| `cros-codecs` 0.0.6, `vaapi` feature | 7.4 s | 127 nodes | rejected |
| `cros-libva` 0.0.12 standalone | 12.95 s | `bindgen` + `libclang` at build time | rejected |

The full `vaapi` feature was rejected on three independent grounds: its transitive tree breaks the small-tree rule, its `drm`/`gbm`/`nix` backends are Linux-only (the tier still compiles on Windows and Android, declining at runtime), and its C2-wrapper API is `Rc`-based and not shaped for the facade. `cros-libva` alone was rejected because its `bindgen` build step needs `libclang` plus libva headers on every build machine, while the tier touches about twenty entry points whose total rebind is smaller than the build dependency.

## RPS audit

Audited against ITU-T H.264 (01/2012), Tier 1, fetched from the ITU for this task. Cross-checks: FFmpeg `libavcodec` in-tree and the backend-gated half of `cros-codecs` 0.0.6 (Tier 2, read-only reference).

Verified clause by clause: frame/field picture numbers (8.2.4.1) match; P-frame (8.2.4.2.1) and B-frame (8.2.4.2.3) initial lists match, including the B1 identity swap, the poc-type-0 non-existing exclusion, and POC ordering for every poc type; sliding window (8.2.5.3) and MMCO 1-6 (8.2.5.4) match, including the MMCO 5 order-count reset arithmetic.

Holes found and how each ships:

1. `cros-codecs` type-1 POC sums the whole `offset_for_ref_frame` cycle instead of entries `0..=position` (spec 8-9). This crate's tracker follows 8-9 exactly, with a regression test that fails against the upstream shape.
2. `cros-codecs` type-0 POC mixes adjusted and unadjusted previous-lsb values across the two 8-3 comparisons in the MMCO 5 corner. Both comparisons here use the adjusted value.
3. `cros-codecs` gap handling skips previous-picture state updates through inserted non-existing frames, so a wrap spanning a gap mis-chains the next offset (8.2.1.2 NOTE). This crate records gap pictures.
4. `cros-codecs` field P-list long-term ordering sorts by `LongTermPicNum` where 8.2.4.2.2 says `LongTermFrameIdx`. Recorded for the interlaced pass: interlaced input declines with typed `Unsupported`, so the field path stays off.

## Proof

Decoded on this box over `/dev/dri/renderD128` (Mesa radeonsi, `VAProfileH264High` + VLD + NV12): `corpus/synth/h264/h264-high.mp4`, one facade packet per MP4 sample plus flush. All 60 output frames match an independent ffmpeg software decode of the same file byte for byte (YUV420P planar SHA-256 per frame, 60/60). The committed e2e test repeats this comparison against a live ffmpeg oracle whenever ffmpeg is present, and the test-support MP4 demux is itself proven NAL-for-NAL against `ffmpeg -c:v copy`.

The committed golden `goldens/h264/h264-high.sha256` does NOT match the current vector: the golden generator's own ffmpeg command reproduces this crate's hashes (60/60), not the golden's, in either planar or NV12 byte order. The corpus is untracked and was regenerated after the golden commit, so the golden names a different vector instance. High 4:2:2 and High 4:4:4 vectors decline with typed `Unsupported`: VA-API defines no H.264 VLD profile outside 8-bit 4:2:0.

Encode sessions open `EntrypointEncSlice` for H.264 High and H.265 Main with CQP rate control and sequence/picture/slice packed headers, all probed before the config. Pictures are all-intra IDRs or a low-delay P chain by GOP shape at CQP 26, with one picture in flight. The headers are hand-written and verified three ways: the H.264 writer round-trips through this crate's own parser, the H.265 writer pins its layout byte offsets, and both streams decode under ffmpeg.

## References

- ITU-T H.264 (01/2012), clauses 7.4.1.2.4, 8.2.1, 8.2.4, 8.2.5: `https://www.itu.int/rec/T-REC-H.264-201201-S`
- `cros-codecs` 0.0.6: `https://docs.rs/cros-codecs/0.0.6/`, `https://github.com/chromeos/cros-codecs`
- libva 1.22.0 `va.h`: `/usr/include/va/va.h`
- FFmpeg `h264_refs.c`, `h264_slice.c`, `vaapi_decode.c`
