# VA-API tier architecture

One negotiated display, one decode context, one four-picture submit pipeline per decoder: the VA-API tier runs H.264 decode, all-intra H.264 and H.265 encode, and GPU transcode behind the caps traits.

## Template

The per-tier skeleton is fixed by `docs/vulkan-arch.md`. Headings and tier rules stay identical across tier docs; the binding below is VA-API.

## Components

Tier rule: every tier names its device, negotiation, submit core, per-codec objects, and glue to the caps traits.

| Component | Responsibility |
|---|---|
| `VaApiDevice` | Negotiated display, entrypoint and export probes |
| `Backend` | Decode config, context, surface pool, pipelined submit |
| `BufferSet` | Reusable parameter buffers per in-flight picture |
| `H264Decoder` | Ingest, submit, readback, surface frames |
| `Frontend` | Annex B assembly, order counts, DPB orchestration |
| `Params` | Syntax onto VA-API picture and slice buffers |
| `VaApiEncoder` | All-intra CQP sessions, packed headers |
| `Transcoder` | Decode plus encode pair under one plan |
| `Scaler` | On-device VPP scale between surface sizes |
| `VaApiFrame` | NV12 bytes, lease, lazy DMA-BUF export |
| `VaBuffer` | Owned data buffer, map-rewrite, destroy on drop |
| `SubmitStats` plus `StatsInner` | Submit gauges, shared with exported frames |
| `BitReader` | Bits, Exp-Golomb, emulation prevention |
| `h264` | NAL units, syntax parser, DPB, picture state |
| `h265`, `vp8`, `vp9`, `av1` | Decoder handles declining typed `Unsupported` |
| `ffi` | Hand rebind of the used libva subset |
| `VaApiError`, `SkipReason` | Typed failures, machine decline |

VA-API: negotiation walks the render nodes under `/dev/dri` in order and keeps the first display that initializes with H.264 VLD into NV12, declining the machine with a named `SkipReason` otherwise. Each `unsafe` block sits at its FFI call site inside the owning module and carries its own justification; owned wrappers destroy every driver object on drop. The decoder owns its `Backend`, built from the first sequence set. The encoder owns its `Session` over one encode context. The transcoder owns its decoder and lazily opened encoder.

## Ownership and lifetimes

Tier rule: every tier states what owns GPU objects, what borrows the device, and what keeps a frame alive.

VA-API: `VaApiDevice` owns the display handle and the render-node file; terminating the display runs before the node closes. `H264Decoder<'a>` borrows the device and owns its `Backend` outright. The encoder and the scaler hold a raw display copy with a documented outlives requirement and own their sessions outright. A surface frame keeps its pool slot through a shared guard: the backend holds one clone per slot, the frame holds another, and the slot stays out of the free set while the frame lives.

`VaApiFrame` owns NV12 bytes, a budget `Lease`, and an optional `DeviceSurface`. `decode` copies pixels and releases its slot at once; `decode_surface` hands out the surface with the slot leased until the frame drops.

Drop order is field order. Buffers sit above their session, so parameter buffers free before the context that owns them. The layout is load-bearing: reordering struct fields changes teardown.

## Submit and complete

Tier rule: every tier names its submit unit, its completion signal, and how a failed submit shows up on the next call.

VA-API: the submit unit is one begun picture: `vaBeginPicture`, one `vaRenderPicture` over the render list, `vaEndPicture`. The backend rings four `BufferSet` slots per decode context. `submit` fills one set and queues without waiting; set reuse syncs that set's own previous surface, so the rewrite is safe. Callers sync at consumption: the NV12 readback, the DMA-BUF export, and the scaled-encode input each sync first. One `decode_surface` call over many packets submits every picture before any sync, so submit of frame N+1 never waits on frame N. The completion signal is `vaSyncSurface` per surface. Semaphores and fences: neither exists in the API; order within a context is submission order.

Gauges: `StatsInner` counts submitted pictures, synced surfaces from both the backend and the export path, failed submits, and driver buffers created. Peak in-flight is submitted-minus-synced. `SubmitStats` snapshots all four.

Recovery: a failed submit quiets its target, bumps the reset gauge, and returns the error on that call; the context stays usable and the following submit runs without a rebuild. No retry runs inside the tier: the caller decides.

Encode is blocking per call by API shape: submit, sync the source, harvest the coded segments, return the packet. One frame per call cannot overlap with itself. Scale overlaps one scale with one consume across its two outputs: the call returns before the scale finishes, reuse waits that output's own previous work, and the consumer syncs at use.

