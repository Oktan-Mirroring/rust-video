# rust-video-sw

Software CPU codec backends behind the facade: five decoders over one hand-written libavcodec sys layer, five CPU encoders. Default features expose no backend; each feature below wires real codecs.

## Decode

The `ffmpeg` feature opens `H264Decoder`, `H265Decoder`, `Vp8Decoder`, `Vp9Decoder`, and `Av1Decoder` over the system libavcodec. Every `open(threads)` fails closed with `SwError::Init` unless the linked libavcodec reports major 61 and libavutil reports major 59, so a drifted ABI never decodes silently. AV1 selects the `libdav1d` decoder by name because id lookup returns whichever AV1 decoder registered first and the true native decoder is hardware only. Decoders emit 8-bit 4:2:0 only; anything else fails with `SwError::BadFrame`.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_sw::{H264Decoder, SwError};

fn run(packet: &Packet) -> Result<usize, SwError> {
    let mut decoder = H264Decoder::open(4)?;
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode

`H264Encoder::open` takes width, height, bitrate in bps, and a `u16` thread count over vendored openh264 0.9.8, behind the `h264-openh264` feature. The ffmpeg encoders take width, height, bitrate in kbps, and a `u32` thread count: `Vp8Encoder` and `Vp9Encoder` over the `libvpx` wrappers, `Av1Encoder` over `libaom-av1`, all behind `ffmpeg`. `H265Encoder` rides `libx265` behind `gpl`, which implies `ffmpeg` and links the GPL path since libx265 is GPL-2+. No crate here binds dav1d or libvpx directly; both arrive through libavcodec wrappers. On Windows the build reads `FFMPEG_LIB_DIR` for `avcodec.lib` and `avutil.lib`.

## Measurements

`../../docs/sw-codecs.md` records Tier 1 figures measured 2026-10-11 on AMD Ryzen 9 9950X3D, 32 threads, 60 GB RAM, release profile, ffmpeg 7.1.5, dav1d 1.5.1, libvpx 1.15.0, through the probe runner:

| Codec | 1080p throughput |
|---|---|
| H.264 decode | 588 fps, 4 threads |
| H.265 decode | 563 fps, 4 threads; 361 fps, 1 thread |
| VP9 decode | 625 fps, 4 threads; 325 fps, 1 thread |
| VP8 decode | 1336 fps, 4 threads; 444 fps, 1 thread |
| AV1 decode | 604 fps, 4 threads; about 347 fps, 1 thread |
| H.264 encode | 272 fps, 4 threads |
| VP9 encode | 368 fps, 4 threads |

VP8, AV1, and H.265 encode throughput is unmeasured; encode probe modes exist for all three (`vp8enc`, `av1enc`, `x265enc`). Exactness is `cmp` of dumped raw frames against the ffmpeg oracle: H.264 60/60, H.265 120/120 at both thread counts, VP9 2000/2000 at both, VP8 1000/1000 at both, AV1 60/60 on three vectors including film grain. Encoder outputs check with `ffprobe -count_frames`: 120/120 each at 1920x1080. Reproduce any row with `cargo run -p rust-video-sw --example probe -- ffdec h264 <in>`.

## Docs

Codec notes, the Windows vcpkg recipe, and the ABI guard boundary live in `../../docs/sw-codecs.md`; backend policy is in `../../docs/matrix.md`.
