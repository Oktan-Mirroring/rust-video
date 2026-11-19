# D3D12 tier

Feature-level probe plus H.264, HEVC, and AV1 decode, video-encoder encode, and transcode behind the caps vocabulary.

## Operations

| Op | Negotiates | Runs |
|---|---|---|
| `H264Decode` | H.264 profile, NV12 target, decode tier 2 | Annex-B in, NV12 frames out |
| `HevcDecode` | HEVC profile, NV12 target, decode tier 2 | Annex-B in, NV12 frames out |
| `H264Encode` / `HevcEncode` | video-encoder codec, CQP session | NV12 in, IDR Annex-B out |
| Shell decoders | profile probe only | loud error, empty flush |

`D3d12Device::open` declines a machine it cannot run on through `D3d12Error::Skipped`, naming the cause; examples print `SKIP` and exit. Decode needs tier 2 or later; tier 1 fails loud.

## Encode

Sessions code intra-only IDR at CQP 26, high profile for H.264 and main for HEVC, with the level drawn from the macroblock and luma tables. CPU input travels through the upload buffer and direct queue into the input texture; GPU input copies the source texture instead. Completion encodes the frame, resolves the metadata, and reads the coded byte count; a driver length outside the buffer fails loud. A size change reopens the session.

## Transcode

The route stays on the GPU when every hop holds on one device: `NtHandle` share plus decode export plus encoder import, pixels staying on the GPU. Otherwise `transcode` and `flush` decline with `Unsupported` naming the first miss. One device serves both ends.

## Frames

`D3d12Frame` holds NV12 bytes with identity and a budget lease; `nv12()` exposes the bytes and `to_cpu()` splits NV12 planes. `export()` returns the NT handle over the shared pool texture, or `Opaque { name: "d3d12" }` when the stream is cropped or the driver declines sharing; the handle is owned per frame and closed on drop.

## Submit and threading

One decode session per stream: 16 shared NV12 pool textures plus 16 crop targets when cropped, one 8 MB upload bitstream, one readback staging per slot. One submit per picture: `DecodeFrame` on the video-decode queue, download on the direct queue. Decode runs submit-then-download: each call submits every picture first, then every queued output starts its copy before the first wait. A busy ticket guards every slot: the next video submit waits the in-flight ticket, and a slot re-records only past its own copy ticket. One fence per queue; the wait parks on a fence event with a 5 s budget and a miss fails as `FenceWait`.

The API stays synchronous per handle with no spawned threads and no shared lock; sessions take `&mut self` and fence waits park on the caller thread. A failed picture resets the stream: DPB, session, and active sequence drop together.

## Validation

DXVA structs carry compile-time size and offset asserts: H.264 1040/224/12 bytes and HEVC 232/1000/12, with the current picture at 4 and 6, the reference lists at 16 and 124, and the order counts at 40 and 140. The e2e suite drives the committed 15-picture vectors through overlap, GPU transcode verified by the oracle, failure reset plus recovery, and a zero-reset steady run.

## Examples

- `decode_h264_d3d12` / `decode_hevc_d3d12`: synthetic Annex-B in, frames out.
- `encode_h264_d3d12` / `encode_hevc_d3d12`: synthetic NV12 in, packets out.
- `transcode_gpu_d3d12`: vector in over the GPU route, bitstream out for the ffmpeg oracle.

Every example skips loud without hardware.
