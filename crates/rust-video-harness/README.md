# rust-video-harness

Vector suites and golden oracles for end-to-end tests across the tiers, plus the synthetic inputs tier tests build on. It drives backends; it implements none.

```rust
use rust_video_harness::AnnexB;

fn pictures(data: &[u8]) -> Vec<Vec<u8>> {
    AnnexB::group_pictures(data, AnnexB::is_h264_vcl)
}
```

Golden rules live in `../../docs/testing/goldens.md`.
