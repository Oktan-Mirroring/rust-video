# MediaCodec tier architecture

One platform codec, one polled event queue, one pending-input stash per decoder: the MediaCodec tier runs H.264, H.265, AV1, VP9, and VP8 byte-buffer decode behind the caps traits.

## Template

This file follows the per-tier skeleton. Headings match `docs/vulkan-arch.md`. Each section opens with its tier rule, then the MediaCodec binding.

## Components

Tier rule: every tier names its device, negotiation, submit core, per-codec objects, and glue to the caps traits.

| Component | Responsibility |
|---|---|
| `MediaCodecDevice` | Probed tier, API floor |
| `ApiLevel` | Device level, SDK gates |
| `FormatSpec` | Codec input format, five mimes |
| `MediaCodecDecoder` | Lifecycle, layout, quirk match |
| `DecodeState` | Configure, start, release machine |
| `Inner` | Owned codec handle, drain dispatch |
| `DrainMode` plus `AsyncDrain` | Sync dequeue or async drain |
| `EventQueue` plus `CodecEvent` | Callback intake, polled drain |
| `PendingInputs` | Input stash in arrival order |
| `OutputLayout` | Geometry, plane splits |
| `QuirkTable` plus `Quirk` | SoC quirk match |
| `MediaCodecError` | Typed tier failures |

MediaCodec: open reads the live device level and refuses below the floor with `BelowMinSdk`. Codec lookup answers by mime, or `NoDecoder` names the mime. Level 28 and up registers all four async callback slots into one `EventQueue`; lower levels use sync dequeue with bounded waits. The `Decoder` trait impl is the caps glue: `decode` queues one packet and drains, `flush` drains without touching the codec. Off Android the native decoder compiles out and the pure pieces (formats, quirks, state machine, layout, queue) stand alone.

## Ownership and lifetimes

Tier rule: every tier states what owns GPU objects, what borrows the device, and what keeps a frame alive.

MediaCodec: each decoder owns its codec handle, its queue, its stash, and its layout outright. Nothing borrows the device: `MediaCodecDevice` is a probe value, and `open` takes an SoC string plus a quirk table, not a device handle. The ndk codec handle is neither `Send` nor `Sync`, so a decoder lives on its creating thread from open to drop. A `CpuFrame` owns refcounted bytes plus plane views over them and holds no codec handle; releasing the output buffer happens before the frame exists. Drop stops the codec and marks the state released.

## Submit and complete

Tier rule: every tier names its submit unit, its completion signal, and how a failed submit shows up on the next call.

MediaCodec: the submit unit is one packet copied into one input buffer through `queue_input_buffer`. The completion signal is an output event on the queue, or a dequeue result on the sync path. A failed queue returns typed on the same call: `InputTooLarge` for an overfull packet, `BufferLost` for a vanished buffer, `InputStarved` after three 10 ms attempts with no input buffer. No failure poisons the decoder; the next call retries fresh. The stash keeps unused input indices across calls. A call holding a stashed index queues without waiting. Decode-only outputs release without a frame on both paths.

## Per-stream pipeline

Tier rule: every tier shows the packet-to-frame and frame-to-packet flows of one stream, including flush.

MediaCodec decode: the state gate runs first, packet PTS becomes microseconds, and the async path pops a stashed input index or waits one bounded batch for fresh events while pulling any outputs in the batch. Packet bytes copy into the input buffer, the buffer queues, and the drain pulls each output range into refcounted bytes and splits planes under the negotiated layout. The sync path dequeues its input with the same bounded attempts and drains outputs between attempts. Flush drains available outputs and stops there; it never calls codec flush. Reset drains, clears the stash, flushes the codec, and restarts async callbacks.

No encode path exists. The tier decodes only; frame-to-packet flow is absent.

## Zero-copy handoff

Tier rule: every tier names its export type, its import path, and the exact match conditions.

MediaCodec: no export type and no import path exist. Every frame copies once from the codec buffer into refcounted bytes; plane splits share that copy without further copies. Input is packet bytes, output is a `CpuFrame`. Match conditions do not apply.

## Memory model

Tier rule: every tier names its allocation kinds, its budget, and its reuse pools.

MediaCodec: the platform owns the input and output buffers. The tier allocates one `Bytes` copy per frame plus plane views over it; splits share the backing without copying. No VRAM budget and no reuse pools exist: frames drop and free. Layout starts tentative from the spec and negotiates on the first output format. Stride and slice height read from the format, falling back to width and height when absent. The stash holds each input index at most once and stays under the codec buffer count.

## Threading

Tier rule: every tier states which handles cross threads and what serializes shared use.

MediaCodec: no decoder handle crosses threads. The `&mut self` trait surface serializes each stream on its driving thread. Codec-owned callback threads push events through `Send` senders; the driving thread polls the receiver. No mutex exists in the tier. Waits are `recv_timeout` plus dequeue timeouts, each bounded, with no lock held across any of them. Multi-stream runs one decoder per thread. The tier owns no background threads; callbacks run on platform codec threads.

## Codec structures

Tier rule: every tier separates reference and ordering state per codec.

MediaCodec: the platform codec holds references and order. The tier holds no DPB and no reorder buffer; per-stream state is the `DecodeState` lifecycle, the `OutputLayout` geometry, and the `PendingInputs` stash. A color value outside planar 19 and semi-planar 21 fails typed as `UnsupportedColorFormat`. Decode-only frames release without entering the layout.

## Invariants

Tier rule: each invariant names the file that enforces it, so a reviewer checks each without running the tier.

- `unsafe` appears only in `src/sdk.rs`, one justified block.
- Async registers all four callback slots; an unset slot faults on Android 12 and earlier. (`src/bridge.rs`)
- Input indices never re-enter the queue; extras wait in the stash in arrival order. (`src/bridge.rs`)
- A stashed index queues without waiting; an empty stash waits at most three bounded batches. (`src/decoder.rs`)
- Reset clears the stash before flushing the codec. (`src/decoder.rs`)
- Flush drains without calling codec flush. (`src/decoder.rs`)
- Decode and flush hold no lock; every wait names its own timeout. (`src/decoder.rs`)
- One thread drives a stream through `&mut self`. (`src/decoder.rs`)
- The raw codec handle never leaves `src/decoder.rs`.
- API-28 entry points never run below level 28; the gate is the live device level. (`src/decoder.rs`)
- Open below the floor fails with `BelowMinSdk` naming the level. (`src/decoder.rs`)
- A packet before start fails with `InvalidState` naming the op. (`src/decoder.rs`)
- Split runs only after negotiation; the drain refreshes the layout first. (`src/decoder.rs`)
- Every output range checks against its buffer; overruns fail as `BufferRange`. (`src/decoder.rs`)
- Drop stops the codec and marks the state released. (`src/decoder.rs`)
- Format builders reject zero and oversize dims. (`src/format.rs`)
- The quirk parser rejects shape breaks with line numbers. (`src/quirks.rs`)

## File map

Tier rule: one row per file, paths relative to the tier crate.

| File | Owns |
|---|---|
| `src/lib.rs` | Module list, public re-exports |
| `src/bridge.rs` | Event queue, input stash |
| `src/decoder.rs` | Decoder, drains, output layout |
| `src/device.rs` | Probed tier device |
| `src/error.rs` | Typed tier failures |
| `src/format.rs` | Input formats, color map |
| `src/quirks.rs` | Quirk table, matching |
| `src/sdk.rs` | API level, SDK gates |
| `quirks.toml` | Embedded quirk source |
| `tests/decode.rs` | Device decode matrix |
| `examples/decode_matrix_mc.rs` | Device decode matrix runner |
