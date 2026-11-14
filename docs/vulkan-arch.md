# Vulkan tier architecture

One negotiated device, one video queue, one three-slot submit ring per codec object: the Vulkan tier runs H.264 and H.265 decode, GOP encode, and GPU transcode behind the caps traits.

## Template

This file defines the per-tier skeleton. Headings stay fixed across tier docs. Each section opens with its tier rule, then the Vulkan binding. A vaapi or mediacodec doc keeps the headings and tier rules and swaps the binding; the d3d11 and d3d12 docs use the ops-focused format instead.

## Components

Tier rule: every tier names its device, negotiation, submit core, per-codec objects, and glue to the caps traits.

| Component | Responsibility |
|---|---|
| `VulkanDevice` | Negotiated device, queue, budget |
| `Ladder` plus `Backend` | Op negotiation over injected queries |
| `AshBackend` plus `RawDevice` | Owned loader, instance, logical device |
| `Session`, `EncodeSession` | Owned GPU resources per codec object |
| `H264Decoder`, `H265Decoder` | Parse, submit, reorder, emit frames |
| `H264Encoder`, `H265Encoder` | GOP packets from CPU or GPU input |
| `Transcoder` | Decode plus encode pair under one plan |
| `FrameSink` | Reorder, budget, staging, plane pools |
| `SubmitSlots` plus `SubmitStats` | Pooled submit slots, completion tracking, gauges |
| `Inflight` plus `Fence` | Pending completion collection |
| `parse` | Splitters, syntax parsers, contexts |
| `params`, `std_h264`, `std_h265` | Parameter sets, StdVideo fills |
| `geometry` | Sizing validated against ladder caps |

Vulkan: negotiation runs rungs in fixed order (loader, instance version, 1.3 floor, extension strings, video caps, queue family, device open) and declines the machine with a named `SkipReason` instead of an error. `raw` is the sole `unsafe` module: every Vulkan call lands there, each block carries its own justification, and no raw handle escapes. Decoders borrow the device and own an optional `Session` built from the first sequence set. Encoders own an `Engine` over one `EncodeSession`. The transcoder borrows both devices and owns a lazily opened encoder.

## Ownership and lifetimes

Tier rule: every tier states what owns GPU objects, what borrows the device, and what keeps a frame alive.

Vulkan: `OwnedDevice` is the shared root. The device, every pooled plane pair, and every escaped frame hold it through `Arc`, so a live frame pins its planes and the device outlives all of them. A decoder borrows `&VulkanDevice` for its whole life and owns its `Session` outright. An encoder owns its `EncodeSession` outright. `Transcoder<'a>` borrows the decode and encode devices and owns the encode hop.

`VulkanFrame` owns refcounted NV12 bytes, a budget `Lease`, and an optional `DeviceExport`. The lease releases VRAM budget on drop. The export pins its `Arc<ExportPair>`, which keeps the pool slot out of reuse. Frames carry no pool handle; buffer return is the explicit `recycle` call, and shared buffers drop instead of rejoining the pool.

Drop order is field order. Parameters sit above the video object, views above their images, so dependents free first. The layout is load-bearing: reordering struct fields changes teardown.

## Submit and complete

Tier rule: every tier names its submit unit, its completion signal, and how a failed submit shows up on the next call.

Vulkan: the submit unit is one recorded command buffer. `SubmitSlots` rings three slots per codec object, each with its own command buffer plus fence, and each session keeps per-slot staging behind them. Submit and poll split: `submit_async` records and queues without waiting, `collect` waits the slot fence outside the queue lock and maps that slot's readback. Decode commits planned layouts once the submit queues; same-queue submission order plus the recorded barriers carry the layout assumptions to the device. One `decode` call submits every picture before collecting, so record of frame N+1 never waits on frame N. The blocking encode wrappers submit and collect adjacently: one frame per call cannot overlap with itself.

Slots: DPB slots map reference ids to image layers through `SlotBook`. Submit slots rotate round-robin; a full ring waits one busy slot outside the lock, back-pressuring the producer to the ring depth. Semaphores: none exist in the tier. Cross-submit order comes from same-queue submission order plus the recorded barriers, never from a signal object.

Reaping: `Inflight` collects one completion token per submitted frame. `submit` tracks a token, `reap_ready` takes signaled tokens without blocking, `drain` waits each fence with its own timeout. `SlotFence` implements `Fence` over the slot's shared fence. Peak inflight plus reset counts surface as `SubmitStats`.

