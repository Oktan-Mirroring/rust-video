# rust-video-livepack

Live MPEG-TS and fMP4/CMAF packaging for coded video. Streaming first: bounded memory, no full-file buffering. It depends on no other workspace crate.

`TsMux` packetizes access units into TS bytes and `TsDemux` parses them back; `FragmentMux` writes fragmented MP4 over per-codec track configs. TS carries H.264 and H.265 only: any other codec muxes fail with `TsMuxError::CodecNotMappable`.

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

Container shapes and timestamp discipline live in `../../docs/livepack-ts-fmp4.md`.
