# rust-video-livepack

Live MPEG-TS and fMP4/CMAF packaging for coded video. Streaming first: bounded memory, no full-file buffering. It depends on no other workspace crate.

## Packaging

`TsMux` packetizes access units into TS bytes and `TsDemux` parses them back; `FragmentMux` writes fragmented MP4 over per-codec track configs. TS carries H.264 and H.265 only: AV1, VP9, VP8, and AV2 have no registered `stream_type` and muxing them fails with `TsMuxError::CodecNotMappable`. `KeyframeGate` holds output until the first keyframe when the caller passes one.

```rust
use rust_video_livepack::{AccessUnit, Codec, TsDemux, TsMux, TsMuxConfig};

fn roundtrip() -> usize {
    let unit = AccessUnit::classified(Codec::H264, vec![0, 0, 0, 1, 0x65], 0, 0);
    let mut mux = TsMux::new(TsMuxConfig::default()).unwrap();
    let mut bytes = Vec::new();
    mux.push(&unit, None, &mut bytes).unwrap();
    let mut demux = TsDemux::new(1 << 20);
    let mut back = Vec::new();
    for piece in bytes.chunks(188) {
        back.extend(demux.push(piece).unwrap());
    }
    back.len()
}
```

## Clocks and queues

One clock everywhere: 90 kHz, tracked by `Ts90k` and `ClockTracker` with `Pcr` carrying the program reference. `BoundedQueue` enforces the memory bound under a caller-chosen `Policy`: `DropOldest` evicts and reports the oldest entry, `Block` refuses a push on a full queue and the caller retries.

## Docs

Container shapes, timestamp discipline, and spec pins live in `../../docs/livepack-ts-fmp4.md`.