Recovery: a failed submit resets the ring plus the session layouts and reference bookkeeping, then retries the submit once, loud on stderr. A failed poll resets the same state and fails; its completion is unknown, so no retry runs. Completed siblings keep their fences and tokens through a reset, so their readbacks still collect. A failed idle poisons the ring until an idle succeeds.

## Per-stream pipeline

Tier rule: every tier shows the packet-to-frame and frame-to-packet flows of one stream, including flush.

Vulkan decode: packet bytes enter the NAL splitter, the codec parser reads each unit, the access-unit splitter groups them, and the reference context compiles one unit into set updates plus at most one picture. `ensure_session` opens or rebuilds the codec object on geometry change. Released slots retire, the `Submission` chains assemble from reused buffers, and the submit runs the CPU readback path or the GPU export path. Readback maps into reused staging, crops coded extent down to display extent, and lands in the pooled buffer or leased planes. The reorder buffer releases frames in display order. Flush drains the splitter, the pending units, then the reorder buffer.

Vulkan encode: a CPU frame or device frame faces NV12 format and extent checks, takes a `PictureStamp`, and encodes through the upload path or the import path. IDR pictures code without references; P pictures predict from the single live reference. Keyframe packets carry driver headers ahead of the coded bytes and the keyframe mark; P packets carry coded bytes alone. Flush emits nothing: no delayed frames exist.

Vulkan transcode: the plan fixes at open from per-hop caps plus same-adapter proof. The encoder opens lazily on the first frame size and reopens on size change. The GPU route runs `decode_surface` into `encode_export` with pixels never mapped. A caps miss declines with `Unsupported` naming the first miss. A scale or convert miss fails loud with `Unsupported`.

## Zero-copy handoff

Tier rule: every tier names its export type, its import path, and the exact match conditions.

Vulkan: a GPU decode submit lands in a leased `ExportPair`: two exportable images, luma plus chroma, cropped to display extent. `export` yields DMA-BUF planes on Unix and an opaque backend descriptor without an export. A surface frame carries empty CPU bytes; its pixels stay on the GPU behind the pinned pair.

Import checks the source format first, then the extent, and refuses mismatch with `Unsupported` or `OverCaps`. The import reuses its `ImportPair` while the pair pointer still matches and re-imports from fresh file descriptors otherwise. Both sides require two-plane NV12. Probes at transcoder open decide the caps: DMA-BUF export, DMA-BUF import, and a two-plane decode format. The share mechanism derives from extension strings: DMA-BUF on Linux, NT handle on Windows. Same-adapter proof compares fingerprint hashes, since raw handles differ per instance on one GPU.

## Memory model

Tier rule: every tier names its allocation kinds, its budget, and its reuse pools.

Vulkan: `GpuMemory`, `GpuBuffer`, `GpuImage`, and `GpuView` are owned wrappers that free on drop. Host-visible coherent memory backs the staging buffers: 16 MiB bitstream upload, coded-size readback, encode upload. Device-local memory backs each DPB array image with one layer per slot plus the encode source image.

The VRAM budget is a `SharedDevice` seeded from device-local heap bytes. Each frame holds a `Lease` against it; excess fails with `OverBudget` naming requested and available. Reuse pools: one `FramePool` per coded size, one export entry per display extent. A live frame pins its planes through the shared pair; a new size drops idle entries and creates one. Scratch never reallocates per frame: coded staging, tight NV12 packing, begin and reference chain buffers, and planar conversion into a caller buffer. Bitstream upload zero-pads through the aligned range and refuses oversize units. Readback maps the coded length, never the allocation size.

## Threading

Tier rule: every tier states which handles cross threads and what serializes shared use.

Vulkan: `VulkanDevice` is `Send` plus `Sync`. The queue mutex covers the queue-submit call only; fence waits and resets run outside it, so a waited submit never blocks another thread's submit. Decoders and encoders are thread-local: parser state plus one codec object, with reference-counted sets that never leave the creating thread. The transcoder borrows both devices and runs on its calling thread. A poisoned queue mutex surfaces as `QueuePoisoned`, never a hang. No fence wait exceeds five seconds. One `decode` call overlaps its pictures through the slot ring; the blocking trait surface stays serial per call. No background threads exist in the tier.

## Codec structures

Tier rule: every tier separates reference and ordering state per codec.

