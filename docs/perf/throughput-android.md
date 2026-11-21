# Android throughput vs FFmpeg

rv trails gold MediaCodec decode on four codecs (vp9 widest: 44 vs 150 fps) and beats it on vp8 sync (95 vs 83) plus h264/h265 noise encode (+25.9%, +21.0%). Sync legs download frames both sides; surface legs are rv-only and software decode is gold-only. Encode settings differ: percents diagnose only.

## Environment

| Item | Value |
|---|---|
| Phone | POCO F5 Pro, Android 15, Snapdragon 8+ Gen 1 (SM8475) |
| FFmpeg gold | 9.0 static aarch64, NDK clang 21, API 21, mediacodec + SW decoders, no SW av1 decoder |
| FFmpeg enc-helper | Gold config + rawvideo decoder + dump_extradata/metadata bsfs |
| rv tree | 83b9284 (2026-11-20) plus uncommitted changes |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |

## Method

- rv legs build on host (`python scripts/android-env.py test -p rust-video-mediacodec --all-targets --no-run --target aarch64-linux-android`), push the newest `throughput-*` binary to `/data/local/tmp/mc-throughput` (`chmod 755`), and run on-device with vectors at `/data/local/tmp/vectors`: `adb shell 'RUST_VIDEO_VECTORS=/data/local/tmp/vectors /data/local/tmp/mc-throughput <leg> --nocapture'`. Surface legs add `RUST_VIDEO_MEDIACODEC_DEVICE=1`.
- Knobs are manual in the ffmpeg model; defaults hold landed behavior. See [tuning](../tuning.md).
- The Windows sheet differs in build, corpus, and vectors: counting discipline only.

## Hardware

### Decode, fps

Sync legs, downloaded frames both sides

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rv submits its own vectors 30x (ITERS=30); gold HW decodes 120f _ip (vp8 30f). Adjacent, not identical: gold needs multi-frame _ip (single-frame HW flush asserts), rv submits single-frame vectors. Gold HW to null downloads frames, which matches the rv sync leg. |
| Counting | fps = output frames / wall both sides. rv one 30-iteration run per leg from the harness print (uncaptured; the Gap 3 re-runs supersede with captured logs). Gold median of 3 timed runs after a discarded warmup (1s settle after warmup, 2s between runs). Short phone runs vary with DVFS; medians and settles contain it, they do not remove it. |
| Build | rv debug test profile, tree 83b9284 (2026-11-20) plus uncommitted changes, target aarch64-linux-android (build/push/run per Method). Gold static aarch64 binary (NDK clang 21, API 21, mediacodec + SW decoders, no SW av1 decoder). Phone per Environment. |
| Gold | round-2 legs (.tmp/gold-dec2.sh): h265, vp9, and av1 HW on 120f _ip, vp8 HW on 30f; kept round-1 leg (.tmp/gold-dec.sh): h264 HW on 120f _ip. Full commands in the scripts. Round-1 HW single-frame runs aborted (MediaCodec flush-assert edge) and are discarded. |
| Verdict basis | absolute bar 60fps: rv vp9 sync 44.2 misses while gold holds 150.2; rest clear. No per-leg budget in pins. vp9-ip leg + held-diff proof PENDING (Gaps 2-3). |

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
| Input | not same bytes: gold runs noise-30.mkv (30f 1080p yuv420p); rv runs Synth noise seeds 1..=4 (dump `encode-mc-nv12-30f.yuv`, sha256 `15f868ac...64ca384`, recipe in bench PROVENANCE). Dump-paired phone re-run PENDING (Gap 1); the rows below predate it. rv ships no vp8/vp9 encode legs. |
| Counting | fps = output frames / wall both sides. rv one 30-iteration run per leg from the harness print (uncaptured; the Gap 3 re-runs supersede with captured logs). Gold median of 3 timed runs after a discarded warmup (1s settle after warmup, 2s between runs). Short phone runs vary with DVFS; medians and settles contain it, they do not remove it. |
| Build | rv debug test profile, tree 83b9284 (2026-11-20) plus uncommitted changes, target aarch64-linux-android (build/push/run per Method). Gold enc-helper sibling binary (gold config + rawvideo decoder + dump_extradata/metadata bsfs). Phone per Environment. |
| Gold | enc-helper on noise-30.mkv, `-g 30`, one MediaCodec encoder per codec; that file is not rv's bytes. Exact command line unrecorded in pins. Settings differ; percents diagnose only. |
| Verdict basis | diagnose-only until the dump-paired re-run (Gap 1) supersedes these rows. Both av1 legs miss 60 by distance at software speed (5.4/11.4). |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 | 57.5 | 72.4 | +25.9% |
| h265 | 57.7 | 69.8 | +21.0% |
| av1 | 5.4 | 11.4 | +111.1% |

## One-sided reference, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | surface: rv vectors through MediaCodec with `RUST_VIDEO_MEDIACODEC_DEVICE=1`. Gold SW decode: h264/h265 30f concat files (round 2), vp8/vp9 30x stream_loop replays (round 1); bytes identical either way (unify on concat files in the re-run, Gap 5). Gold vp8/vp9 encode on the noise-30.mkv shape (see Encode sheet). |
| Counting | fps = output frames / wall both sides. rv one 30-iteration run per leg from the harness print (uncaptured; the Gap 3 re-runs supersede with captured logs). Gold median of 3 timed runs after a discarded warmup (1s settle after warmup, 2s between runs). Short phone runs vary with DVFS; medians and settles contain it, they do not remove it. |
| Build | rv debug test profile, tree 83b9284 (2026-11-20) plus uncommitted changes, target aarch64-linux-android (build/push/run per Method; surface legs add `RUST_VIDEO_MEDIACODEC_DEVICE=1`). Gold static aarch64 binary / enc-helper sibling. Phone per Environment. |
| Gold | no pair per row, with reason: surface rv-only (no gold counterpart); gold SW decode (the rv tier holds no software decode legs on-device; gold ships no SW av1 decoder); gold vp8/vp9 encode (no rv legs). |
| Verdict basis | reference only. Absolute bar 60fps for the rv surface rows (vp9 44.6 misses, rest clear). |

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
