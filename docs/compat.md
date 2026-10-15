<!-- source: version-compatibility sweep, 2026-10-11; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Compatibility

One binary spans every driver, OS release, and device it can run on.
Build time varies what compiles; runtime varies what runs. Never both
for one axis. Tier tags: **[T1]** fetched primary, **[T2]** consistent
secondary, **[?]** unresolved, never filled. All fetches 2026-10-11;
ash, ndk-crate, libva, NDK, and CDD pins re-verified 2026-10-15.

## Pins

| Axis | Pin | Source |
|---|---|---|
| `ash` | `0.38.0+1.3.281` (headers 1.3.281) [T1] | crates.io ash API |
| `libloading` (via ash `loaded`) | `^0.8` [T1] | ash deps API |
| `wgpu` | not in tree; `30.0.1` applies only if a consumer lands (planned [?]) | crates.io wgpu API |
| `ndk` / `ndk-sys` | `0.9.0` / `0.6.0+11769913` [T1] | crates.io API, docs.rs ndk 0.9.0 |
| `windows` | not in tree; `0.62.2` named for the D3D11 tier (planned [?]) | crates.io API |
| NDK (CI) | r30 (2026-09-08) [T1] | android/ndk releases |
| `cargo-ndk` (CI) | unused; CI runs plain `cargo check --target` under `nttld/setup-ndk` [T1] | `.github/workflows/ci.yml` |
| `cargo-msrv` (CI) | unused; the `msrv` job runs `just msrv` on the pinned toolchain [T1] | `.github/workflows/ci.yml` |
| libva latest / floor | 2.24.1 / 2.14.0 [T1] | libva releases, Ubuntu packages |
| MSRV | 1.87 [T1] | workspace Cargo.toml |

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
for android-24; CDD 14 section 3.3.1 retains it) [T1]. Certified
handhelds from API 34 must decode H.264/H.265/VP8/VP9/AV1 and
encode H.264/VP8/AV1 (CDD 14 section 2.2.2) [T1]; below that,
attempt creation and handle absence.

Rules: `ndk 0.9.0` with `media` plus fixed `api-level-28`, NDK
r30, plain `cargo check --target` (no `cargo-ndk`). Every call above
21 sits behind an `ApiLevel` gate over
`ndk_sys::android_get_device_api_level`; missing API skips that op,
never crashes, never silently degrades. NDK has no codec-list API,
so codec choice over JNI `MediaCodecList` is planned [?]. Quirk
table `quirks.toml` keys `(soc, codec)` to entries and ships 4
(2 confirmed, 2 suspected), loader tested. Emulator CI over
negotiation and software paths at 21/24/28/33/34 is planned [?];
hardware decode needs a physical-device lab [?].

## VA-API, D3D11, wgpu

VA-API: dlopen `libva.so.2`, `vaInitialize` reports the runtime
version, caps come purely from profile/entrypoint queries [T1, va.h].
No libva floor in code. Floor-container CI against 2.14.0 (jammy),
2.20.0 (noble), and 2.24.1 is planned [?]. Headless-driver CI story
is [?].

D3D11: floor is Windows 8 desktop, the `ID3D11VideoDevice` minimum
[T1, MS Learn]. The tier is a stub: `rust-video-d3d11` holds no code
and no `windows` dependency (planned [?], pin 0.62.2 named above).
Decoder GUIDs will come from the windows-crate constants only
(H.264, HEVC, VP9, AV1, APV profiles all present [T1]); never
hand-copy hex. Runtime enumeration per adapter (hardware, WARP,
reference; profiles; formats) is planned [?].

wgpu is not a decode tier: its `Backends` list has no D3D11 and no
video API [T1, docs.rs]. Allowed roles are upload/render interop
behind a facade feature only, backend chosen at runtime. No consumer
exists, so the dependency stays out and nothing is pinned; the
30.0.1 pin above applies only if it lands.

## MSRV

Floor 1.87, declared as workspace `rust-version`. Every pin
satisfies it [T1]. CI enforces with the `msrv` job running the `just
msrv` recipe (clippy plus test) on the pinned 1.87.0 toolchain.
Any dependency whose `rust_version` exceeds the floor is rejected at
review. A bump is a minor-version decision with a changelog note,
never silent.

## Features vs runtime

| Axis | Mechanism | Why |
|---|---|---|
| OS tier inclusion | facade tier features; default enables `vulkan`, `vaapi`, `mediacodec`, each compiling on every target | a target builds only the tiers it enables |
| NDK API ladder | fixed `api-level-28` plus `ApiLevel` gates (21 floor, 28 async notify) | features expose symbols, gates pick paths |
| Vulkan ladder | runtime only | one binary spans 1.0 loaders to 1.4 drivers |
| VA-API caps | runtime only | driver mix unknown until init |
| D3D11 adapter/GUID/format | runtime only, planned [?] | per-adapter enumeration |
| wgpu backend | planned [?]; runtime `Backends` flags when it lands | one binary, many GPUs |
| software codecs | `rust-video-sw` features (`ffmpeg`, `gpl`, `h264-openh264`); facade codec features map to stub crates | compile cost and binary size |
| quirk table | runtime data file | per-device values, suspected until measured |

Anti-rule: no `cfg` on driver versions, no `cfg` on OS API levels,
no build script baking host GPU state into the binary. The corpus
script gates tests only, never behavior.

## CI matrix

`just ci` on Ubuntu plus Windows stays GPU-free. Around it: the
`msrv` job on pinned 1.87.0; one Android NDK check
(`rust-video-mediacodec` for `aarch64-linux-android` under NDK r30).
Planned [?]: emulator jobs at 21/24/28/33/34, Lavapipe Vulkan
ladder, VA-API floor containers, Windows no-GPU enumeration,
self-hosted GPU runners, an Android device lab for hardware e2e.

## Packaging

One dependency: the `rust-video` facade. Tiers arrive as facade
features: `av1`, `av2`, `h264`, `h265`, `vp9`, `d3d11`,
`mediacodec`, `vaapi`, and `vulkan`, each wired to its crate.
Default carries the three portable tiers (`vulkan`, `vaapi`,
`mediacodec`); every tier crate compiles on every target, and the
Linux-only modules inside the VA-API crate compile out where its
platform is absent. Codecs and `d3d11` stay opt-in. No `vp8`, no
`sw` wiring, no `wgpu-interop` (planned [?]).
Shared vocabulary (frames, packets, backend traits) lives in the
leaf `rust-video-caps`; tier crates build on it, never on the
facade outside dev-dependencies, and never on each other. The
facade resolves tiers over caller-supplied `Capabilities` so users
write one code path. Adding a tier is minor; removing is major.

## Open questions

1. ash ≥1.4.317 headers: watch releases; interim is hand-written modules.
2. `erupt` as alternate Vulkan binding, unexamined; probe if ash bites.
3. Lavapipe version and video-extension exposure: probe `vulkaninfo`.
4. VA-API headless CI story: probe DRM plus driver-name matrix.
5. Android hardware lab: owner infra decision.
6. JNI `MediaCodecList` levels: probe Android reference at implementation.
7. Suspected quirk-table values: per-device measurement.
8. Sub-Win8 floors: no requirement; Win8 stands.