## Per-stream pipeline

Tier rule: every tier shows the packet-to-frame and frame-to-packet flows of one stream, including flush.

VA-API decode: packet bytes enter the NAL splitter, the parser reads each unit, slices group into pending pictures on first-VCL boundaries, and `begin` derives order counts, bridges frame-number gaps, and builds the initial reference lists. `ensure_backend` opens the decode context from the first sequence set. The free-slot scan skips DPB-backed and leased slots, the submit runs, `commit` applies reference marking, and the output slots read back into pooled bytes or surface frames. A mid-stream sequence change reads the drained slots under the old crop and drops the backend unless the pool still fits. Flush submits the open picture, then drains the DPB.

VA-API encode: a CPU frame faces NV12 format and size checks, copies through the upload image onto the upload surface, and encodes from there. A device surface skips the upload. Every picture codes as an IDR at CQP 26, and the packet carries driver headers ahead of the coded bytes marked a keyframe. Flush emits nothing: each IDR stands alone.

VA-API transcode: the plan fixes at open from per-hop caps plus the export and encode probes. The encoder opens lazily on the first frame size and reopens on size change. The GPU route runs `decode_surface` into `encode_surface`, scaling through the VPP pipeline on a size miss with one scaled picture held back so the following scale submits before the previous encode runs. The CPU route runs `decode` into `to_cpu` into `encode`, naming the first miss on stderr. A scale or convert miss fails loud with `Unsupported` on either route.

## Zero-copy handoff

Tier rule: every tier names its export type, its import path, and the exact match conditions.

VA-API: `export` syncs the surface and calls `vaExportSurfaceHandle` with read-only separate layers, yielding two NV12 DMA-BUF planes. Without a device surface, after a reconfig retired the pool, or when the driver refuses, the export is an opaque backend descriptor. A surface frame carries empty CPU bytes; its pixels stay on the GPU behind the leased slot.

Import takes the display and surface behind the frame. The checks run in order: same display, exact session size. Mismatch refuses with `Unsupported`. Every surface in the tier is NV12, so no format check runs. The DMA-BUF export probe runs a 64x64 surface through the export call at device open; the encode probe checks the entrypoint per codec at transcoder open.

## Memory model

Tier rule: every tier names its allocation kinds, its budget, and its reuse pools.

VA-API: `VaBuffer` is the owned data buffer: created once, rewritten by mapping, recreated only when a payload overflows it, destroyed on drop. The surface pool is fixed at backend open: DPB capacity plus four. The encoder holds one upload image, one upload surface, and two reconstruction surfaces. The scaler holds two outputs. Readback and harvest allocate one `Vec` per frame or packet; those bytes belong to the caller.

The decode budget is an unbounded readback budget: each frame leases its byte count, which documents that output frames are copies. The fixed surface pool cannot overrun.

Reuse pools: four `BufferSet` slots per decode context with grow-only slice buffers, rewritten parameter buffers per picture, one upload image per encode session, one picture header pack per quantization, and decoder scratch for slice parameters and reference lists. Steady-state pictures create no buffers; a buffer recreates only when a payload passes its largest size so far.

## Threading

Tier rule: every tier states which handles cross threads and what serializes shared use.

VA-API: every handle is `Send` without `Sync`: the device, the backend, the decoder, the encoder, the scaler, and the frame. Each moves across threads only when idle, and every call runs on one thread. No `Mutex` or `RwLock` exists in the tier; the only shared ownership is `Arc` guards and atomic gauges, so no wait can sit under a shared lock. Decoders and encoders are thread-local: parser state plus one codec object each. No background threads exist in the tier. Surface waits are unbounded driver waits on the calling thread; nothing serializes them because nothing shares them.

## Codec structures

Tier rule: every tier separates reference and ordering state per codec.

VA-API decode: `Frontend` holds the parser, the active sequence set, the DPB over surface slots, the order-count tracker, and the open picture. `begin` derives counts per section 8.2.1, bridges gaps with recorded non-existing frames, and builds initial lists per 8.2.4. `modify_into` rebuilds each slice list without allocating. `commit` marks, stores, and bumps the picture, returning output slots in display order. The DPB drains through the bumping process at flush. A reference list naming a picture the DPB cannot supply fails typed `BrokenReference`.

