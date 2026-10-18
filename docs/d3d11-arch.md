# D3D11 tier

Profile probe plus H.264 decode, MFT encode, and transcode
behind the caps vocabulary.

## Operations

| Op | Negotiates | Runs |
|---|---|---|
| `H264Decode` | H.264 profile, NV12 target, session config | Annex-B in, NV12 frames out |
| `H264Encode` / `HevcEncode` | encoder MFT, D3D11 bind | NV12 in, coded packets out |
| `ShellDecode` x5 | profile probe only | loud error, empty flush |

`D3d11Device::open` declines a machine it cannot run on through
`D3d11Error::Skipped`, naming the cause in `SkipReason`
(`device.rs`, `error.rs`); examples print `SKIP` and exit.

## Encode

Encode runs over Media Foundation encoder MFTs: `D3d11Encoder`
packs strided NV12 contiguous and drives `mf::Session`
(`encoder.rs`, `mf.rs`). Sessions code 30 fps progressive NV12
with an area-scaled bitrate, width times height times 3 clamped
to 100_000..=50_000_000 (`mf.rs`).

CPU input travels as one aligned MF sample stamped with pts;
GPU input copies the decode-slice region into the cached MFT
input texture under the submit lock (`mf.rs`). Completion drains
`ProcessOutput` until `NEED_MORE_INPUT`; a size change drains
and reconfigures (`mf.rs`).

Asynchronous MFTs run on the callback model: an
`IMFAsyncCallback` pump holds one outstanding `BeginGetEvent`
per consumed event, and the callback only queues the event type
and re-arms, never touching the GPU. `ProcessInput`,
`ProcessOutput`, and the drain wait stay on the submitter
thread. A submit failure flushes the transform and resets the
pump instead of wedging the session (`mf.rs`).

## Transcode

`Transcoder` pairs an H.264 decoder with an MF encoder and plans
in caps vocabulary (`GpuCaps`, `Plan`, `Route`, `FallbackCause`)
(`transcode.rs`, caps `transcode.rs`). The route stays on the GPU
when every hop holds on one device: `NtHandle` share plus decode
export plus encoder import, with surfaces flowing through
`encode_surface` or `encode_scaled`, pixels never mapped.
Otherwise the CPU path runs with the first miss named loud:
`ShareUnsupported` without a share mechanism, else
`NoSharedExport` or `EncoderCpuOnly`. Scale and convert hops fail
loud the same way.

The route never spans adapters: one device serves both ends, and
`Plan::for_devices` has no call site in the tier
(`transcode.rs`).

## Frames

`D3d11Frame` holds NV12 bytes with identity and a budget lease;
`nv12()` exposes the bytes and `to_cpu()` splits NV12 planes
(`decoder.rs`). `export()` returns the NT handle over the shared
decode texture, or `Opaque { name: "d3d11" }` when the driver
declines sharing. `GpuSurface` stays device-resident without CPU
access and carries an `ExportLayout`: the two-plane descriptor
(format, slice, visible origin and extent) behind the NT handle,
sourcing the scale and encode regions so the GPU route stays
pixel-clean (`decoder.rs`, `scale.rs`, `encoder.rs`). Each frame
holds a `Lease` against the `DedicatedVideoMemory` budget,
released on drop (caps `traits.rs`).

## Performance notes

- One decode session per stream: 16 reference views plus 4 spare
  over one array texture, one copy lane per view; readback
  copies the visible rectangle only (`session.rs`,
  `submit.rs`).
- One submit per picture: four buffers (picture parameters,
  inverse quantization matrix, bitstream, slice control) inside
  one begin/end frame pair; raw-2 configs take short slice
  controls (`session.rs`).
- Decode runs submit-then-readback: every picture submits and copies
  first, then every queued output reads back, so back-to-back
  submits never stall on a map (`decoder.rs`).
- Each copy lane records on its own deferred context and the
  immediate context executes the finished list, ending a per-lane
  event query that gates the read (`submit.rs`).
- Bitstream bytes past the driver allocation fail as
  `BitstreamTooLarge`; nothing truncates (`session.rs`).
- CPU encode packs strided NV12 contiguous once per frame into
  one aligned MF sample stamped with pts (`encoder.rs`,
  `mf.rs`).
- The MF platform starts once per process and never shuts down
  (`mf.rs`).
- The DPB holds 16 slots; each submission carries picture
  parameters, quantization matrix, slices, and bitstream
  (`dpb.rs`, `session.rs`).

## Threading and teardown

- The immediate context stays the single submitter behind one
  device-level lock: decode begin/end pairs, command-list
  execution, video-processor blits, and MF texture copies take
  it briefly. Completion waits, maps, and CPU copies never hold
  it (`device.rs`, `submit.rs`, `session.rs`, `scale.rs`,
  `mf.rs`).
- The API stays synchronous per handle with no spawned threads;
  independent sessions submit concurrently through the lock and
  wait outside it. The shared export texture travels in an
  `Arc`. `D3d11Device::open` sets
  `SetMultithreadProtected(true)` on the immediate context
  (`device.rs`, `session.rs`).
- Each MF entry takes a `ComGuard`; async MFT events arrive on
  MF worker threads through the callback pump, which only
  signals. GPU work stays on the submitter thread (`mf.rs`).
- A failed picture resets the stream: DPB, session, shared
  export, and active sequence drop together, and the next
  picture reopens clean (`decoder.rs`).
- Over-budget frames fail as `OverBudget`, oversized streams as
  `OverCaps`, short CPU splits return `None`; no reachable
  `expect` remains (`error.rs`, `decoder.rs`).

## Validation notes

DXVA H.264 structs carry compile-time size and offset asserts:
1040/224/864 bytes with the current picture at 4, the reference
list at 16, and the order counts at 40 (`dxva.rs`). The staging
copy test asserts above 186 MB/s over 1080p frames
(`session.rs`).

Shell decoders report `ProfileUnsupported` without a profile and
the pending picture builder with one; flush returns empty
(`decoder.rs`). Hardware decode matches the ffmpeg
software oracle byte-exact over the committed vector: 15
frames, 1,728,000 NV12 bytes, 0 mismatches
(`tests/e2e.rs hardware_matches_software_oracle`).

`tests/e2e.rs` drives the committed 15-picture vector through
overlap (`max_inflight` above 1), GPU transcode verified clean
by the ffmpeg oracle, failure reset plus recovery, and a
zero-reset steady run; `H264Decoder::resets` counts stream
resets (`decoder.rs`, `tests/e2e.rs`).

`tests/throughput.rs` measures 30 1080p GPU hops against
the 60 fps bar (305.5 fps); `RUST_VIDEO_BENCH_STRICT=1`
fails below the bar.

## Examples

- `decode_h264_d3d11`: synthetic Annex-B in, frames out.
- `decode_hevc` / `decode_vp9` / `decode_av1` / `decode_mpeg2` /
  `decode_vc1`: profile check plus pending-builder handling.
- `encode_h264` / `encode_hevc`: synthetic NV12 in, packets out.
- `transcode_gpu_d3d11`: vector in over the GPU route,
  bitstream out for the ffmpeg oracle.

Every example skips loud without hardware.
