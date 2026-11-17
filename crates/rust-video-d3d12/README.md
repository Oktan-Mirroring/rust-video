# rust-video-d3d12

Direct3D 12 tier for Windows: H.264, HEVC, and AV1 decode, video encoder encode, and GPU transcode behind the caps traits.

## Decode

`D3d12Device::open` probes the adapter and feature level, and declines machines it cannot run on with `D3d12Error::Skipped`, naming the cause in `SkipReason`; the runners print `SKIP` and exit. Decode needs tier 2 or later; tier 1 fails loud with `DecodeTier1`. `H264Decoder` carries a full decoded picture buffer, `HevcDecoder` an HEVC picture buffer with reorder output, and `Av1Decoder` AV1 reference slots with show-existing display. `Mpeg2Decoder`, `Vc1Decoder`, and `Vp9Decoder` are shell decoders: profile probe only, a loud typed error per packet, empty flush.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d12::{D3d12Device, D3d12Error, H264Decoder};

fn run(packet: &Packet) -> Result<usize, D3d12Error> {
    let device = D3d12Device::open()?;
    let mut decoder = H264Decoder::open(&device);
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode and transcode

`D3d12Encoder` drives video encoder sessions coding intra-only IDR at CQP 26. Sessions build for H.264 and HEVC; any other codec fails with `EncodeArgsPending`. Flush returns empty; a size change reopens the session. `Transcoder` decodes H.264 and HEVC with textures flowing device-resident and pixels never mapped where one device holds both ends, and declines with `Unsupported` naming the first miss otherwise. The route never spans adapters. Frames carry NV12 bytes with an NT handle export and a budget `Lease` released on drop, or `Opaque` where the driver declines sharing.

## Measurements

`../../docs/d3d12-arch.md` records 112.7 fps measured 2026-11-13 on Windows 11 Pro N build 26200, Ryzen 9 9950X3D, RTX 5080 on driver 32.0.16.2114, debug build, via `cargo test -p rust-video-d3d12 --test throughput`: 30 1080p GPU hops against the 60 fps bar.

## Docs

The binding architecture is in `../../docs/d3d12-arch.md`. Decode one H.264 stream with `cargo run -p rust-video-d3d12 --example decode_h264_d3d12`; the runner skips loud without hardware.
