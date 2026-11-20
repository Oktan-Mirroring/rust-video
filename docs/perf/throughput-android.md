# Android throughput vs FFmpeg

rv trails gold MediaCodec decode on four codecs (vp9 widest: 44 vs 150 fps) and beats it on vp8 sync (95 vs 83) plus h264/h265 noise encode (+25.9%, +21.0%). Sync legs download frames both sides; surface legs are rv-only and software decode is gold-only. Encode settings differ: percents diagnose only.

On-device run 2026-11-20, POCO F5 Pro, rv debug test profile single 30-iter legs, gold median of 3 after a discarded warmup. Encode pairing and held-diff proof are PENDING (see Gaps).

## Environment

| Item | Value |
|---|---|
| Phone | POCO F5 Pro, Android 15, Snapdragon 8+ Gen 1 (SM8475) |
| FFmpeg gold | 9.0 static aarch64, NDK clang 21, API 21, mediacodec + SW decoders, no SW av1 decoder |
| FFmpeg enc-helper | Gold config + rawvideo decoder + dump_extradata/metadata bsfs |
| rv tree | 83b9284 (2026-11-20) plus uncommitted changes |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |

## Method

- rv legs build on host (`python scripts/android-env.py test -p rust-video-mediacodec --all-targets --no-run --target aarch64-linux-android`), push the newest `throughput-*` binary to `/data/local/tmp/mc-throughput` (`chmod 755`), and run on-device with vectors at `/data/local/tmp/vectors`: `adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput <leg> --nocapture'`. One 30-iteration run per leg; fps comes from the harness print, 10 of 13 legs clear the 60fps bar. Surface legs add `RUST_VIDEO_MEDIACODEC_DEVICE=1`.
- Gold decode: static gold binary, `-benchmark`, `-f null -`. Round-2 legs (`.tmp/gold-dec2.sh`): SW decoders on 30f concat files (h264/h265), MediaCodec decoders on 120f _ip inputs (vp8 30f). Kept round-1 legs (`.tmp/gold-dec.sh`): h264 HW on 120f _ip, vp8/vp9 SW via 30x stream_loop replays. Round-1 HW single-frame runs aborted (MediaCodec flush-assert edge) and are discarded.
- Gold encode: enc-helper sibling binary on noise-30.mkv (30f 1080p yuv420p), `-g 30`, one MediaCodec encoder per codec. That file is not rv's bytes: rv encodes Synth noise seeds 1..=4, dumped by `dump_vectors` (`encode-mc-nv12-30f.yuv`, recipe in bench PROVENANCE). Phone re-run on the dump is pending; encode values below predate it.
- fps divides output frames by wall both sides; gold takes the median of 3 timed runs (warmup discarded, 1s settle after warmup, 2s between runs), rv reports one run each.
- Short phone runs vary with DVFS; medians and settles contain it, they do not remove it.
- Knobs are manual in the ffmpeg model; defaults hold landed behavior. See [tuning](../tuning.md).

## Hardware

### Decode, fps

Sync legs, downloaded frames both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 145.8 | 80.2 | -45.0% |
| h265 | 135.4 | 102.5 | -24.3% |
| vp8 | 83.3 | 94.8 | +13.8% |
| vp9 | 150.2 | 44.2 | -70.6% |
| av1 | 167.4 | 152.2 | -9.1% |

Gold HW inputs are 120f _ip (vp8 30f); rv submits its own vectors 30x: adjacent, not identical. rv vp9 sync misses the 60 bar at 44.2 while gold holds 150.2.

### Encode, fps

Noise input both sides, settings differ, percents diagnose only:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 57.5 | 72.4 | +25.9% |
| h265 | 57.7 | 69.8 | +21.0% |
| av1 | 5.4 | 11.4 | +111.1% |

Both av1 legs miss 60 by distance at software speed. rv ships no vp8/vp9 encode legs.

## Software

Gold SW decode, no rv counterpart (see One-sided reference); the rv tier holds no software decode legs on-device.

## One-sided reference, fps

- rv surface decode, no gold counterpart: h264 87.7, h265 103.6, vp8 78.5, vp9 44.6, av1 129.5.
- Gold SW decode, no rv counterpart: h264 103.8, h265 159.6, vp8 94.9, vp9 11.5; gold ships no SW av1 decoder.
- Gold vp8/vp9 encode, no rv legs: 21.2, 12.1.

## Gaps

1. PENDING dump-paired encode re-run: gold noise-30.mkv vs rv Synth seeds 1..=4 are different bytes. Push `encode-mc-nv12-30f.yuv` (dump recipe in bench PROVENANCE), run the enc-helper `-g 30` shape on it, median-of-3; supersedes the encode rows.
2. PENDING vp9-ip leg on phone: the diff-5 vehicle (120f `vp9-1080p_ip.ivf`, gold's bytes) sits uncommitted in tree; run `vp9 --nocapture` on-device per the resume block.
3. PENDING held-diff proof: pack fast paths, direct pack, buffer reuse, pipelined encode, vp9-ip leg (diffs 1-5, 7v2, 8-10, all uncommitted). Emulator verdicts kept 7v2+8 and cut 6; fps proof needs the phone. Resume block (host build, median-of-3, 1 warmup discarded, 2s settles):

```
python scripts/android-env.py test -p rust-video-mediacodec --all-targets --no-run --target aarch64-linux-android
$bin = Get-ChildItem target/aarch64-linux-android/debug/build/rust-video-mediacodec/*/out/throughput-* -Exclude *.d | Sort-Object LastWriteTime -Descending | Select-Object -First 1
adb push $bin.FullName /data/local/tmp/mc-throughput; adb shell chmod 755 /data/local/tmp/mc-throughput
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput encode_h264 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput encode_h265 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput vp9 --nocapture'
adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors RUST_VIDEO_MEDIACODEC_DEVICE=1 /data/local/tmp/mc-throughput surface --nocapture'
```

4. rv phone runs are uncaptured (no raw log; single runs as printed). The re-runs above supersede them with captured logs.
5. SW vp8/vp9 gold runs 30x stream_loop replays (round 1) while h264/h265 runs 30f concat files (round 2); bytes identical either way. Unify on concat files in the re-run.

## Comparability notes

- Content: gold SW legs run the vectors rv submits 30x, identical bytes (concat files for h264/h265, stream_loop replays for vp8/vp9). HW legs are _ip-adjacent: gold needs multi-frame _ip (its single-frame HW flush asserts), rv submits its own vectors. Encode legs are not same-bytes yet (gold noise-30.mkv vs rv Synth seeds 1..=4).
- Shape: gold HW to null downloads frames, which matches the rv sync leg; surface legs stay one-sided.
- Gaps: see Gaps (encode pairing, vp9-ip leg, held-diff proof, uncaptured rv runs, SW input unification); rv has no vp8/vp9 encode legs; gold has no SW av1 decoder.
- Cross-OS: the Windows sheet differs in build, corpus, and vectors: counting discipline only.
