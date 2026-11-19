# rust-video-h264

H.264 grammar: sets, slices, and picture description. One parser behind every tier, generic over the tier error; tier caps arrive as gates. Software decode and encode ship in `rust-video-sw`.

```rust
use rust_video_h264::split_nals;

fn units(data: &[u8]) -> usize {
    split_nals(data).len()
}
```

Backend policy lives in `../../docs/matrix.md`.
