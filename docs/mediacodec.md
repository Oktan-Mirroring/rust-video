<!-- source: ndk 0.9.0 published source, android.jar platform android-36.1; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# MediaCodec tier

`rust-video-mediacodec` decodes H.264, H.265, AV1, and VP9 through the
platform `MediaCodec` on Android, behind the facade `Decoder` trait.
Byte buffers in, `CpuFrame` out. No surface path yet.

Source tiers: **[T1]** read from the artifact, **[T2]** consistent
secondary, **[?]** unresolved, never filled.

## Gates

| Axis | Floor | Source |
|---|---|---|
| Device | API 21, `AMediaCodec` starts here [T1] | NDK headers, ndk-sys 0.6 |
| Async callbacks | API 28, `setAsyncNotifyCallback` [T1] | ndk 0.9.0 `api-level-28` gate |
| AV1 decode | API 29, first platform AV1 codecs [T2] | Android 10 behavior notes |
| Bindings | ndk `0.9.0`, ndk-sys `0.6.0` [T1] | docs/compat.md pins |

`open` reads the live level through `android_get_device_api_level`
[T1, ndk-sys 0.6] and refuses below the floor with `BelowMinSdk`.
Level 28 and up drains through async callbacks bridged into a polled
`EventQueue`; older devices use sync dequeue with a bounded wait.
The API-28 entry points are never called below 28; the loader binds
function symbols lazily, so referencing them does not break API-21
startup [T2].

## Formats

One builder per codec, all pure and host-tested:

| Builder | Mime | Min API |
|---|---|---|
| `FormatSpec::h264` | `video/avc` | 21 |
| `FormatSpec::h265` | `video/hevc` | 21 |
| `FormatSpec::av1` | `video/av01` | 29 |
| `FormatSpec::vp9` | `video/x-vnd.on2.vp9` | 21 |
| `FormatSpec::vp8` | `video/x-vnd.on2.vp8` | 21 |

Keys (`mime`, `width`, `height`, `frame-rate`, `csd-0`, `csd-1`,
`color-format`, `stride`, `slice-height`), the five mimes above, the
color values 19 (planar) / 21 (semi-planar), and the buffer flags 2
(codec-config) / 4 (end-of-stream) / 32 (decode-only) were read from
the installed platform, not copied from memory [T1]:

```sh
javap -classpath "$ANDROID_HOME/platforms/android-36.1/android.jar" \
  -constants android.media.MediaFormat | grep -E 'KEY_|MIMETYPE_VIDEO'
```

Output layout comes from the live output format on every
`FormatChanged`, with `width`/`height` as the stride fallback. Color
values outside 19/21 fail typed as `UnsupportedColorFormat`.

## Bridge

`EventQueue` is the async-to-poll joint. On API 28+ all four ndk
callback slots push `CodecEvent`s; `decode` and `flush` drain them.
All four slots stay set even when a handler is a no-op: leaving one
unset while any other is set faults on Android 12 and earlier
[T1, ndk 0.9.0 `AsyncNotifyCallback` docs, NdkMediaCodec.cpp].
Input indices the current call cannot use go back on the queue;
frames are never dropped to make progress.

## Quirks

`crates/rust-video-mediacodec/quirks.toml` holds the per-`SoC` table,
embedded at build time and matched by lowercase substring against the
device `SoC` name. Every entry names its `fallback`: the behavior the
code takes when the quirk does not match.

Suspected entries graduate to confirmed only under the 2-`SoC`
device-proof rule: the misbehavior reproduces on two distinct chips,
different vendors preferred, through the repo device tier, and the
entry's `source` names both runs. Until then the code takes the
fallback everywhere and the entry stays `suspected`.

## Host behavior

Off Android everything native is compiled out. `open` returns the
typed `BackendAbsent`; format builders, quirk matching, the
`DecodeState` machine, plane splitting, and the event queue all run
as host unit tests (37 tests). Device-only code is covered by
`cargo check --target aarch64-linux-android`.

## Limits

- No end-of-stream API: the facade `Packet` carries no EOS signal,
  so trailing frames that need one stay inside the codec.
- No timestamps out: `CpuFrame` has no timing field, so output PTS
  is dropped after queueing.
- Byte-buffer copies: one copy per frame from codec buffers.
  A zero-copy surface path is future work, not this tier.

## References

- <https://docs.rs/ndk/0.9.0/ndk/media/media_codec/struct.MediaCodec.html>
- <https://docs.rs/ndk/0.9.0/ndk/media/media_format/struct.MediaFormat.html>
- <https://developer.android.com/reference/android/media/MediaFormat>
- <https://developer.android.com/reference/android/media/MediaCodec>
- <https://developer.android.com/ndk/reference/group/media>
