# rust-video-core

Shared backend mechanics behind the video tiers: codec identities, plane checks and copies, a worker for split-phase readback copies, and duration reads. Tier authors use these for buffer plumbing shared across backends.

## Usage

```rust
use rust_video_core::{check_plane, micros};
use std::time::Duration;

fn describe(data: &[u8]) -> (u64, bool) {
    (micros(Duration::from_millis(5)), check_plane(data, 64, 64, 1).is_ok())
}
```

## Requirements

Rust 1.87 or later, with the standard library alone.

Shared-shape notes live in `../../docs/architecture.md`.
