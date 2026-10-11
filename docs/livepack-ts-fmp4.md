# Live TS + fMP4 packaging

Streaming-first container packaging for `rust-video-livepack`: MPEG-TS mux
and demux plus a live fMP4/CMAF mux. Bounded memory throughout: packetizers
work per access unit, the demuxer caps section and PES reassembly, and no
path buffers a whole file.

## Paths

- `crates/rust-video-livepack/src/ts.rs` — `TsMux`, `TsDemux`
- `crates/rust-video-livepack/src/fmp4.rs` — `FragmentMux`, track configs
- `crates/rust-video-livepack/src/codec.rs` — `Codec`, `AccessUnit`,
  keyframe classification, `KeyframeGate`
- `crates/rust-video-livepack/src/time.rs` — `Ts90k`, `Pcr`,
  `ClockTracker`
- `crates/rust-video-livepack/src/queue.rs` — `BoundedQueue`, `FrameSlot`

## Timestamp and PCR discipline

One clock everywhere: 90 kHz. `AccessUnit` carries raw 33-bit PTS/DTS;
`ClockTracker` unwraps observations onto a monotonic line, treating only
backward jumps past half the wrap interval as wraps, so small reorder
never looks like time travel.

- TS: PES stamps PTS always and DTS when it differs; the first packet of
  every access unit carries PCR from DTS, and keyframes set
  `random_access_indicator`. Short final packets stuff through the
  adaptation field, never the payload, so unbounded (`length == 0`) PES
  packets stay byte-exact.
- fMP4: `tfdt` carries unwrapped DTS, `trun` carries per-sample duration,
  size, flags, and composition offset (`v1` when negative). Timescale is
  90 kHz, matching the TS clock tick for tick.

## Keyframe hooks

`KeyframeGate` is the realtime join/loss hook. Any thread calls
`request()`; each mux polls per access unit and returns
`HeldForKeyframe` — appending nothing, advancing no sequence number —
until a keyframe clears the gate. Callers may assert the flag on
`AccessUnit::new` or let `AccessUnit::classified` derive it: first-VCL
rules for H.264/H.265, `show_existing_frame`/`frame_type` bits for AV1.

## Container shapes

- TS: single program, PAT/PMT before the first and every keyframe unit,
  one video PES per access unit (stream id `0xE0`). PIDs configurable;
  defaults match `ffmpeg` (`PMT 0x1000`, video `0x100`).
- fMP4: `ftyp` + `moov` init segment, optional `styp` segment headers,
  one-sample CMAF chunks (`moof` + `mdat`). H.264/H.265 samples convert
  annex-B to 4-byte lengths; AV1 samples pass through untouched.

## Spec pins

| Value | Source |
|---|---|
| TS/PES/PCR/CRC | ITU-T H.222.0 (ISO 13818-1 twin), §2.4 |
| `stream_type` 0x1B/0x24, PIDs, table ids | H.222.0 §2.4.4.9 + [FFmpeg n7.1 `mpegts.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavformat/mpegts.h) |
| NAL types | [FFmpeg n7.1 `h264.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/h264.h), [`hevc.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/hevc/hevc.h) |
| OBU types, frame bits | [FFmpeg n7.1 `av1.h`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavcodec/av1.h), [dav1d 1.5.1 `obu.c`](https://github.com/videolan/dav1d/blob/1.5.1/src/obu.c) |
| `av1C` record | [AV1-ISOBMFF §2.3](https://aomediacodec.github.io/av1-isobmff/#av1codecconfigurationbox-section) |
| `cmfc`/`iso6` brands | [FFmpeg n7.1 `movenc.c`](https://github.com/FFmpeg/FFmpeg/blob/n7.1/libavformat/movenc.c) |
| Boxes, `avcC`/`hvcC` | ISO 14496-12, 14496-15 |

AV1 over MPEG-TS is refused (`CodecNotMappable`): no registered
`stream_type` exists — even FFmpeg master carries none, and `0xCF` there
is HLS AAC audio. H.265 config passes an `hvcC` record through opaquely
instead of rebuilding one from parameter sets.

## Tests

- Unit (44): clocks, CRC check value `0x0376E6E7`, splitter, classifiers,
  SPS parse, box layouts, gate behavior.
- `tests/ts_roundtrip.rs` (6): packetize → depacketize → identical NALUs,
  PTS/DTS, flags; odd chunk splits; unbounded PES; wrap; PCR.
- `tests/ts_corrupt.rs` (7): gap, CRC, bad PES, oversize, truncation,
  duplicates, each failing loud with state intact.
- `tests/fmp4_ffmpeg.rs` (4): FFmpeg oracle — real H.264/H.265 TS through
  our demux (NALUs and stamps match `ffprobe`), our fMP4 back through
  `ffprobe`/decode (codec, dims, brands, frame counts), AV1 via IVF, and
  our TS accepted by `ffmpeg` (program table, NALU identity).

## Limits

Single program, single video PID, video only. One sample per `trun`;
samples cap at `u32::MAX - 8` bytes. The demuxer learns PIDs from PAT/PMT
and ignores unknown streams.
