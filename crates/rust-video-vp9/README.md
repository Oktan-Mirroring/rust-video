# rust-video-vp9

VP9 grammar shared by every tier: uncompressed frame headers plus tile assembly. `Ingest` parses frame headers and splits tiles, generic over the tier error type; each `Fault` converts into it through `From`. Software decode and encode ship in `rust-video-sw`.

```rust
use rust_video_vp9::{Fault, Ingest};

fn frames(data: &[u8]) -> usize {
    Ingest::<Fault>::new().ingest_packet(data).unwrap().len()
}
```

Backend policy lives in `../../docs/matrix.md`.