VA-API encode: IDR-only. Every picture codes without references. The frame number, IDR id, and order count derive from the picture count. Rate control is CQP 26 clamped to 51, packed once per quantization. The feedback is the coded size in the harvested segments. The driver emits parameter sets from the packed headers, and every packet prepends those headers.

H.265, VP8, VP9, and AV1 each export a decoder handle from a shim module. Every `decode` call declines typed `Unsupported` naming the codec, and `flush` returns empty. Their syntax subdirectories carry no `mod` declaration and do not join the build.

## Invariants

Tier rule: each invariant names the file that enforces it, so a reviewer checks each without running the tier.

- Every `unsafe` block sits at its FFI call site and carries its own justification (`device.rs`, `backend.rs`, `decoder.rs`, `encoder.rs`, `scale.rs`, `buffer.rs`, `error.rs`).
- `submit` returns before completion; every sync happens at consumption or at set reuse (`backend.rs`, `decoder.rs`, `encoder.rs`).
- A failed submit quiets its target and leaves the context reusable; no retry runs inside the tier (`backend.rs`).
- Slice payloads sub-slice the packet bytes; reference lists rebuild in decoder scratch (`frontend.rs`, `decoder.rs`).
- `decode` copies pixels and releases its slot at once; `decode_surface` leases its slot until the frame drops (`decoder.rs`).
- Export syncs before the handle export and answers opaque without a surface or on refusal (`decoder.rs`).
- Encode refuses a foreign display or a size miss before touching the driver (`encoder.rs`, `scale.rs`).
- The scaler returns before the scale finishes; reuse waits its own previous work and the consumer syncs at use (`scale.rs`, `encoder.rs`).
- Reconfig reads drained slots under the old crop and drops the backend unless the pool still fits (`decoder.rs`).
- The crop refuses a window outside the coded extent (`backend.rs`).
- Buffers rewrite in place and recreate only on overflow; steady-state pictures create none (`buffer.rs`, `backend.rs`, `encoder.rs`).
- Every driver object dies with its owner: buffers, contexts, surfaces, configs, images, display (`buffer.rs`, `backend.rs`, `encoder.rs`, `scale.rs`, `device.rs`).
- Buffers sit above their session, so teardown frees parameters before the context (`encoder.rs`, `scale.rs`).
- No `Mutex` or `RwLock` exists in the tier; all handles are `Send` without `Sync` (`device.rs`, `backend.rs`, `decoder.rs`, `encoder.rs`, `scale.rs`).
- A picture before any sequence set fails typed; a geometry change rebuilds the codec object (`decoder.rs`).
- Non-H.264 decoders decline typed naming the codec; their flush returns empty (`h265.rs`, `vp8.rs`, `vp9.rs`, `av1.rs`).
- Off Linux the crate compiles empty; every module is Linux-gated at the root (`lib.rs`).

## File map

Tier rule: one row per file, paths relative to the tier crate.

| File | Owns |
|---|---|
| `src/lib.rs` | Module list, public re-exports |
| `src/device.rs` | Negotiated display, node walk, probes |
| `src/backend.rs` | Decode submit core, surface pool, stats |
| `src/decoder.rs` | H.264 decoder, frames, DMA-BUF export |
| `src/frontend.rs` | Annex B assembly, order counts, DPB |
| `src/params.rs` | Syntax onto VA-API buffers |
| `src/encoder.rs` | All-intra CQP sessions, packed headers |
| `src/scale.rs` | VPP scaler, two-output overlap |
| `src/transcode.rs` | GPU and CPU routes, lazy encoder |
| `src/buffer.rs` | Owned data buffers, map-rewrite |
| `src/bitstream.rs` | Bit reader, Exp-Golomb |
| `src/error.rs` | Skip reasons, typed failures |
| `src/ffi.rs` | Hand rebind of the used libva subset |
| `src/h264.rs` | H.264 module root |
| `src/h264/nalu.rs` | NAL unit splitting |
| `src/h264/parser.rs` | H.264 syntax parser |
| `src/h264/dpb.rs` | Decoded-picture buffer |
| `src/h264/picture.rs` | Picture state, counts, marking |
| `src/h265.rs` | H.265 handle, typed decline |
| `src/vp8.rs` | VP8 handle, typed decline |
| `src/vp9.rs` | VP9 handle, typed decline |
| `src/av1.rs` | AV1 handle, typed decline |
| `tests/` | Hardware-gated integration tests |
| `examples/` | Decode, encode, transcode runners |
