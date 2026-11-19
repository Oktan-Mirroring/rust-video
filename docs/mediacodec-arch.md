# MediaCodec tier architecture

One platform codec, one polled event queue, one pending-input stash per decoder: H.264, H.265, AV1, VP9, and VP8 byte-buffer decode plus H.264, H.265, and AV1 encode behind the caps traits.

## Negotiation

Open reads the live device level and refuses below the floor with `BelowMinSdk`. Codec lookup answers by mime, or `NoDecoder` names the mime. Level 28 and up registers all four async callback slots into one event queue; lower levels use sync dequeue with bounded waits.

## Submit and drain

The submit unit is one packet copied into one input buffer. The completion signal is an output event on the queue, or a dequeue result on the sync path. A failed queue returns typed on the same call: `InputTooLarge` for an overfull packet, `BufferLost` for a vanished buffer, `InputStarved` after three bounded attempts with no input buffer. No failure poisons the decoder; the next call retries fresh. The stash keeps unused input indices across calls; a call holding a stashed index queues without waiting. Decode-only outputs release without a frame on both paths.

Flush drains available outputs and stops there. Reset drains, clears the stash, flushes the codec, and restarts async callbacks.

Encode takes a spec with mime, size, bitrate, and keyframe interval; zero dims or bitrate fail before the codec opens.

## Frames

Every frame copies once from the codec buffer into refcounted bytes; plane splits share that copy without further copies. Layout starts tentative from the spec and negotiates on the first output format. Stride and slice height read from the format, falling back to width and height when absent. A color value outside planar 19 and semi-planar 21 fails typed. No export type and no import path exist.

## Threading

No decoder handle crosses threads. The `&mut self` trait surface serializes each stream on its driving thread. Codec-owned callback threads push events through `Send` senders; the driving thread polls the receiver. No mutex exists in the tier. Waits are bounded receive and dequeue timeouts with no lock held across any of them. Multi-stream runs one decoder per thread.

## Invariants

- `unsafe` appears only in the SDK module, one justified block.
- Async registers all four callback slots; an unset slot faults on Android 12 and earlier.
- Input indices enter the queue once; extras wait in the stash in arrival order.
- A stashed index queues without waiting; an empty stash waits at most three bounded batches.
- Reset clears the stash before flushing the codec.
- API-28 entry points run at 28 and up; the gate is the live device level.
- Open below the floor fails with `BelowMinSdk` naming the level.
- A packet before start fails with `InvalidState` naming the op.
- Split runs only after negotiation; the drain refreshes the layout first.
- Every output range checks against its buffer; overruns fail as `BufferRange`.
- Drop stops the codec and marks the state released.
- Format builders reject zero and oversize dims.
- The quirk parser rejects shape breaks with line numbers.
