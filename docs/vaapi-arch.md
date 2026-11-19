# VA-API tier architecture

One negotiated display, one decode context, one four-picture submit pipeline per decoder: H.264, H.265, VP9, and AV1 decode, all-intra H.264 and H.265 encode, and GPU transcode behind the caps traits.

## Negotiation

The node walk tries the render nodes under `/dev/dri` in order and keeps the first display that initializes with H.264 VLD into NV12, declining the machine with a named `SkipReason` otherwise. The decoder owns its backend, built from the first sequence set. The encoder owns its session over one encode context.

## Submit pipeline

The submit unit is one begun picture: `vaBeginPicture`, one `vaRenderPicture` over the render list, `vaEndPicture`. The backend rings four buffer slots per decode context. Submit fills one slot and queues without waiting; slot reuse syncs that slot's own previous surface. Sync happens at consumption: the NV12 readback, the DMA-BUF export, and the scaled-encode input each sync first. One `decode_surface` call over many packets submits every picture before any sync. The completion signal is `vaSyncSurface` per surface; order within a context is submission order.

A failed submit quiets its target, bumps the reset gauge, and returns the error on that call; the context stays usable and the following submit runs without a rebuild. No retry runs inside the tier.

Encode is blocking per call by API shape: submit, sync the source, harvest the coded segments, return the packet. Scale overlaps one scale with one consume across two outputs: the call returns before the scale finishes, reuse waits that output's own previous work, and the consumer syncs at use.

## Decode and encode

Decode: packet bytes enter the NAL splitter, the parser reads each unit, slices group into pending pictures on first-VCL boundaries. Begin derives order counts, bridges frame-number gaps, and builds the initial reference lists. The free-slot scan skips DPB-backed and leased slots, the submit runs, commit applies reference marking, and output slots read back into pooled bytes or surface frames. A mid-stream sequence change reads drained slots under the old crop and drops the backend unless the pool still fits. Flush submits the open picture, then drains the DPB.

Encode: a CPU frame faces format and size checks, copies through the upload image onto the upload surface, and encodes from there. A device surface skips the upload. Every picture codes as an IDR at CQP 26, and the packet carries driver headers ahead of the coded bytes marked a keyframe.

## Zero-copy handoff

Export syncs the surface and calls `vaExportSurfaceHandle` with read-only separate layers, yielding two NV12 DMA-BUF planes. Without a device surface, after a reconfig retired the pool, or when the driver refuses, the export is an opaque backend descriptor. A surface frame carries empty CPU bytes; its pixels stay on the GPU behind the leased slot. Import takes the display and surface behind the frame and checks same display, then exact session size. Every surface in the tier is NV12, so no format check runs.

## Memory

The owned data buffer is created once, rewritten by mapping, recreated only when a payload overflows it, destroyed on drop. The surface pool is fixed at backend open: DPB capacity plus four. The encoder holds one upload image, one upload surface, and two reconstruction surfaces. Readback and harvest allocate one vector per frame or packet; those bytes belong to the caller. The decode budget is an unbounded readback budget: each frame leases its byte count, documenting that output frames are copies.

## Threading

Every handle is `Send` without `Sync` and moves across threads only when idle; every call runs on one thread. No mutex exists in the tier; the only shared ownership is `Arc` guards and atomic gauges. No background threads exist. Surface waits are unbounded driver waits on the calling thread.

## Invariants

- Every `unsafe` block sits at its FFI call site and carries its own justification.
- Submit returns before completion; every sync happens at consumption or at slot reuse.
- Slice payloads sub-slice the packet bytes; reference lists rebuild in decoder scratch.
- `decode` copies pixels and releases its slot at once; `decode_surface` leases its slot until the frame drops.
- Encode refuses a foreign display or a size miss before touching the driver.
- Reconfig reads drained slots under the old crop and drops the backend unless the pool still fits.
- The crop refuses a window outside the coded extent.
- Buffers rewrite in place and recreate only on overflow; steady-state pictures create none.
- Every driver object dies with its owner.
- Buffers sit above their session, so teardown frees parameters before the context.
- A picture before any sequence set fails typed; a geometry change rebuilds the codec object.
- Off Linux the crate compiles empty.
