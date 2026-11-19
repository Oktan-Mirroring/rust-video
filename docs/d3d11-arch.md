# D3D11 tier

Profile probe plus H.264, HEVC, AV1, and VP9 decode, MFT encode, and transcode behind the caps vocabulary.

## Operations

| Op | Negotiates | Runs |
|---|---|---|
| `H264Decode` | H.264 profile, NV12 target, session config | Annex-B in, NV12 frames out |
| `H264Encode` / `HevcEncode` | encoder MFT, D3D11 bind | NV12 in, coded packets out |
| Shell decoders | profile probe only | loud error, empty flush |

`D3d11Device::open` declines a machine it cannot run on through `D3d11Error::Skipped`, naming the cause; examples print `SKIP` and exit.

## Encode

Encode runs over Media Foundation encoder MFTs. Sessions code 30 fps progressive NV12 with an area-scaled bitrate, width times height times 3 clamped to 100_000..=50_000_000.

CPU input travels as one aligned MF sample stamped with pts; GPU input copies the decode-slice region into the cached MFT input texture under the submit lock. Completion drains `ProcessOutput` until `NEED_MORE_INPUT`; a size change drains and reconfigures.

Asynchronous MFTs run on the callback model: the pump holds one outstanding `BeginGetEvent` per consumed event, and the callback only queues the event type and re-arms. GPU work stays on the submitter thread. A submit failure flushes the transform and resets the pump instead of wedging the session.

## Transcode

The route stays on the GPU when every hop holds on one device: `NtHandle` share plus decode export plus encoder import, with surfaces flowing through the GPU encode entry points, pixels staying on the GPU. Otherwise `transcode` and `flush` decline with `Unsupported` naming the first miss. One device serves both ends.

## Frames

`D3d11Frame` holds NV12 bytes with identity and a budget lease; `nv12()` exposes the bytes and `to_cpu()` splits NV12 planes. `export()` returns the NT handle over the shared decode texture, or `Opaque { name: "d3d11" }` when the driver declines sharing. The GPU surface stays device-resident and carries the two-plane descriptor behind the NT handle, sourcing the scale and encode regions.

## Submit and threading

One decode session per stream: 16 reference views plus 4 spare over one array texture, one copy lane per view; readback copies the visible rectangle only. One submit per picture: picture parameters, quantization matrix, bitstream, and slice control inside one begin/end frame pair. Decode runs submit-then-readback: every picture submits and copies first, then every queued output reads back. Bitstream bytes past the driver allocation fail as `BitstreamTooLarge`.

The immediate context stays the single submitter behind one device-level lock; completion waits, maps, and CPU copies run outside it. The API stays synchronous per handle; independent sessions submit concurrently through the lock and wait outside it. A failed picture resets the stream: DPB, session, shared export, and active sequence drop together, and the next picture reopens clean.

## Validation

DXVA H.264 structs carry compile-time size and offset asserts: 1040/224/864 bytes with the current picture at 4, the reference list at 16, and the order counts at 40. Hardware decode matches the ffmpeg software oracle byte-exact over the committed vector: 15 frames, 1,728,000 NV12 bytes, 0 mismatches. The e2e suite drives the committed vector through overlap, GPU transcode verified by the oracle, failure reset plus recovery, and a zero-reset steady run.

## Examples

- `decode_h264_d3d11`: synthetic Annex-B in, frames out.
- `encode_h264` / `encode_hevc`: synthetic NV12 in, packets out.
- `transcode_gpu_d3d11`: vector in over the GPU route, bitstream out for the ffmpeg oracle.

Every example skips loud without hardware.