Vulkan decode: `SlotBook` maps stable reference ids to DPB slots with claim, release, and release-all. `DpbMeta` holds each slot's StdVideo reference info, and a layout array tracks each layer. The begin chain carries active slots, the refs chain carries decode references, and the setup slot claims the decode target. A reset issues the RESET control and clears metadata plus slots. The reorder buffer holds pictures keyed by order count with capacity from the sequence set (`max_num_reorder_frames` for H.264, `max_num_reorder_pics` for H.265) and flushes everything on a sequence reset, since order counts restart there. The H.264 context applies marking with long-term ids; the H.265 context applies reference-set echo. Unknown references answer by resync, never by guessing.

Vulkan encode: `SlotBook` maps stable reference ids to DPB slots with claim, release, and release-all, and `EncodeMeta` holds each slot's order count plus frame number. A P-capable session runs two DPB slots: one holds the live reference while the setup slot receives the reconstructed picture, then the reference retires. Each P picture carries a single-entry L0 list over the live slot; no B-frames exist. Each picture takes a `PictureStamp` with a wrapping IDR id, an advancing order count, a frame number modulo 16, and its IDR flag. `Gop` sets the IDR cadence: an IDR every 30 pictures by default, every picture as IDR in all-intra mode. A P without a live reference codes as IDR instead. Rate control is CBR per extent at 30 frames per second with a 64 kbit floor. The feedback query reports the coded size. The driver emits parameter sets from the codec sets, and keyframe packets prepend those headers.

## Invariants

Tier rule: each invariant names the file that enforces it, so a reviewer checks each without running the tier.

- `unsafe` appears only in `src/raw.rs`, and every block carries its justification.
- `Session` and `EncodeSession` declare the submit ring first, so teardown idles the device before dependents free; parameters sit above the video object and views above their images.
- Decoder and encoder open reject a device negotiated for another op with `OverCaps`.
- A picture before any parameter set returns `NoSession`; a geometry change rebuilds the codec object.
- Bitstream upload pads to the aligned range and refuses oversize units with `OverCaps`.
- Readback maps the coded length, never the allocation size.
- Every encode begin carries the CBR control; rate state comes from the begin chain.
- Encode P pictures predict from the single live reference; no B-frames exist.
- A P without a live reference codes as IDR and its packet marks the keyframe.
- A failed submit resets the ring plus session state and retries once; a failed poll resets and fails.
- Every `SubmitSlots` error leaves the ring reset and requires the caller to reset session state.
- The queue mutex covers the submit call only; fence waits run outside it.
- Encode import checks source format before extent and refuses mismatch.
- Export and import both require two-plane NV12 and refuse anything else.
- Live frames pin their export pair; the pool reuses only a size-matching slot with a sole owner.
- Queue lock poison surfaces as `QueuePoisoned`; every fence wait names its own timeout.
- The NAL splitter drains consumed bytes once per push, so input buffering stays bounded.
- The crop refuses a display window outside the coded extent.

## File map

Tier rule: one row per file, paths relative to the tier crate.

| File | Owns |
|---|---|
| `src/lib.rs` | Module list, public re-exports |
| `src/device.rs` | Negotiated device, queue, budget |
| `src/ladder.rs` | Negotiation over injected backend |
| `src/raw.rs` | Owned handles, sole unsafe code |
| `src/inflight.rs` | Fence collection, reaping |
| `src/decoder.rs` | Decoders, frames, display sink |
| `src/encoder.rs` | GOP engines, packet assembly |
| `src/transcode.rs` | GPU route with typed decline, lazy encoder |
| `src/error.rs` | Skip reasons, typed failures |
| `src/session/decode.rs` | Decode submit core, DPB image |
| `src/session/encode.rs` | Encode submit core, reference tracking, rate control |
| `src/session/slots.rs` | Submit-slot ring, completion tracking, recovery |
| `src/session/geometry.rs` | Sizing validation, level maps |
| `src/session/params.rs` | Parameter set templates |
| `src/session/std_h264.rs` | H.264 StdVideo fills |
| `src/session/std_h265.rs` | H.265 StdVideo fills |
| `src/parse/nal.rs` | Annex-B splitting |
| `src/parse/au.rs` | Access unit grouping |
| `src/parse/bits.rs` | Bit reader |
| `src/parse/h264.rs` | H.264 syntax parser |
| `src/parse/h265.rs` | H.265 syntax parser |
| `src/parse/refs.rs` | Contexts, slot book, reorder |
| `tests/` | Hardware-gated integration tests |
| `examples/` | Decode, encode, transcode runners |
