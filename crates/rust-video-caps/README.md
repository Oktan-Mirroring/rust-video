# rust-video-caps

Shared vocabulary for realtime video: frames, packets, backend traits, and codec models with spec particularities enforced by types. The workspace leaf: every tier builds on this crate.

`CpuFrame` carries owned planar bytes and `GpuFrame` is a device-resident frame without CPU access; `Packet` carries coded bytes with timing. `Decoder::decode` borrows a packet and returns owned frames, `Encoder::encode` borrows a CPU frame and returns packets. `GpuExport` is `DmaBuf` on Unix, `NtHandle` on Windows, `Opaque` anywhere. One module per codec carries `Params` plus level tables pinned to its spec; out-of-spec values fail with a typed `Violation` at the call site.

```rust
use rust_video_caps::h264::NalInfo;

fn unit_type(byte: u8) -> u8 {
    NalInfo::validate(byte).unwrap().unit_type
}
```

Ownership and data-plane rules live in `../../docs/runtime.md`.
