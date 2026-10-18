# D3D12 tier

Feature-level probe plus H.264 and HEVC decode, video-encoder
encode, and transcode behind the caps vocabulary.

## Operations

| Op | Negotiates | Runs |
|---|---|---|
| `H264Decode` | H.264 profile, NV12 target, decode tier 2 | Annex-B in, NV12 frames out |
| `HevcDecode` | HEVC profile, NV12 target, decode tier 2 | IDR Annex-B in, NV12 frames out |
| `H264Encode` / `HevcEncode` | video-encoder codec, CQP session | NV12 in, IDR Annex-B out |
| `ShellDecode` x4 | profile probe only | loud error, empty flush |

`D3d12Device::open` declines a machine it cannot run on through
`D3d12Error::Skipped`, naming the cause in `SkipReason`
(`device.rs`, `error.rs`); examples print `SKIP` and exit.

## Encode

Encode runs over D3D12 video-encoder sessions: `D3d12Encoder`
packs strided NV12 contiguous and drives `EncodeSession`
(`encoder.rs`, `encode.rs`). Sessions code intra-only IDR at
CQP 26, high profile for H.264 and main for HEVC, with the
level drawn from the macroblock and luma tables; extents past
5.2 and 6.2 fail loud (`encode.rs`).

CPU input travels through the upload buffer and direct queue
into the input texture; GPU input copies the source texture
instead (`encode.rs`). Completion encodes the frame, resolves
the metadata, and reads the coded byte count; a driver length
outside the buffer fails loud. Flush returns empty; a size
change reopens the session (`encoder.rs`, `encode.rs`).

## Transcode

`Transcoder` pairs an H.264 or HEVC decoder with a video
encoder and plans in caps vocabulary (`GpuCaps`, `Plan`,
`Route`, `FallbackCause`) (`transcode.rs`, caps
`transcode.rs`). The route stays on the GPU when every hop
holds on one device: `NtHandle` share plus decode export
plus encoder import, with textures flowing through
`decode_texture` and `encode_texture`, pixels never mapped.
Otherwise the CPU path runs with the first miss named loud:
`ShareUnsupported` without a share mechanism, else
`NoSharedExport` or `EncoderCpuOnly`. Scale and convert hops
fail loud the same way; HEVC GPU decode rejects
multi-picture packets loud (`transcode.rs`, `decoder.rs`).

The route never spans adapters: one device serves both ends,
and `Plan::for_devices` has no call site in the tier
(`transcode.rs`).

## Frames

`D3d12Frame` holds NV12 bytes with identity and a budget
lease; `nv12()` exposes the bytes and `to_cpu()` splits NV12
planes (`decoder.rs`). `export()` returns the NT handle over
the shared pool texture, or `Opaque { name: "d3d12" }` when
the stream is cropped or the driver declines sharing; the
handle is owned per frame and closed on drop (`decoder.rs`,
`decode.rs`). `GpuTexture` stays device-resident without CPU
access (`texture.rs`). Each frame holds a `Lease` against
the `DedicatedVideoMemory` budget, released on drop
(`device.rs`, caps `traits.rs`).

## Performance notes

- One decode session per stream: 16 shared NV12 pool textures
  plus 16 crop targets when cropped, one 8 MB upload bitstream,
  one readback staging per slot; download tightens pitch and
  crops to the visible rectangle (`decode.rs`, `submit.rs`).
- One submit per picture: `DecodeFrame` on the video-decode
  queue, download on the direct queue; slices pack
  start-coded and pad to 128 bytes (`decode.rs`).
- Decode runs submit-then-download: each call submits every picture
  first, then every queued output starts its copy before the
  first wait, so back-to-back downloads never stall on a
  fence (`decoder.rs`, `decode.rs`).
- Each pool slot records on its own allocator and list, and
  each queue signals its own fence; a video wait never
  serializes behind copy values (`decode.rs`, `encode.rs`,
  `submit.rs`).
- Bitstream bytes past the driver allocation fail as
  `BitstreamTooLarge`; nothing truncates (`decode.rs`,
  `encode.rs`).
- CPU encode packs strided NV12 contiguous once per frame,
  then stages through the upload buffer (`encoder.rs`,
  `encode.rs`).
- The encoder sizes one bitstream staging per session at
  pixels times 3 plus 64 KiB, 64 KiB aligned, and reads the
  coded length from resolved metadata (`encode.rs`).
- The DPB holds 16 slots; each submission carries picture
  parameters, quantization matrix, slices, and bitstream
  (`dpb.rs`, `decode.rs`).
- One fence per queue: `ExecuteCommandLists` plus signal
  returns a ticket, and the wait parks on a fence event with a
  5 s budget; a miss fails as `FenceWait`, never hangs
  (`submit.rs`).

## Threading and teardown

- The API stays synchronous per handle with no spawned threads
  and no shared lock; sessions take `&mut self` and fence
  waits park on the caller thread. Independent sessions submit
  concurrently over their own queues and fences (`submit.rs`).
- A busy ticket guards every slot: the next video submit waits
  the in-flight video ticket, and a slot re-records only past
  its own copy ticket, so no reset lands on in-flight work
  (`decode.rs`).
- A failed picture resets the stream: DPB, session, and
  active sequence drop together; a failed encode drops its
  session and a failed scale drops the scaler. A failed close
  or cast resets its slot pair first, so a retry starts clean
  instead of wedging on a poisoned list (`decoder.rs`,
  `encoder.rs`, `transcode.rs`, `submit.rs`).
- Over-budget frames fail as `OverBudget`, oversized streams
  as `OverCaps`, short CPU splits return `None`; no reachable
  `expect` remains (`error.rs`, `decoder.rs`).

## Validation notes

DXVA structs carry compile-time size and offset asserts:
H.264 1040/224/12 bytes and HEVC 232/1000/12, with the
current picture at 4 and 6, the reference lists at 16 and
124, and the order counts at 40 and 140 (`dxva.rs`). Decode
needs tier 2 or later; tier 1 fails loud (`device.rs`).

Emitted H.264 parameter sets match the encoder: High
profile CABAC without 8x8 transform signaling, and the
ffmpeg oracle decodes the transcode output warning-free
(`encode.rs`, `encoder.rs`, `tests/e2e.rs`). HEVC encode on
boxes reporting the codec but refusing its configuration
fails the first frame loud with `OverCaps` (`encoder.rs`).

Shell decoders report `ProfileUnsupported` without a profile
and the pending picture builder with one; flush returns
empty (`decoder.rs`).

`tests/e2e.rs` drives the committed 15-picture vector through
overlap (`max_inflight` above 1), GPU transcode verified
clean by the ffmpeg oracle, failure reset plus recovery,
and a zero-reset steady run; `H264Decoder::resets` counts
stream resets (`decoder.rs`, `tests/e2e.rs`).

## Examples

- `decode_h264_d3d12` / `decode_hevc_d3d12`: synthetic
  Annex-B in, frames out.
- `decode_vp9_d3d12` / `decode_av1_d3d12` /
  `decode_mpeg2_d3d12` / `decode_vc1_d3d12`: profile check
  plus pending-builder handling.
- `encode_h264_d3d12` / `encode_hevc_d3d12`: synthetic NV12
  in, packets out.
- `transcode_gpu_d3d12`: vector in over the GPU route,
  bitstream out for the ffmpeg oracle.

Every example skips loud without hardware.
