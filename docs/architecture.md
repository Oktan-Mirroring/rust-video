# Architecture

One facade over software and platform hardware codecs. Tiers
execute, codec models describe, caps names the shared
vocabulary, and the caller decides every route.

## Facade

`crates/rust-video` selects and names; it runs no codec.
`TierPreference::resolve` picks one tier for a codec from
caller-supplied `Capabilities` on a `Platform`: `Auto` walks
the platform order, `Force` takes one tier or fails with
`TierError`. Orders: Windows Vulkan, D3D11, D3D12, Software;
Linux Vulkan, VA-API, Software; Android Vulkan, MediaCodec,
Software; elsewhere Vulkan, Software. Software H.265 resolves
only with a realtime backend marked. The facade re-exports
caps vocabulary (frames, packets, `Decoder`/`Encoder`,
transcode plans) and wires each backend crate as a feature;
default carries `vulkan`, `vaapi`, `mediacodec`. `build.rs`
derives one `*_available` cfg per OS-gated tier from feature
plus target, and gates corpus tests on `have_corpus`.

## Caps vocabulary

`crates/rust-video-caps` is the leaf every tier builds on.
`Packet` carries coded bytes. `CpuFrame` carries owned planar
bytes; `GpuFrame` carries identity only, with `try_to_cpu`
and `export` failing `BackendAbsent` until a tier attaches.
`GpuExport` is `DmaBuf` on Unix, `NtHandle` on Windows,
`Opaque` anywhere. `Decoder::decode` borrows the packet and
returns owned frames; `Encoder::encode` borrows a `CpuFrame`
and returns packets; both have `flush`. `SharedDevice`
leases VRAM bytes through `try_acquire`, failing `OverBudget`
past budget; `Lease` releases on drop. `FramePool` hands out
cleared buffers through `acquire` and takes uniquely owned
buffers back through `recycle`. Transcode vocabulary:
`GpuCaps` per-hop probes, `Plan` with `gpu`, `cpu`,
`for_caps`, `for_hops`, `for_devices` constructors, `Route`
(`Gpu` or caller-built `Cpu`), and `FallbackCause` naming
the first miss.

## Tier and model split

Tiers own devices, submit, and frames. Models own spec
values. The six platform and software tiers
(`rust-video-sw`, `rust-video-vulkan`, `rust-video-vaapi`,
`rust-video-d3d11`, `rust-video-d3d12`,
`rust-video-mediacodec`) depend on `rust-video-caps` alone
in the workspace outside dev-dependencies. Codec models
live two places: the six caps modules (`h264`, `h265`,
`av1`, `av2`, `vp8`, `vp9`) with `Params` plus level tables
read from the specifications, and the five namespace crates
(`rust-video-h264`, `rust-video-h265`, `rust-video-av1`,
`rust-video-av2`, `rust-video-vp9`), each one unit struct
with no behavior; software behavior for those codecs lives
in `rust-video-sw`. The models stay centralized as one
spec-checked set. `rust-video-harness` drives tiers under
test (vectors, oracles, synth inputs) and depends on the
facade plus caps. `rust-video-livepack` packs coded video
into TS and fMP4 and depends on no workspace crate. The
codec matrix in `../README.md` names every live cell.

## Data-plane ownership

Packet and plane bytes are refcounted (`bytes::Bytes`); a
clone shares the backing. The caller keeps the packet.
Each decoder owns its `FramePool`;
frames carry no pool handle, and shared buffers drop instead
of rejoining the pool. GPU frames pin their device resources
until dropped: a live Vulkan frame pins its export pair and
device, a VA-API surface frame leases its pool slot, D3D
frames hold their budget lease, MediaCodec frames hold
refcounted copies with no codec handle. Queues are bounded
and explicit: livepack `BoundedQueue` under `DropOldest`
evicts and reports, under `Block` refuses with `WouldBlock`
for the caller to retry. Pools recycle instead of
reallocating, and leases account every byte.
Full rules: `runtime.md`.

## Threading

Handles are thread-local unless stated. Vulkan: the device
is `Send` plus `Sync` with the queue mutex covering the
submit call only; decoders and encoders stay on one thread.
VA-API: every handle is `Send` without `Sync`; no lock
exists in the tier. D3D11: the immediate context is the
single submitter behind one device-level lock, waits run
outside it, MF events arrive on worker threads that only
signal. D3D12: no shared lock, sessions take `&mut self`,
fence waits park on the caller thread. MediaCodec: no handle
crosses threads; platform callback threads push through
`Send` senders and the driving thread polls. Software:
`FfmpegDecoder` is `Send` without `Sync`. Every tier runs
on the calling thread; waits are per-stream and bounded,
and each stream owns its queue.

## Decline policy

Selection happens up front. `tier::resolve` fails typed when no tier fits.
Each `open` declines machines it cannot run on instead of
erroring: Vulkan returns `Ok(None)` with `open_or_skip`
naming the rung through `SkipReason`; VA-API declines with
`SkipReason`; D3D11 and D3D12 decline through `Skipped`;
MediaCodec refuses below the floor with `BelowMinSdk` and
unknown mimes with `NoDecoder`. Staged codecs decline per
packet with typed `Unsupported` naming the codec, and
`flush` returns empty. Transcoders plan in caps vocabulary
and decline with the first miss named; `Plan::cpu` is the
caller-built vocabulary for the CPU route. Examples and
tests skip loud without hardware or input.

## Documents

| Document | Owns |
|---|---|
| `vulkan-arch.md` | Vulkan tier architecture |
| `vulkan.md` | Vulkan ops companion |
| `vaapi-arch.md` | VA-API tier architecture |
| `vaapi.md` | VA-API scope, provenance, audit, proof |
| `d3d11-arch.md` | D3D11 tier architecture |
| `d3d12-arch.md` | D3D12 tier architecture |
| `mediacodec-arch.md` | MediaCodec tier architecture |
| `mediacodec.md` | MediaCodec gates reference |
| `sw-codecs.md` | Software verdicts, figures, license |
| `matrix.md` | Backend policy matrix |
| `runtime.md` | Data-plane ownership rules |
| `livepack-ts-fmp4.md` | Container packaging detail |
| `compat.md` | Pins, floors, packaging, CI matrix |
| `sourcing.md` | Evidence tiers for external values |
| `../README.md` | Scope, codec matrix, quick start |
