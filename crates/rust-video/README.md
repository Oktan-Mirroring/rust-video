# rust-video

Safe Rust interface over realtime video encode and decode. Applications name a codec and a tier preference, and the facade resolves which backend runs the stream.

## Tier selection

`TierPreference::resolve` takes a `VideoCodec`, caller-supplied `Capabilities`, and a `Platform`, and returns the `Tier` that runs the stream. `Auto` walks the platform order and returns the first tier the capabilities support; `Force` takes one tier or fails with `TierError`.

```rust
use rust_video::{Capabilities, Platform, Tier, TierPreference, VideoCodec};

fn pick() -> Result<Tier, rust_video::TierError> {
    let caps = Capabilities::none().with_vulkan().with_software();
    TierPreference::Auto.resolve(VideoCodec::H264, &caps, Platform::host())
}
```

Each platform probes in a fixed order. Windows tries Vulkan, D3D11, D3D12, then Software. Linux tries Vulkan, VA-API, then Software. Android tries Vulkan, MediaCodec, then Software. Any other OS tries Vulkan, then Software. Software H.265 resolves only when the caller adds `with_h265_software`; without it the software tier still resolves the other codecs.

## Features

Default carries the three portable tiers: `vulkan`, `vaapi`, `mediacodec`. The codec features `h264`, `h265`, `av1`, `av2`, and `vp9` each wire one namespace crate, and `d3d11` plus `d3d12` wire the Windows tiers. There is no `vp8`, `sw`, or `wgpu-interop` feature. The build script derives one availability cfg per OS-gated tier from feature plus target, and `have_corpus` when the corpus directory exists.

## Docs

Tier rules live in `../../docs/architecture.md`, backend policy in `../../docs/matrix.md`, and packaging plus platform detail in `../../docs/compat.md`. The facade re-exports the shared vocabulary from `../rust-video-caps`.
