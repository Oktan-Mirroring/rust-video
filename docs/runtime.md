<!-- source: runtime data-plane design, 2026-10-11; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Runtime data plane

Ownership and flow rules for live frames. Code homes: packet and frame
types in `crates/rust-video/src/`, slots and queues in
`crates/rust-video-livepack/src/lib.rs`, allocation proof in
`crates/rust-video-harness/src/alloc_tracker.rs`.

## Ownership

- Packet and plane bytes are refcounted (`bytes::Bytes`). A clone shares
  the backing; only `try_into_mut` on a unique handle reuses the buffer.
- `Decoder::decode` borrows the packet and returns owned frames. The
  caller keeps the packet; the decoder never stores it.
- `GpuFrame` carries identity only (size, format, generation). It has no
  bytes accessor. CPU access is the explicit `try_to_cpu` call, which
  fails until a backend lands.
- `FramePool` is owned by one decoder. Frames carry no pool handle;
  returning a buffer is the explicit `recycle` call, and shared buffers
  drop instead of rejoining the pool.

## Pool, queue, and sync discipline

- One pool per decoder, sized for that decoder's frames. `acquire` hands
  out a cleared recycled buffer or a fresh one; `recycle` thaws only
  uniquely owned buffers.
- Queues are bounded and explicit. `FrameSlot` holds the latest value;
  `BoundedQueue` under `DropOldest` evicts the oldest entry and returns
  it to the caller; under `Block` a full queue fails with `WouldBlock`
  and the caller retries. Nothing is unbounded, nothing drops silently.
- One stalled stream never blocks the pipeline: a full queue refuses or
  evicts, it never parks the producer, and each stream owns its queue.
- Sync is per-stream timeline semaphores: each stream advances its own
  counter, and a wait names its stream plus value. No idle wait exists:
  a wait always names the value it needs, and a stalled stream waits on
  its own timeline only.

## Export matrix

`GpuFrame::export` returns the platform handle for zero-copy handoff.
All variants compile on all targets; behavior arrives with the tiers.

| Platform | Variant | Handle |
|---|---|---|
| Unix | `DmaBuf` | fd, stride, offset, modifier |
| Windows | `NtHandle` | NT handle |
| Any | `Opaque` | backend name |

Until a backend attaches, `export` and `try_to_cpu` return
`BackendAbsent`.

## VRAM budget

- Each `SharedDevice` carries a byte budget. `try_acquire` leases bytes
  against it or fails with `OverBudget` naming requested and available.
- A `Lease` releases on drop. Cloned handles share one budget.
- Tier crates attach real devices later; the facade rule stands now:
  no allocation beyond budget, no silent overrun.
