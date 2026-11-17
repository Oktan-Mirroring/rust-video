# rust-video-d3d11

Direct3D 11 tier for Windows: H.264, H.265, VP9, and AV1 decode, Media Foundation encode, and GPU transcode behind the caps traits.

## Decode

`D3d11Device::open` probes the adapter and declines machines it cannot run on with `D3d11Error::Skipped`, naming the cause in `SkipReason`; the runners print `SKIP` and exit. `H264Decoder`, `HevcDecoder`, `Vp9Decoder`, and `Av1Decoder` take Annex B, IVF, or OBU bytes in and return NV12 frames. `Mpeg2Decoder` and `Vc1Decoder` are shell decoders: profile probe only, a loud typed error per packet, empty flush.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_d3d11::{D3d11Device, D3d11Error, H264Decoder};

fn run(packet: &Packet) -> Result<usize, D3d11Error> {
    let device = D3d11Device::open()?;
    let mut decoder = H264Decoder::open(&device);
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode and transcode

`D3d11Encoder` drives Media Foundation encoder MFTs, coding 30 fps NV12 with a bitrate of width times height times 3 clamped to 100_000..=50_000_000. Format coverage probes per machine across H.264, HEVC, VP9, and AV1 subtypes. `Transcoder` decodes H.264 only and refuses any other decode codec at open; the route stays on the GPU with pixels never mapped where one device holds the decode export and the encoder import, and `transcode` plus `flush` decline with `Unsupported` naming the first miss otherwise. The route never spans adapters. Frames carry NV12 bytes with an NT handle export and a budget `Lease` released on drop.

## Measurements

`../../docs/d3d11-arch.md` records hardware decode matching the ffmpeg software oracle byte-exact over the committed vector: 15 frames, 1,728,000 NV12 bytes, 0 mismatches. The same notes record 519.8 fps measured 2026-11-13 on Windows 11 Pro N build 26200, Ryzen 9 9950X3D, RTX 5080 on driver 32.0.16.2114, debug build, via `cargo test -p rust-video-d3d11 --test throughput`: 30 1080p GPU hops against the 60 fps bar.

## Docs

The binding architecture is in `../../docs/d3d11-arch.md`. Decode one H.264 stream with `cargo run -p rust-video-d3d11 --example decode_h264_d3d11`; the runner skips loud without hardware.
