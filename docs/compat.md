<!-- source: version-compatibility sweep, 2026-10-11; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Compatibility

One binary spans every driver, OS release, and device it can run on.
Build time varies what compiles; runtime varies what runs. Never both
for one axis. Tier tags: **[T1]** fetched primary, **[T2]** consistent
secondary, **[?]** unresolved, never filled. All fetches 2026-10-11.

## Pins

| Axis | Pin | Source |
|---|---|---|
| `ash` | `0.38.0+1.3.281` (headers 1.3.281) [T1] | crates.io ash API |
| `libloading` (via ash `loaded`) | `^0.8` [T1] | ash deps API |
| `wgpu` | `30.0.1`, rust 1.87.0 [T1] | crates.io wgpu API |
| `ndk` / `ndk-sys` | `0.9.0` / `0.6.0+11769913` [T1] | crates.io API, docs.rs ndk 0.9.0 |
| `windows` | `0.62.2`, rust 1.82 [T1] | crates.io API |
| NDK (CI) | r30 (2026-09-08) [T1] | android/ndk releases |
| `cargo-ndk` (CI) | `4.1.2` [T1] | crates.io API |
| `cargo-msrv` (CI) | `0.19.3` [T1] | crates.io API |
| libva latest / floor | 2.24.1 / 2.14.0 [T1] | libva releases, Ubuntu packages |
| MSRV | 1.87, binding constraint wgpu 30.0.1 [T1] | workspace Cargo.toml |

## Vulkan

Headers 1.3.281 via ash 0.38, latest ash release [T1]. Device floor
for any video op is Vulkan 1.3: `video_queue` requires 1.3 in the
registry, and all video ops remain KHR extensions, never promoted
[T1, vk.xml]. Missing from ash: `encode_av1` (spec 1.3.302),
`decode_vp9` (spec 1.4.317), `maintenance2`, quantization-map and
intra-refresh extensions [T1, Khronos news].

Rules: dlopen the loader, never link it (`vulkan-1.dll` /
`libvulkan.so.1` per the loader interface doc) [T1]. Create the
instance at API 1.0 for maximum driver inclusion, then negotiate up
per device: instance version, device properties, extension strings,
then video capabilities and format properties per codec op [T1,
docs.vulkan.org]. Query before every use. Missing loader, pre-1.3
drivers, or absent video extensions mean graceful tier skip, never
degraded video: video cannot exist below 1.3, so fallback is
tier-off. Hand-write the two missing ash extension modules against
registry vk.xml inside `rust-video-vulkan`, each behind its
extension-string check; no ash fork, no git pin. Lavapipe CI
coverage is [?].

## Android

minSdk 21. NDK ladder from `NdkMediaCodec.h` [T1]: 21 sync
create/configure/buffers/queue; 26 input surfaces and parameters;
28 async callbacks; 31 low-latency and HDR keys; 33 frame-rendered
callbacks; nothing above 33 exists, so ndk 0.9.0 covers the whole
surface [T1]. Vulkan tier needs API 24+ (CDD 7.0 lists `libvulkan.so`
for android-24) [T1]. Certified handhelds from API 34 must decode
H.264/H.265/VP8/VP9/AV1 and encode H.264/VP8/AV1 (CDD 14) [T1];
below that, attempt creation and handle absence.

Rules: `ndk 0.9.0` with `media` plus the `api-level-NN` chain, NDK
r30, `cargo-ndk`. Every call above 21 sits behind an `SDK_INT` gate
read once at tier init (exact accessor [?], probe docs.rs at
implementation); missing API skips that op, never crashes, never
silently degrades. NDK has no codec-list API, so codec choice and
quirks go over JNI `MediaCodecList` [T2, levels to verify]. Quirk
table `quirks.toml` keys `(manufacturer, soc, mime)` to flags; all
values are Tier 3 until measured, so it ships empty-but-functional
with the loader tested. Emulator CI covers negotiation and software
paths at 21/24/28/33/34; hardware decode needs a physical-device lab
[?].

## VA-API, D3D11, wgpu

