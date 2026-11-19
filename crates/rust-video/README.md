# rust-video

Safe Rust interface over realtime video encode and decode. Applications name a codec and a tier preference, and the facade resolves which backend runs the stream.

`TierPreference::resolve` takes a `VideoCodec`, caller-supplied `Capabilities`, and a `Platform`, and returns the `Tier` that runs the stream. `Auto` walks the platform order and returns the first tier the capabilities support; `Force` takes one tier or fails with `TierError`.

```rust
use rust_video::{Capabilities, Platform, Tier, TierPreference, VideoCodec};

fn pick() -> Result<Tier, rust_video::TierError> {
    let caps = Capabilities::none().with_vulkan().with_software();
    TierPreference::Auto.resolve(VideoCodec::H264, &caps, Platform::host())
}
```

Windows tries Vulkan, D3D11, D3D12, then Software. Linux tries Vulkan, VA-API, then Software. Android tries Vulkan, MediaCodec, then Software. Any other OS tries Vulkan, then Software. Default features carry the three portable tiers: `vulkan`, `vaapi`, `mediacodec`.

Tier rules live in `../../docs/architecture.md`.
