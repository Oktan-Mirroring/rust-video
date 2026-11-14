# rust-video-harness

Vector suites and golden oracles for end-to-end tests across the tiers, plus the synthetic inputs tier tests build on. It drives backends; it implements none.

`Suite` and `Case` describe a corpus run, `Golden` pins expected hashes with `Mismatch` reports on drift, `Oracle` runs the ffmpeg reference, and `FrameHash` hashes decoded output. `CountingAlloc` with `assert_no_allocs_after_warmup` backs the zero-copy tests. The synthetic side splits Annex B into picture groups through `AnnexB`, builds H.264 vectors with `H264Synth` and NV12 frames with `Synth`, and reads IVF and MP4 through `Ivf` and `Stream`.

```rust
use rust_video_harness::AnnexB;

fn pictures(data: &[u8]) -> Vec<Vec<u8>> {
    AnnexB::group_pictures(data, AnnexB::is_h264_vcl)
}
```

## Docs

Vector rules live in `../../docs/testing/vectors.md`, golden rules in `../../docs/testing/goldens.md`, and the test layout in `../../docs/testing/README.md`.
