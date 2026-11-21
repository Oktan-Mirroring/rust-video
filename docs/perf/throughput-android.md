# Android throughput vs FFmpeg

rv trails gold MediaCodec decode on four codecs (vp9 widest: 44 vs 150 fps) and beats it on vp8 sync (95 vs 83) plus h264/h265 noise encode (+25.9%, +21.0%). Encode settings differ: percents diagnose only.

## Environment

| Item | Value |
|---|---|
| Phone | POCO F5 Pro, Android 15, Snapdragon 8+ Gen 1 (SM8475) |
| FFmpeg gold | 9.0 static aarch64, NDK clang 21, API 21, mediacodec + SW decoders, no SW av1 decoder |
| FFmpeg enc-helper | Gold config + rawvideo decoder + dump_extradata/metadata bsfs |
| rv tree | 83b9284 (2026-11-20) plus uncommitted changes |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |

## Method

- Device build: `python scripts/android-env.py test -p rust-video-mediacodec --all-targets --no-run --target aarch64-linux-android`; push newest `throughput-*` to `/data/local/tmp/mc-throughput`, `chmod 755`.
- Run: `adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput <leg> --nocapture'`; surface legs add `RUST_VIDEO_MEDIACODEC_DEVICE=1`.
- fps = output frames / wall, both sides; rv one 30-iteration run per leg; gold median of 3 after 1 discarded warmup, 1s + 2s settles.
- Phone DVFS varies short runs; medians contain it, never remove it.
- Knobs manual (ffmpeg model); defaults hold. See [tuning](../tuning.md).

## Hardware

### Decode, fps

Sync legs, downloaded frames both sides

**Fact sheet**

| Field | Fact |
|---|---|
| Input | adjacent, not identical: rv submits single-frame vectors 30x; gold needs multi-frame (single-frame HW flush asserts) |
| Counting | fps = frames/wall; rv single run; gold median of 3, settles |
| Build | rv debug test profile on-device; gold static aarch64; phone per Environment |
| Gold | HW decode on 120f _ip (vp8: 30f); HW-to-null downloads, matches rv sync shape |
| Verdict basis | bar 60: vp9 sync 44.2 misses; rest clear; no per-leg budgets |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 145.8 | 80.2 | -45.0% |
| h265 | 135.4 | 102.5 | -24.3% |
| vp8 | 83.3 | 94.8 | +13.8% |
| vp9 | 150.2 | 44.2 | -70.6% |
| av1 | 167.4 | 152.2 | -9.1% |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | not same bytes: gold runs noise-30.mkv; rv runs Synth seeds 1..=4; dump-paired re-run pending |
| Counting | fps = frames/wall; rv single run; gold median of 3, settles |
| Build | rv debug test profile on-device; gold enc-helper sibling; phone per Environment |
| Gold | one MediaCodec encoder per codec, `-g 30`; settings differ, percents diagnose only |
| Verdict basis | diagnose-only until the dump-paired re-run; rv ships no vp8/vp9 encode legs |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 57.5 | 72.4 | +25.9% |
| h265 | 57.7 | 69.8 | +21.0% |
| av1 | 5.4 | 11.4 | +111.1% |

## One-sided reference, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rows as labeled; surface legs add `RUST_VIDEO_MEDIACODEC_DEVICE=1` |
| Counting | fps = frames/wall; rv single run; gold median of 3, settles |
| Build | rv debug test profile on-device; gold static/enc-helper; phone per Environment |
| Gold | no pair per row, with reason (see rows) |
| Verdict basis | reference only; bar 60 for rv surface rows |

- rv surface decode, no gold counterpart: h264 87.7, h265 103.6, vp8 78.5, vp9 44.6, av1 129.5.
- Gold SW decode, no rv counterpart: h264 103.8, h265 159.6, vp8 94.9, vp9 11.5; gold ships no SW av1 decoder.
- Gold vp8/vp9 encode, no rv legs: 21.2, 12.1.

## Gaps

1. PENDING dump-paired encode re-run (recipe in bench PROVENANCE); supersedes encode rows.
2. PENDING vp9-ip leg on phone (diff-5 vehicle, gold's bytes, uncommitted in tree).
3. PENDING held-diff proof (pack/reuse/pipeline/vp9-ip, uncommitted); emulator verdicts kept 7v2+8, cut 6; fps proof needs the phone. Resume block below.
4. rv phone runs uncaptured; the re-runs supersede with captured logs.
5. SW vp8/vp9 gold used 30x loop replays vs 30f concats elsewhere; bytes identical either way.

```
python scripts/android-env.py test -p rust-video-mediacodec --all-targets --no-run --target aarch64-linux-android
$bin = Get-ChildItem target/aarch64-linux-android/debug/build/rust-video-mediacodec/*/out/throughput-* -Exclude *.d | Sort-Object LastWriteTime -Descending | Select-Object -First 1
adb push $bin.FullName /data/local/tmp/mc-throughput; adb shell chmod 755 /data/local/tmp/mc-throughput
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput encode_h264 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput encode_h265 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput vp9 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors RUST_VIDEO_MEDIACODEC_DEVICE=1 /data/local/tmp/mc-throughput surface --nocapture'
```
