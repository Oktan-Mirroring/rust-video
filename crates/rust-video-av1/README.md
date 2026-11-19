# rust-video-av1

AV1 grammar shared by every decode tier: OBU splitting, sequence and frame headers, and tile assembly, generic over the tier error type. Software decode and encode ship in `rust-video-sw`.

```rust
use rust_video_av1::{split_obus, Error};

fn units(data: &[u8]) -> usize {
    split_obus::<Error>(data).unwrap().len()
}
```

Backend policy lives in `../../docs/matrix.md`.