VA-API: dlopen `libva.so.2`, `vaInitialize` reports the runtime
version, caps come purely from profile/entrypoint queries [T1, va.h].
No libva floor in code. CI builds against 2.14.0 (jammy), 2.20.0
(noble), and 2.24.1 [T1]. Headless-driver CI story is [?].

D3D11: floor is Windows 8 desktop, the `ID3D11VideoDevice` minimum
[T1, MS Learn]. `windows 0.62.2` with the D3D11 features [T1].
Decoder GUIDs come from the windows-crate constants only (H.264,
HEVC, VP9, AV1, APV profiles all present [T1]); never hand-copy hex,
which closes the hw-tiers GUID gap. Runtime enumerates device
(hardware, WARP, reference), profiles, and formats per adapter; the
basic display adapter reports no decoders and the tier skips [T2].

wgpu 30.0.1 is not a decode tier: its `Backends` list has no D3D11
and no video API [T1, docs.rs]. Allowed roles are upload/render
interop behind a facade feature only, backend chosen at runtime. If
no consumer exists, the dependency stays out. Its rust_version
1.87.0 exactly equals the MSRV floor, so it stays exactly pinned.

## MSRV

Floor 1.87, declared as workspace `rust-version`. Every pin
satisfies it [T1]. CI enforces with `cargo-msrv verify` plus a `just
msrv` recipe running check and test on the pinned 1.87.0 toolchain.
Any dependency whose `rust_version` exceeds the floor is rejected at
review. A bump is a minor-version decision with a changelog note,
never silent.

## Features vs runtime

| Axis | Mechanism | Why |
|---|---|---|
| OS tier inclusion | facade `tier-*` features, `cfg(target_os)` deps | a target cannot carry another OS's tier |
| NDK API ladder | `api-level-NN` features plus `SDK_INT` gates | features expose symbols, gates pick paths |
| Vulkan ladder | runtime only | one binary spans 1.0 loaders to 1.4 drivers |
| VA-API caps | runtime only | driver mix unknown until init |
| D3D11 adapter/GUID/format | runtime only | per-adapter enumeration |
| wgpu backend | runtime `Backends` flags | one binary, many GPUs |
| software codecs | facade `codec-*` features | compile cost and binary size |
| quirk table | runtime data file | per-device Tier 3 values |

Anti-rule: no `cfg` on driver versions, no `cfg` on OS API levels,
no build script baking host GPU state into the binary. The corpus
script gates tests only, never behavior.

## CI matrix

`just ci` on Ubuntu plus Windows stays GPU-free. Around it: MSRV job
on pinned 1.87.0; Android NDK builds for all targets at minSdk 21
with every `api-level` feature; emulator jobs at 21/24/28/33/34 for
negotiation and software paths; Lavapipe Vulkan ladder [?]; VA-API
floor containers; Windows no-GPU enumeration expecting skip-or-list;
self-hosted GPU runners plus an Android device lab for hardware
e2e [?].

## Packaging

One dependency: the `rust-video` facade. Tiers arrive as facade
features (`tier-vulkan`, `tier-vaapi`, `tier-d3d11`,
`tier-mediacodec`, `codec-*`, `wgpu-interop`), each an optional
dependency on its tier crate; defaults are all tiers compilable on
the building target, individually disableable. Tier crates never
depend on each other. The facade re-exports runtime capability
discovery so users write one code path. Adding a tier is minor;
removing is major.

## Open questions

1. ash ≥1.4.317 headers: watch releases; interim is hand-written modules.
2. `erupt` as alternate Vulkan binding, unexamined; probe if ash bites.
3. Lavapipe version and video-extension exposure: probe `vulkaninfo`.
4. VA-API headless CI story: probe DRM plus driver-name matrix.
5. Android hardware lab: owner infra decision.
6. `ndk::api_level()` accessor shape: probe docs.rs at implementation.
7. JNI `MediaCodecList` levels: probe Android reference at implementation.
8. All quirk-table values: per-device measurement.
9. Sub-Win8 floors: no requirement; Win8 stands.
