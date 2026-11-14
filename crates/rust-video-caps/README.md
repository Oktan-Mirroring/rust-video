# rust-video-caps

Shared vocabulary for realtime video in this workspace: frames, packets, backend traits, and codec models with their spec particularities enforced by types. Every tier builds on this crate; it builds on nothing else here.

## Vocabulary

`CpuFrame` carries owned planar bytes and `GpuFrame` is a device-resident frame without CPU access; `Packet` carries coded bytes with timing. `Decoder::decode` borrows a packet and returns owned frames, `Encoder::encode` borrows a CPU frame and returns packets, and `Device` names the execution target. `GpuExport` describes a zero-copy handle: `DmaBuf` on Unix, `NtHandle` on Windows, `Opaque` anywhere. Transcode planning speaks `Plan`, `Route`, and `FallbackCause`: `Route::Gpu` keeps device-resident textures with pixels never mapped, `Route::Cpu` stays caller-built, and `FallbackCause` names the first missed GPU hop so the caller decides the route.

```rust
use rust_video_caps::h264::NalInfo;

fn unit_type(byte: u8) -> u8 {
    NalInfo::validate(byte).unwrap().unit_type
}
```

## Codec models

One module per codec, each with `Params` plus level tables pinned to its spec: H.264 to ITU-T H.264 (01/2012), H.265 to ITU-T H.265 V10 (07/2024), AV1 and AV2 to the AOMedia specifications with AV2 at v1.0.0, VP9 to VP9 v0.7, VP8 to RFC 6386. VP8 has no profiles, levels, or tiles, and its model reflects that. Out of spec values fail with a typed `Violation` at the call site.

## Docs

Ownership and data plane rules live in `../../docs/runtime.md`, backend policy in `../../docs/matrix.md`. The facade re-exports these types from `../rust-video`.
