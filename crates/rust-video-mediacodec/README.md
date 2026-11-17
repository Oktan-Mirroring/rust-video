# rust-video-mediacodec

Android MediaCodec tier: H.264, H.265, AV1, VP9, and VP8 byte-buffer decode plus H.264, H.265, and AV1 byte-buffer encode behind the caps traits. Off Android the native handles compile out and the pure pieces stand alone.

## Decode

`MediaCodecDecoder::open` takes a `FormatSpec` and refuses below the device floor with `BelowMinSdk`. Lookup answers by mime or fails with `NoDecoder` naming it. API 28 and up register all four async callback slots into one `EventQueue`; lower levels use sync dequeue with bounded waits. Each queued packet copies once into an input buffer and each output drains into refcounted bytes behind a `MediaCodecSurface`; `export` answers `GpuExport::Opaque` and there is no import path.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_mediacodec::{FormatSpec, MediaCodecDecoder, MediaCodecError};

fn run(packet: &Packet) -> Result<usize, MediaCodecError> {
    let spec = FormatSpec::h264(1920, 1080).expect("dims");
    let mut decoder = MediaCodecDecoder::open(&spec)?;
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode and gates

`MediaCodecEncoder::open` takes an `EncodeSpec` with width, height, bitrate, frame rate, and keyframe interval; `encode` queues one `CpuFrame` and drains packets, `flush` drains without new input. Device floor is API 21, async callbacks need API 28, AV1 needs API 29. A color value outside planar 19 and semi-planar 21 fails typed as `UnsupportedColorFormat`. The snippet above is Android-only: `open` is gated on the target OS.

## Docs

The binding architecture is in `../../docs/mediacodec-arch.md` with the ops companion in `../../docs/mediacodec.md`. Probe the device matrix with `cargo run -p rust-video-mediacodec --example decode_matrix_mc`.
