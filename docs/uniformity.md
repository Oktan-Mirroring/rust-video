# Uniformity

Every tier carries the same codec folders, the same six test files, and per-codec examples under one naming pattern. The grammar crates share one skeleton with one name per concept. `rust-video-core` holds the mechanics shared across backends. This file states the enforced shape and tables every deviation with its reason.

## Tier layout

The GPU tiers (`d3d11`, `d3d12`, `vulkan`, `vaapi`) group decode code into one folder per codec: `src/h264/`, `src/h265/`, `src/av1/`, `src/vp9/`. Each folder holds `mod.rs` plus the roles its code fills:

| Role | Contents |
|---|---|
| `decoder.rs` | tier decoder over the codec |
| `frontend.rs` | grammar ingest glue |
| `params.rs` | API payload builders |
| `dpb.rs` | reference bookkeeping |
| `picture.rs` | submit arguments |

Every role file carries code; folders hold only roles their code fills. `d3d11` and `d3d12` share one file list: identical module blocks plus the same four folders.

Two tiers are flat. `sw` decodes through one `FfmpegDecoder` parameterized by codec with thin delegation wrappers, and encodes through one file per backend under disjoint feature gates; a CPU backend has no device to probe. `mediacodec` runs one decoder parameterized by a mime string; splitting it per codec would clone one string into a folder.

## Grammar skeleton

`h264`, `h265`, `av1`, and `vp9` share these files for these concepts: `bits.rs` (`BitReader`), `error.rs` (error taxonomy), `framing.rs` (unit splitting), `picture.rs` (ingest into pictures), `tests.rs`. H.26x codecs add `slice.rs`, `sps.rs`, `pps.rs`, and the parameter files their syntax needs (`vps.rs`, `hrd.rs`, `scaling.rs`, `vui.rs`, `ptl.rs`, `rps.rs`). Frame codecs use `frame.rs` instead of `slice.rs`; AV1 carries its parameter set in `sequence.rs`. The crate roots re-export nine old names as plain `pub use` aliases, so existing imports resolve.

## Shared core

`rust-video-core` holds a construct when it is backend-agnostic and used by two or more tiers: `pump` (the `CopyPump` readback worker), `planes` (strided-plane validation and row copies), `codecs` (the shared `Codec` identity), `time` (saturating duration reads). Tiers re-export hoisted items at their old paths.

## Tests and examples

Every tier ships `e2e.rs`, `uniform.rs`, `surface_contract.rs`, `overlap.rs`, `async_delivery.rs`, and `throughput.rs`. Every tier ships `decode_{codec}_{tier}.rs` and `encode_{codec}_{tier}.rs` for each codec its API supports. Capability overviews (`decode_matrix_*`, `encode_matrix_*`), GPU transcode demos, and the `vkprobe` and `sw/probe` tools stay as tier-meaningful extras. Helpers shared across tiers live in `rust-video-harness`; helpers typed to one tier stay in that tier.

## Deviations

| Site | Shape | Reason |
|---|---|---|
| `d3d12/src/vp9/` | `mod.rs` + `decoder.rs` only | probe shell; the tier has no VP9 picture builder |
| `vulkan`, no `vp9/`/`vp8/` | folders absent | the tier has no VP9/VP8 decoders |
| `vulkan`, no `picture.rs` | role absent | the submit path is generic over the shared `Picture` |
| `vulkan/src/session/` | whole directory | multi-codec session scaffolding shared across codecs |
| `vaapi/src/vp9/lookups.rs` | extra role file | VP9 probability tables shared by `params.rs` |
| `vaapi/src/vp8.rs` | flat file | single-file staged decoder; a folder would wrap one file |
| `vaapi` role modules | `pub(crate)` | `h264` ingest types are reused across codecs and runtime files |
| `d3d11` role modules | private `mod` + re-exports | unifying the import style would churn every intra-codec import |
| `sw` encoders | one file per backend | disjoint backends under disjoint feature gates |
| `sw`, no `device.rs` | file absent | no hardware to open or probe |
| `mediacodec/src/format.rs`, `src/android.rs` | whole files | one `FormatSpec` struct and one role-generic session cover all codecs |
| grammar specs | NAL vs OBU, `Picture` vs `Frame`, `Sps` vs `SequenceHeader`, `slice.rs` vs `frame.rs` | vocabulary pinned by each codec family |
| grammar errors | trait style vs enum style | each family's taxonomy kept; kinds share names |
| `h265`, no `StrictGate` | gate absent | tier capability policy is tier API, not grammar |
| `H265Gate`, no `Default` | trait absent | tier `Gate` structs would break |
| alias attributes | plain `pub use` | deprecation attributes warn at tier use sites |
| overlap pumps | one per tier | public decode traits differ per tier |
| seams, frames, transcoders | per tier | backend-typed objects and session logic |
| tier error enums, `Profile` | per tier | variant sets and construction idioms differ |
| AV1 DXVA params | per DX tier | DirectX ABI structures, not backend-agnostic |
| H.26x params, DPBs, frontends | per tier | threaded through tier errors, pins, and picture types |
| `encode/` + `decode/` folders | absent | only seven files are one-sided in both DX crates, and nesting them splits helpers from the codec folders they serve |
| `d3d12/examples/common/hevc.rs` | keeps its name | pinned by tests through `#[path]`; tests are out of rename scope |
| missing encode/decode examples | absent per tier | each gap matches a missing API constructor |
