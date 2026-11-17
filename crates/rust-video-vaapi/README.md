# rust-video-vaapi

VA-API tier for Linux: H.264, H.265, and AV1 decode, all-intra H.264 and H.265 encode, and GPU transcode behind the caps traits. Off Linux the public API vanishes while the portable syntax modules still compile; only the OS modules are Linux-gated at the root.

## Decode

`VaApiDevice::open` sorts the render nodes under `/dev/dri` and keeps the first display that initializes with H.264 VLD into NV12. Each decoder submits one begun picture at a time through a four-deep `BufferSet` ring and syncs at consumption with `vaSyncSurface`; a failed submit quiets its target and returns the error on that call with the context still usable. `decode` copies pixels and releases its slot at once, `decode_surface` leases its slot until the frame drops. `Vp8Decoder` and `Vp9Decoder` are staged handles: every packet declines with typed `Unsupported` until their frontends wire.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vaapi::{H264Decoder, VaApiDevice, VaApiError};

fn run(packet: &Packet) -> Result<usize, VaApiError> {
    let device = VaApiDevice::open()?;
    let mut decoder = H264Decoder::open(&device);
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode and transcode

`VaApiEncoder` codes every picture as an IDR at CQP 26, H.264 High profile or H.265 Main, with packed headers. `Transcoder` runs `decode_surface` into the encoder from H.264 sources, with an on-device scale hop on a size miss and the encoder opening lazily on first frame size. `export` syncs the surface and yields two NV12 DMA-BUF planes; import checks same display then session size, with no format check in the path.

## Proof

`../../docs/vaapi.md` records a 60/60 byte-exact decode of `corpus/synth/h264/h264-high.mp4` over `/dev/dri/renderD128` (Mesa radeonsi, `VAProfileH264High` plus VLD plus NV12) against an independent ffmpeg software decode, YUV420P planar SHA-256 per frame. H.265 and AV1 hardware fps is pending Linux runs. Syntax sources are assimilated from `cros-codecs` 0.0.6 with provenance in `../../docs/vaapi.md`; there is no `cros-codecs` dependency.

## Docs

The binding architecture is in `../../docs/vaapi-arch.md` with the ops companion in `../../docs/vaapi.md`. Decode one H.264 stream with `cargo run -p rust-video-vaapi --example decode_h264_vaapi`; the runner skips loud without hardware.
