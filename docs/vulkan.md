# Vulkan tier

Negotiation ladder plus H.264/H.265 decode, IDR encode, and transcode
behind the caps vocabulary.

## Operations

| Op | Negotiates | Runs |
|---|---|---|
| `H264Decode` / `H265Decode` | decode queue, codec ext, caps rung | Annex-B in, NV12 frames out |
| `H264Encode` / `H265Encode` | encode queue, codec ext, caps rung | NV12 in, IDR Annex-B out |

`VulkanDevice::open` returns `Ok(None)` on a declined machine, never an
error; `open_or_skip` names the rung through `SkipReason`.

## Encode

IDR-only: every picture codes without references, so one DPB slot and
no reference lists. The driver emits parameter sets from the session
sets, and the feedback query reports the coded size. CBR rate control
scales with the extent. Inter coding stays out until measured demand.

Every begin carries the CBR control; drivers set rate control state
from the begin chain. Newer validation wants the state preconfigured
through a control command and flags each begin
(`VUID-...-08254`); the control path fails on drivers, so the begin
chain stays.

## Transcode

`Transcoder` pairs a decode device with an encode device and plans in
caps vocabulary (`GpuCaps`, `Plan`, `Route`, `FallbackCause`). The
route stays on the GPU when every hop holds on one adapter: decode
exports DMA-BUF planes, the encoder imports them, pixels never map.
Otherwise the CPU path runs with the first miss named loud:
`ShareUnsupported` without an external-memory mechanism, else
`NoSharedExport`, `EncoderCpuOnly`, or `DeviceMismatch` across
adapters. Scale and convert hops fail loud the same way.

Pairing compares adapter fingerprints (vendor, device, LUID hashed),
stable across instances; raw physical-device handles differ per
instance and compared unequal on the same GPU.

## Frames

`VulkanFrame` holds NV12 bytes on the CPU path and a device export
on the GPU path, where `export()` returns DMA-BUF planes on Unix
and `nv12` is empty; without a DMA-BUF export it is
`Opaque { name: "vulkan" }`. `VulkanDevice::share_mechanism` reports
the platform mechanism (`DmaBuf` on Linux, `NtHandle` on Windows)
when the adapter exposes external-memory extensions. A surface frame
keeps its device alive until dropped.

## Performance notes

- One command buffer and fence per session, reset per submit.
- Bitstream upload maps the access unit zero-padded through the
  aligned range, readback maps the coded length, never the
  allocation size.
- The sink reuses its coded staging and crops into pooled buffers.
- Exportable planes pool per size; idle sizes drop when a new one arrives.
- The encoder reuses one DMA-BUF import per export pair.
- Readback maps into reused vectors without a zero-fill first.
- The NAL splitter drains consumed bytes once per push.
- The sync `Decoder`/`Encoder` traits serialize submits; overlap
  needs an async submit shape, not a deeper pool.

## Threading and teardown

- `VulkanDevice` is `Send` plus `Sync`; every submit holds the queue
  mutex shared with its device. Decoders and encoders are thread-local
  parser state plus one session.
- Session fields drop parameters before the session they belong to,
  and views before their images. A poisoned queue mutex surfaces as
  `QueuePoisoned`, never a hang.
- A failed submit wedges its session; later submits fail without
  touching the queue, since the fence may still be submitted.
- Crop bounds, short planes, and short readbacks return `Err`
  (`OverCaps`, `ShortReadback`); no reachable `expect` remains.

## Validation notes

Decode submits zero VUIDs; encode carries only the rate-control
skew above plus a bogus per-encode `07012 uninitialized`. H.264
decode under `VK_LAYER_KHRONOS_validation` 1.4.309 segfaults at the
first submit: valgrind pins a layer-owned use-after-free (its H.264
tracker frees record-time copies it later hands to the driver).
H.265 is clean, plain mode is clean, goldens pass; NVIDIA's samples
document the same family of layer DPB-tracking defects. Re-test
past 1.4.309 before trusting H.264 validation again.

RADV is untested beyond negotiation: its VCN3 queue advertises
encode but session creation fails, and decode rejects the combined
DST/DPB/transfer image usage. A RADV-capable shape needs separate
images; parked until demand.

## Examples

- `decode_h264_vk` / `decode_h265_vk`: `VK_VECTOR` in, planar out.
- `encode_h264_vk` / `encode_h265_vk`: raw NV12 in, Annex-B out.
- `transcode_gpu_vk`: coded in, coded out, plan on stderr.
- `decode_matrix_vk`: op matrix plus transcode plan probe.
- `vkprobe`: `VKPROBE_CODEC=h264|h265` decode probe.

Every example skips loud without hardware or input.
