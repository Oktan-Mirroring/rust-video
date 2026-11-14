# rust-video-vulkan

Vulkan video tier: H.264, H.265, and AV1 decode, H.264 and H.265 GOP encode, and GPU transcode behind the caps traits. One negotiated device per op, one video queue, one three-slot submit ring per codec object.

## Decode

`VulkanDevice::open` negotiates a device for one `CodecOp` and returns `None` where the machine declines instead of erroring; `open_or_skip` keeps the `SkipReason`. The five ops are `H264Decode`, `H265Decode`, `Av1Decode`, `H264Encode`, and `H265Encode`. Each decoder parses access units, submits one recorded command buffer per picture through its three-slot ring, and emits frames in display order out of a reorder buffer. Flush drains the splitter, pending units, then the reorder buffer. No fence wait exceeds five seconds.

```rust
use rust_video_caps::packet::Packet;
use rust_video_caps::traits::Decoder;
use rust_video_vulkan::{CodecOp, H264Decoder, VulkanDevice, VulkanError};

fn run(packet: &Packet) -> Result<usize, VulkanError> {
    let Some(device) = VulkanDevice::open(CodecOp::H264Decode)? else {
        return Ok(0);
    };
    let mut decoder = H264Decoder::open(&device)?;
    let mut total = decoder.decode(packet)?.len();
    total += decoder.flush()?.len();
    Ok(total)
}
```

## Encode and transcode

`H264Encoder` and `H265Encoder` open on width and height with a `Gop` cadence: an IDR every 30 pictures by default, all-intra available. Packets code keyframes on IDR pictures. `Transcoder` pairs one decoder with one encoder under a caps `Plan`, routing decode export to encoder import from H.264, H.265, or AV1 sources with the encoder opening on the first frame. GPU decode lands in leased NV12 images exported as DMA-BUF planes on Unix; `share_mechanism` reports `DmaBuf` where the adapter enables external memory FD and `None` otherwise.

## Docs

The binding architecture is in `../../docs/vulkan-arch.md` with the ops companion in `../../docs/vulkan.md`. Decode one H.264 stream with `cargo run -p rust-video-vulkan --example decode_h264_vk`; the runner skips loud without hardware.
