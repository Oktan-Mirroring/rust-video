# Vulkan tier architecture

One negotiated device, one video queue, one three-slot submit ring per codec object: H.264 and H.265 decode, GOP encode, and GPU transcode behind the caps traits.

## Negotiation

Rungs run in fixed order: loader, instance version, 1.3 floor, extension strings, video caps, queue family, device open. A miss declines the machine with a named `SkipReason` instead of an error.

## Submit ring

The submit unit is one recorded command buffer. Three slots per codec object rotate round-robin, each with its own command buffer plus fence; a full ring waits one busy slot outside the queue lock. `submit_async` records and queues without waiting, `collect` waits the slot fence outside the lock and maps that slot's readback. One `decode` call submits every picture before collecting, so frame N+1 records while frame N completes.

A failed submit resets the ring plus session state and retries once; a failed poll resets and fails without retry, since its completion is unknown. Completed siblings keep their fences through a reset, so their readbacks still collect.

## Decode and encode

Decode: packet bytes enter the NAL splitter, the codec parser reads each unit, the access-unit splitter groups them, and the reference context compiles one unit into set updates plus at most one picture. Sessions open or rebuild on geometry change. Readback maps into reused staging, crops coded extent to display extent, and lands in pooled buffers. The reorder buffer releases frames in display order keyed by order count, with capacity from the sequence set.

Encode: IDR pictures code without references; P pictures predict from the single live reference. No B-frames exist. An IDR lands every 30 pictures by default, every picture in all-intra mode. A P without a live reference codes as IDR. Rate control is CBR per extent; keyframe packets prepend driver headers.

## Zero-copy handoff

A GPU decode submit lands in a leased export pair: two exportable images, luma plus chroma, cropped to display extent. `export` yields DMA-BUF planes on Unix and an opaque backend descriptor without an export. Import checks source format, then extent, and refuses mismatch. Both sides require two-plane NV12. Same-adapter proof compares fingerprint hashes, since raw handles differ per instance on one GPU.

## Memory

Owned wrappers free on drop. Host-visible coherent memory backs staging: 16 MiB bitstream upload, coded-size readback, encode upload. Device-local memory backs each DPB array image plus the encode source image. Each frame holds a `Lease` against the VRAM budget seeded from device-local heap bytes; excess fails with `OverBudget`. One frame pool per coded size, one export entry per display extent. Bitstream upload zero-pads through the aligned range and refuses oversize units.

## Threading

The device is `Send` plus `Sync`. The queue mutex covers the submit call only; fence waits and resets run outside it. Decoders and encoders are thread-local. Lock poison surfaces as `QueuePoisoned`, and no fence wait exceeds five seconds. No background threads exist in the tier.

## Invariants

- `unsafe` appears only in the raw-handle module, and every block carries its justification.
- Sessions declare the submit ring first, so teardown idles the device before dependents free; parameters sit above the video object and views above their images.
- A picture before any parameter set returns `NoSession`; a geometry change rebuilds the codec object.
- Bitstream upload pads to the aligned range and refuses oversize units.
- Readback maps the coded length.
- Every encode begin carries the CBR control; rate state comes from the begin chain.
- A P without a live reference codes as IDR and its packet marks the keyframe.
- Live frames pin their export pair; the pool reuses only a size-matching slot with a sole owner.
- The NAL splitter drains consumed bytes once per push, so input buffering stays bounded.
- The crop refuses a display window outside the coded extent.
