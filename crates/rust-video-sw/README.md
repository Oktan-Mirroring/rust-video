# rust-video-sw

CPU encode and decode for H.264, H.265, AV1, VP9, and VP8. Features select the backend per direction: `ffmpeg` decodes all five codecs and encodes AV1, VP9, and VP8 through the system libavcodec; `h264-openh264` encodes H.264; `gpl` encodes H.265 through libx265 and implies `ffmpeg`.

## Usage

```rust
use rust_video::Decoder;
use rust_video::Packet;
use rust_video_sw::H264Decoder;

fn run(packet: &Packet) -> usize {
    let mut decoder = H264Decoder::open(0).unwrap();
    decoder.decode(packet).unwrap().len() + decoder.flush().unwrap().len()
}
```

## Requirements

Rust 1.87 or later. The `ffmpeg` feature links the system libavcodec and libavutil; on Windows, `FFMPEG_LIB_DIR` points at `avcodec.lib` and `avutil.lib`.

Software codec notes live in `../../docs/sw-codecs.md`.
