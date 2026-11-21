# Windows throughput vs FFmpeg

d11 h264 surface medians 5421 fps against gold 1461 (+271%) on identical bytes; d12 h264 sustained reads 2435 (+74%) on the primed steady method. Encode inverts 6/6 on shared noise (d11 h264 393 vs 92), pace only, settings and quality unmatched. F7 is dead: the 96 fps floor came from a C-only openh264 link, the nasm build runs 256.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| CPU | Ryzen 9 9950X3D, 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 31C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, unselected) |
| rv tree | 1c3a1e8 |

## Method

- rv capture: `just benches-release` (green proof), then per tier `cargo test --release -p rust-video-<tier> --test throughput -- --nocapture` with tier `rust-video-d3d11`, `rust-video-d3d12`, `rust-video-vulkan`, and `rust-video-sw --features h264-openh264`.
- Encode dumps: `cargo run -p rust-video-harness --bin dump_vectors -- .tmp/dump-vectors` (byte-compares file against in-process build at write time); `--check` re-verifies. Recipes and hashes: bench PROVENANCE.
- Devices: d3d11/d3d12 open the default hardware adapter (RTX 5080 here); rv-vulkan takes the first enumerated adapter passing the op (NVIDIA here); ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Knobs are manual in the ffmpeg model; defaults hold landed behavior. See [tuning](../tuning.md).
- Compare against the Linux sheet on counting discipline only: build, corpus, and vectors differ.

## Hardware

### Decode, fps

Surface

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rv 30 timed passes (HOPS/ITERS=30) over the single-frame blob per codec: synth h264 30B (sha256 `623ffdac...0fdb1`), nvenc h265 21901B, svtav1 obu 45380B, vpx vp9 ivf 783128B (full hashes in PROVENANCE and the vectors inventory); noise-IDR class (synth = SPS + PPS + header-only IDR slice, one grey frame). d12 hevc times the first picture only. Gold decodes the 120x concat of the same blob (120f). |
| Counting | fps = output frames / wall both sides; ffmpeg `speed=` x 30fps input, media clock implies 30fps output within 5%, frame counts exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms). rv d11 median-of-3, d12/vk mean-of-2. Full-binary throughout (no isolated grading on this table). Quiet box (no builds, CPU idle, GPU near idle); samples under spikes discarded and re-run; F4 tags throughout (vk h265 encode single, floor 229.6): lull F4 228.3+ certifies GPU-bound legs, host-heavy legs need CPU-quiet (zero-python); wide-spread mean-of-2 keeps the tighter pair with a note. Warmup: d11 100ms + untimed trailing flush; d12 4 passes; vk single pass. Spreads: d11 h264 4960-5657, h265 1797-1915, av1 1276-1298; d12 h265 thin pair 1916/2000 (submit-luck spread; tiebreak2 isolated median 1995.1 firms GREEN, grade unchanged). |
| Build | release test profile (x86-64-v3, thin LTO, opt 3, CGU1 per compat); tree 1c3a1e8 + held mc/va diffs (phone-gated, off on Windows); rustc 1.100.0, driver 621.24, RTX 5080. |
| Gold | cuvid resident (`-f null`) on the 120x concat per codec (h264-synth-120x `d0aa1721...bd2afc8`, h265-120x `bbe541ef...e2ba6558`, av1-120x `15c24e8d...2e7ba3f`, vp9-120x `529fe913...fff0729`); gpu-resident both sides, same bytes. h265 gold 1120 pools two medians (1074/1167, 18% pooled spread; pooling per sheet, run log unrecorded in pins). vp9 120f file supersedes the 30f 194 (same bytes, startup amortizes). cuvid-resident and d3d11va-downloaded golds use different decoders, not comparable to each other. Medians one era (4e92a7c, 1 warmup + 3 timed); era command lines unrecorded in pins (gold-run.ps1 ran superseded bytes). d11 vp9 decode is NVDEC both sides (D3D11 VP9 profile; software MFT is the encoder only). |
| Verdict basis | budget 1.5x rebaseline best per leg (perf-rebaseline.md); absolute bar 60fps. Floors F20 (d11 hevc -7.0%), F21 (d11 av1 -11.0%), F22 (d12 av1) exempt the misses; rest GREEN including d12 hevc thin +1.7%. vk cells keep verify2 (handoff confirm-only, floors hold). Proof: .tmp/perf-handoff.md + tiebreak2 + Floors table. |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 surface | 1461 | 5421 | +271.0% |
| d3d12 h264 surface | 1461 | 4724 | +223.3% |
| vulkan h264 surface | 1461 | 3724 | +154.9% |
| d3d11 h265 surface | 1120 | 1822 | +62.7% |
| d3d12 h265 surface | 1120 | 1958 | +74.8% |
| vulkan h265 surface | 1120 | 1479 | +32.1% |
| d3d11 av1 surface | 819 | 1280 | +56.3% |
| d3d12 av1 surface | 819 | 1310 | +60.0% |
| vulkan av1 surface | 819 | 1270 | +55.1% |
| d3d11 vp9 surface | 250.2 | 270 | +7.9% |

Sustained overlap

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rv one 120f IP pass per overlap pump over the committed _ip streams (h264_ip 362289B, h265_ip 79718B, av1_ip 108696B, vp9_ip 114029B; testsrc IP class, 4I+116P h264); frames counted then dropped (byte parity stays in the overlap tests), counts exact, drops zero. d11 all four + d12 all three prime 4 packets untimed (steady state); vk times the cold pass. |
| Counting | fps = output frames / wall both sides; ffmpeg `speed=` x 30fps input, media clock implies 30fps output within 5%, frame counts exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms). rv d11 median-of-3, d12/vk mean-of-2. Full-binary throughout, grading stays full. Quiet box (no builds, CPU idle, GPU near idle); samples under spikes discarded and re-run; F4 tags throughout (vk h265 encode single, floor 229.6): lull F4 228.3+ certifies GPU-bound legs, host-heavy legs need CPU-quiet (zero-python); wide-spread mean-of-2 keeps the tighter pair with a note. d12 av1 keeps the tighter lull 2305 over the 15%-spread quiet pair (copy-phase readback noise); isolated medians 2372 (handoff) and 2070.8 (tiebreak2) corroborate. |
| Build | release test profile (x86-64-v3, thin LTO, opt 3, CGU1 per compat); tree 1c3a1e8 + held mc/va diffs (phone-gated, off on Windows); rustc 1.100.0, driver 621.24, RTX 5080. |
| Gold | cuvid resident on the same _ip 120f bytes (gold stays resident while rv hands CPU frames back: every percent favors gold). _ip golds confirm rebaseline within 6%. Downloaded shape-matched pair per codec: gold 810/765/825/792 vs rv d3d11 sustained 1954/1719/2095/1501 (h264/h265/av1/vp9); gold includes a 373MB file write (sheet values, run log unrecorded in pins). vp9 120f file supersedes the 30f 194 (same bytes, startup amortizes). cuvid-resident and d3d11va-downloaded golds use different decoders, not comparable to each other. Medians one era (4e92a7c, 1 warmup + 3 timed); era command lines unrecorded in pins (gold-run.ps1 ran superseded bytes). |
| Verdict basis | budget 1.5x rebaseline best per leg (perf-rebaseline.md); absolute bar 60fps. All GREEN (+33 to +116%), grading full throughout (gap 12 closed: tiebreak2 isolated 2070.8, spread 11.6% readback-noise class). vk cells keep verify2 (confirm-only; av1sust -7.4% code-side, F13 ceiling holds). Proof: .tmp/perf-handoff.md + tiebreak2. |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1398 | 3021 | +116.1% |
| vulkan h265 sustained | 1293 | 1769 | +36.8% |
| vulkan av1 sustained | 1197 | 2014 | +68.3% |
| d3d11 h264 sustained | 1398 | 1954 | +39.8% |
| d3d11 h265 sustained | 1293 | 1719 | +32.9% |
| d3d11 av1 sustained | 1197 | 2095 | +75.0% |
| d3d11 vp9 sustained | 1017 | 1501 | +47.6% |
| d3d12 h264 sustained | 1398 | 2435 | +74.2% |
| d3d12 h265 sustained | 1293 | 2064 | +59.6% |
| d3d12 av1 sustained | 1197 | 2305 | +92.6% |

Sync

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rv 30 timed passes (HOPS/ITERS=30) over the single-frame blob per codec (same blobs as surface; d12 hevc times the first picture only); downloaded frames both sides. h264 rows (†) are adjacent, unpercented: gold shows the _ip downloaded number while rv runs synth (gold cannot download synth bytes). |
| Counting | fps = output frames / wall both sides; ffmpeg `speed=` x 30fps input, media clock implies 30fps output within 5%, frame counts exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms). rv d11 median-of-3, d12/vk mean-of-2. Isolated grading only on full-binary collapse or >=10% full discount with the budget at stake (isolated = same binary filtered to the leg, 3x, median): d11 av1 isolated-graded (quiet median 1038; full-binary collapses 1/3, clean runs -16% quiet); hevc grades full (no collapse 3/3, -5%, clears the budget); rest full-binary. Quiet box (no builds, CPU idle, GPU near idle); samples under spikes discarded and re-run; F4 tags throughout (vk h265 encode single, floor 229.6): lull F4 228.3+ certifies GPU-bound legs, host-heavy legs need CPU-quiet (zero-python); wide-spread mean-of-2 keeps the tighter pair with a note. Warmup: d11 100ms + untimed trailing flush; d12 4 passes; vk single pass. Spreads: d11 h264 3267-3845, h265 1195-1377 (isolated 1391 headroom); d12 quiet means. |
| Build | release test profile (x86-64-v3, thin LTO, opt 3, CGU1 per compat); tree 1c3a1e8 + held mc/va diffs (phone-gated, off on Windows); rustc 1.100.0, driver 621.24, RTX 5080. |
| Gold | d3d11va downloaded (hwdownload + rawvideo file) on the 120x concat per codec (same 120f files as surface); h264 rows show the _ip downloaded number (_ip golds confirm rebaseline within 6%). h264-synth download impossible in this build (d3d11va reports a corrupt frame and writes nothing; cuvid plus hwdownload unimplemented). vp9 120f file supersedes the 30f 194 (same bytes, startup amortizes). cuvid-resident and d3d11va-downloaded golds use different decoders, not comparable to each other. Medians one era (4e92a7c, 1 warmup + 3 timed); era command lines unrecorded in pins (gold-run.ps1 ran superseded bytes). |
| Verdict basis | budget 1.5x rebaseline best per leg (perf-rebaseline.md); absolute bar 60fps. All GREEN (d11 av1 isolated +10.9%, hevc full +3.1%). d12 trim (quiet): sync engines improved on all three legs; av1 wall 1073.8 lands between baseline 1031 and claim 1107 (submit 100 to 80us tracks the wall 1:1, claim retired as submit-luck). vk cells keep verify2 (confirm-only). Proof: .tmp/perf-handoff.md + Floors table. |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 † | 810 | 3764 | adjacent |
| d3d11 h265 | 873 | 1321 | +51.3% |
| d3d11 av1 | 837 | 1038 | +24.0% |
| d3d11 vp9 | 262.8 | 247 | -6.0% |
| d3d12 h264 † | 810 | 2495 | adjacent |
| d3d12 h265 | 873 | 1481 | +69.6% |
| d3d12 av1 | 837 | 1074 | +28.3% |
| vulkan h264 † | 810 | 2853 | adjacent |
| vulkan h265 | 873 | 1431 | +63.9% |
| vulkan av1 | 837 | 1226 | +46.5% |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | rv 30 timed noise frames per leg (NV12 1080p Synth::noise; dumps frozen gold-paired: d11 seeds 4..34 `f234c8b1...36e2837`, d12 seeds 0..30 `7c21a501...49e811d3`, vk seeds 0..4 cycled x30 `9f8242ef...d02cf79`). Untimed warmups: d11 seeds 0..4, d12 seed 30, vk same seeds plus flush. Seeds alias pairwise (`seed | 1`): the 30-seed dumps hold 15 distinct pairs; both sides share the dumps. |
| Counting | fps = output frames / wall both sides; ffmpeg `speed=` x 30fps input, media clock implies 30fps output within 5%, frame counts exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms). rv d11 median-of-3, d12/vk mean-of-2. Full-binary throughout. Quiet box (no builds, CPU idle, GPU near idle); samples under spikes discarded and re-run; F4 tags throughout (vk h265 encode single, floor 229.6): lull F4 228.3+ certifies GPU-bound legs, host-heavy legs need CPU-quiet (zero-python); wide-spread mean-of-2 keeps the tighter pair with a note. MFT pump legs carry 1-python host tax even in F4-lull. |
| Build | release test profile (x86-64-v3, thin LTO, opt 3, CGU1 per compat); tree 1c3a1e8 + held mc/va diffs (phone-gated, off on Windows); rustc 1.100.0, driver 621.24, RTX 5080. |
| Gold | NVENC p1 intra CQP qp 28 (AV1 IP g4; all-intra refused) on the 4x concat of each tier's timed frames (120f: enc-d11 `608a5759...a25f3`, enc-d12 `996a24a0...ef1d22`, enc-vk `a143a19b...3166451`). Settings and bitrate unmatched, no quality claim; percents diagnose pace only. Noise intra is brutal for CQP: gold falls from 549/456/618 on testsrc (rebaseline) to 92/142/158 on noise. Medians one era (4e92a7c, 1 warmup + 3 timed); era command lines unrecorded in pins (gold-run.ps1 ran superseded bytes). |
| Verdict basis | no budget verdicts: floor-bound throughout. d11 MFT Auto cap F14 (byte-exact legacy; faster settings barred by goldens); d12 h264 physics F3 (9.65ms; near the ~125 noise entropy-coding floor); vk h264 ladder cap F8 (~535 fastest rung unhit); vk h265 driver floor F4 (4.3ms, ffmpeg-confirmed). vk cells keep verify2 (confirm-only). Proof: roofline + .tmp/perf-handoff.md + Floors table. |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 92.1 | 393 | +326.7% |
| d3d11 h265 | 141.6 | 686 | +384.5% |
| d3d11 av1 | 157.8 | 538 | +240.9% |
| d3d12 h264 | 92.4 | 104 | +12.6% |
| vulkan h264 | 92.4 | 498 | +439.0% |
| vulkan h265 | 141.6 | 229 | +61.7% |

## Engine occupancy

**Fact sheet**

| Field | Fact |
|---|---|
| Input | scratch res vectors (.tmp/ladder-vec, host-only, uncommitted): noise-IDR singles + 120f IP at 1440p/4K per codec (ffmpeg 9.0 gyan recipes in engine-ladder.md; h264 sync/surface and all encode legs are in-code synth with no file to swap, so the h264 ladder rides sustained). Committed 1080p refs: h264 713452B/1f (unused by PC legs), h265-nvenc 21901B, av1-obu 45380B, vp9-ivf 783128B, _ip 362289/79718/108696/114029B at 120f. Pixels 1.0x/1.78x/4.0x. |
| Counting | release single-leg runs, 12 loops per run, 3 runs per cell (median-of-medians fps, n=36 loops), nvidia-smi dmon -s u sampled concurrently per run. Quiet gate per run (no cargo/rustc, CPU ≤15%, GPU idle sm ≤20, enc/dec ≤5; 30s wait + recheck, else SKIP + top-up). dmon columns are window means diluted by cargo gaps; peaks carry the signal. |
| Build | base HEAD 0a19b6d (main-tree runs audited to base bytes + base profile by file mtime + commit scope; tainted cells re-ran in a pristine worktree @0a19b6d with its own target dir); rustc 1.100.0, driver 621.24, RTX 5080. |
| Gold | no ffmpeg counterpart: rv-only occupancy probe. NVDEC duty-100% is unobserved at any tested resolution on any tier; host, parse, and cap walls arrive first. The ladder's d12 h264 session-build wall predates the 5c0eaeb prime. |
| Verdict basis | engine-tracking vs first-wall per cell (ratios against 1.78x/4.0x pixels). Proof: .tmp/engine-ladder.md (300+ clean runs). |

| Tier | Engine signal | Wall that arrives first |
|---|---|---|
| d11 | vp9 surface tracks pixels linearly to 4K (~30% NVDEC mean); vp9 pipe NVDEC 25-47%, 1.73ms service | av1 host cliff (14-16x collapse, engine idle); h265 sync readback floor; h265 surface non-monotonic; NVENC capped (F14) or software (F5) |
| d12 | av1 decode/surface/sustained linear to 4K at low duty; av1 sustained engine share 49-90% (393us gap-implied) | h265 4K parse wall; NVENC h264 physics-saturated in-leg (F3); h265 encode no HW path |
| vk | all legs 2.8-3.7x for 4x pixels, top duty sustained h265 ~22% mean | NVENC driver floor (F4) plus ladder cap (F8); no wall in sight at 4K |

## Software

### Decode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | gold 600f our bytes: 5x _ip cats (sw-h264-600f `d8078654...fee3fc`, sw-h265-600f `5a119a10...0c3a7c`, sw-av1-600f `dde34fdd...761f859`, sw-vp9-600f `a712bd2a...12fcc07`; vp8 600x single-frame `333aba5b...7436e2025`, no _ip exists). h264 IP-only legs dropped (no our-file counterpart). Per-frame pict counts unverified in pins (recipes carry no -bf flag); rows keep pre-convergence labels. |
| Counting | fps = output frames / wall (ffmpeg `speed=` x 30fps input). Gold median of 5 after 1 discarded warmup (era rule); settles unrecorded for this era. rv: no legs on this table. |
| Build | gold side only: ffmpeg 9.0 gyan binaries (libx264, libx265, libvpx, libsvtav1, libdav1d) on the Environment box; era 4e92a7c, no later re-run in pins. No rv build on this table. |
| Gold | this table is the gold reference: default SW decoders on the 600f files, thread rows per labels (1t/4t/8t/auto). Exact command lines unrecorded for this era (rebaseline-era commands in .tmp/gold-run.ps1 ran regenerated testsrc, superseded). |
| Verdict basis | one-sided reference, no budget: rv has no legs (ffmpeg/gpl backends need linkable libav*, absent from the binaries-only build; Windows link requires FFMPEG_LIB_DIR per the sw build script). |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| h264 IPB 1t | 2625.0 | no leg | no comparison |
| h264 IPB 4t | 5760.0 | no leg | no comparison |
| h264 IPB 8t | 7050.0 | no leg | no comparison |
| h264 IPB auto | 6450.0 | no leg | no comparison |
| h265 IPB 4t | 2778.0 | no leg | no comparison |
| h265 IPB auto | 4950.0 | no leg | no comparison |
| av1 4t | 3030.0 | no leg | no comparison |
| av1 auto | 3360.0 | no leg | no comparison |
| vp9 1t | 1566.0 | no leg | no comparison |
| vp9 4t | 3810.0 | no leg | no comparison |
| vp9 8t | 5190.0 | no leg | no comparison |
| vp9 auto | 5280.0 | no leg | no comparison |
| vp8 4t | 1107.0 | no leg | no comparison |
| vp8 auto | 3840.0 | no leg | no comparison |

### Encode, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | 10x sw dump enc-sw-yuv420p-300f.yuv (300f yuv420p, `ca7fa70b...cfb94727`; seeds 0..4 cycled = 2 distinct frames by `seed | 1` aliasing). 5 Mbps nominal, vpx realtime. |
| Counting | fps = output frames / wall (ffmpeg `speed=` x 30fps input). Gold median of 5 after 1 discarded warmup (era rule); settles unrecorded for this era. rv: no legs on this table. |
| Build | gold side only: ffmpeg 9.0 gyan binaries on the Environment box; era 4e92a7c, no later re-run in pins. No rv build on this table. |
| Gold | this table is the gold reference: libx264/libx265 ultrafast zerolatency, vpx cpu8 realtime, aom cpu8, svt p13 la0 lp4/lp6, per row labels. aom cpu10 ERRs both sides (Result too large; cpu-used range 0-8, crate pins cpu8). Exact command lines unrecorded for this era (gold-run.ps1 ran superseded testsrc). |
| Verdict basis | one-sided reference, no budget: same missing-libav* reason as software decode. OpenH264 runs rv-only (see One-sided reference). |

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| x264 ultrafast zerolat 4t | 720.0 | no leg | no comparison |
| x264 ultrafast zerolat auto | 930.0 | no leg | no comparison |
| x265 ultrafast zerolat 4t | 108.9 | no leg | no comparison |
| x265 ultrafast zerolat auto | 108.6 | no leg | no comparison |
| vpx-vp8 cpu8 rt 4t | 88.2 | no leg | no comparison |
| vpx-vp8 cpu8 rt 8t | 108.3 | no leg | no comparison |
| vpx-vp9 cpu8 rt 4t | 120.0 | no leg | no comparison |
| vpx-vp9 cpu8 rt 8t | 122.7 | no leg | no comparison |
| aom cpu8 8t | 19.1 | no leg | no comparison |
| aom cpu10 8t | ERR Result too large | no leg, crate pins cpu8 | no comparison |
| svt p13 la0 lp4 | 209.1 | no leg | no comparison |
| svt p13 la0 lp6 | 267.3 | no leg | no comparison |

## One-sided reference, fps

**Fact sheet**

| Field | Fact |
|---|---|
| Input | surface-encode: GPU surface decoded from synth, no YUV bytes (per-pass surface freshness unrecorded in pins). Transcode: timed synth (d11 warmup alien packets `annex_b_h264_packets` + cropped; d12 warmup clone of timed). openh264: sw 30f planar dump seeds 0..4 cycled (`4cd405cd...afd5fec`) = 2 distinct frames by `seed | 1` aliasing; distinct-content probe reads 145 vs 256. vp9enc: d11 30f dump seeds 4..34 (no NVENC VP9 path exists). livepack: synthetic units, no codec bytes. |
| Counting | fps = output frames / wall. d11 median-of-3, d12 mean-of-2 (xcode noisy legs); livepack isolated singles (full-binary parallel alloc-churn costs 16-29%). Quiet box (no builds, CPU idle, GPU near idle); samples under spikes discarded and re-run; F4 tags throughout (vk h265 encode single, floor 229.6). |
| Build | release test profile (x86-64-v3, thin LTO, opt 3, CGU1 per compat); tree 1c3a1e8 + held mc/va diffs (phone-gated, off on Windows); rustc 1.100.0, driver 621.24, RTX 5080. openh264 nasm ASM path (the C-only link reads ~96; F7 dead). |
| Gold | no ffmpeg counterpart on any row: surface-encode, transcode, and livepack have no gold legs; openh264 absent from the ffmpeg build (no libopenh264); VP9 has no NVENC path. Reference only. |
| Verdict basis | reference only, no budgets; absolute bar 60fps (all rows clear; vp9enc 69 tightest). |

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 653, h265 922, av1 722.
- Transcode via rv, no ffmpeg counterpart: d3d11 454, d3d12 622.
- h264 openh264 via rv, absent from the ffmpeg build: 256.
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.
- livepack via rv, no ffmpeg counterpart (isolated singles): TsMux 502432, TsDemux 502015, FragMux 166447.

## Floors

Proof tags: perf-final = `.tmp/perf-final.md`, verify = `.tmp/perf-verify.md`, verify2 = `.tmp/perf-verify2.md`, ladder = `.tmp/engine-ladder.md`, roofline = `.tmp/roofline.md`; bare hashes are commits.

| # | Claim | Proof | Status |
|---|---|---|---|
| F1 | d11 h264 surface demonstrated pace ~285us | perf-final §F1; verify2 +24%; e93b61d best 5429; verifymax lull 5611; handoff best 5657 | VACATED, leg GREEN |
| F2 | d12 h264 surface quoted 2112 silicon max | perf-final §F2 (rejected: B-sample 2243 beats it); verify moot | MOOT, leg GREEN |
| F3 | d12 h264 encode 9.65ms physics (8ms NVENC entropy + ~1ms) | roofline; verify2 spot flat; ladder 103/103 | CONFIRMED |
| F4 | vk h265 encode 4.3ms driver floor, content-independent | roofline; verify2 spot; ladder 230.6/230.6 | CONFIRMED |
| F5 | d11 vp9 encode software MFT floor (fastest QvS landed) | roofline; verify2 spot; ladder 69/69 zero NVENC | CONFIRMED |
| F6 | d11 h264 sustained 1103-stands walk (MFT wait + locality + pack + drains) | roofline; verify VACATED (leg 1430 past 1142); verify2 +30%; ada9dea 1969; verifymax lull 1957; handoff median 1954 | VACATED, leg GREEN |
| F7 | sw h264 encode 10.8ms parallel floor (threads scale 1-4, lib caps at 4) | perf-final §F7 (C-only build); KILLED: nasm build 256 (sw-base logs, 5ab612f, thread sweep); openh264-sys2 build.rs has no rerun-if (stale C-only link) | DEAD (mechanism was a build artifact) |
| F8 | vk h264 encode ladder-capped (~535 fastest rung vs 750; API floor 1.22ms) | perf-final §F8; verify2 spot | CONFIRMED |
| F9-11 | d12 sync >= surface + 311us bus-min - handoff (446/761/1005us walls) | perf-final §F9-11; verify REOPENED (corrected steady arithmetic lands under budget); verify2 VACATED-MOOT (prime+pool, +72/+28/+10%) | VACATED-MOOT, legs GREEN |
| F12 | d12 h264 sustained engine-alone 707 > 657us budget | perf-final §F12 + CORRECTION (707 is span, not service); verify2 spot; SUPERSEDED by 5c0eaeb (2503); verifymax full-lull 2450; handoff quiet 2435 | VACATED (method retired); gap re-derivation moot (leg GREEN +60%) |
| F13 | vk av1 sustained ~2100 zero-copy asymptote (slope 0.15, copy 187us) | perf-final §F13; verify2 spot (2016 under ~2100); verifymax lull 1785 (holds as ceiling); handoff quiet 1865 (ceiling holds, pace -7.4% code-side) | CONFIRMED |
| F14 | d11 MFT Auto cap (byte-exact legacy; faster settings barred by goldens) | perf-final §F14; verify2 spot (7 legs flat); adapt-d11 corroborates ±4%; verifymax 5/6 within -3% or content-explained; handoff quiet av1enc 537.8 vs 543.7 (-1.1%, anomaly was 1-python host tax) | CONFIRMED whole |
| F15 | vk h264 surface phased sums 239-244us vs 217.4 budget | verify §F15; verify2 spot | CONFIRMED |
| F16 | vk h265 surface sums 605-614us vs 505.1 budget | verify §F16; verify2 spot | CONFIRMED |
| F17 | vk h264 sustained service gaps 280-286us vs 247.8 (max 3571 vs 4036) | verify §F17; verify2 spot; handoff quiet 2967 (pace recovered, -1.8%) | CONFIRMED |
| F18 | vk h265 sustained gaps 510-517us vs 380.1 | verify §F18; verify2 spot; handoff quiet 1707 (-3.5%) | CONFIRMED |
| F19 | d11 h265 decode additive 815us retired (overlap: walls under phase sums); overlap-aware floor under the 780.6 budget | verify §F19; verifymax: 1436 phase-impossible and unreproduced; handoff: full-quiet median 1321.4 clears 1281 (+3.1%), isolated 1390.8 headroom | VACATED-CONFIRMED (leg GREEN full; grades full, no new rule) |
| F20 | d11 h265 surface submit+wait sum vs 510.2 budget (direct split) | verify §F20; verifymax: 552 wall load-era, wait-floor refuted; handoff `1c3a1e8` print: submit 87-95 (DXVA API 66-69 + mandatory surface work, no fat) + wait 442-448 (engine, d12 cross-confirms 442-444); best mean pace 518.1 vs 510.2; 1962 retired as tail; min_sum 506 single-call tail, mean unobserved under 518 | CONFIRMED RE-DERIVED (submit API-bound, wait engine-bound; floored) |
| F21 | d11 av1 surface submit+wait sum vs 695.4 budget (direct split) | verify §F21; verifymax analog re-walk retired (wait ~= 713 analog wrong); handoff direct: wait 694-699 fits the budget alone, submit-side API cost binds; min_sum 746-756 misses budget by 51us (tail cannot reach) | CONFIRMED RE-RE-DERIVED (budget unreachable; floored) |
| F22 | d12 av1 surface engine 691-694us vs 677.1 budget | verify §F22; verify2 spot; verifymax lull engines 694-700 in band; handoff quiet 691/692 exact | CONFIRMED |

## Gaps

1. Quiet-box re-grade round on HEAD: CLOSED (handoff: d11 quiet median-of-3 on all 19 legs, d12 quiet means, vk confirm-only).
2. d12 av1 sustained isolated pace: CLOSED (handoff: full-B 2472.7 confirms 2465 best-vs-best, isolated quiet median 2372, depth 4 filled; grading stays full).
3. F21 re-walk: CLOSED (verifymax: wall-floor ~= 792us isolated vs 695us budget, old waits retired, F21 CONFIRMED).
4. d12 h265 encode: no HW path on this adapter (ladder SKIP on 621.24).
5. d12 VP9: no picture builder (no sync/surface/sustained legs).
6. d11 vp9 surface-encode: no surface import.
7. d12 h265 4K: parse fails with `picture without first slice` on the ffmpeg-valid NVENC stream (ladder BLOCKED).
8. d11 av1 4K host cliff: 14-16x collapse at idle engine (phase re-probe on master).
9. d11 h265 surface 4K non-monotonic: 1686 beats 1440p 1070 (NAL-structure compare).
10. sw ffmpeg/gpl legs on Windows: need linkable libav* (`FFMPEG_LIB_DIR` per the sw build script).
11. d12 h265 surface C-sample tiebreak: CLOSED (tiebreak2: isolated quiet median-of-3 1995.1 (1986.8/2005.4/1995.1), +3.6% vs 1926, spread 0.9%, F4 >= 228.5 + dirt <= +26 on all samples; firms the thin full-mean GREEN, grade unchanged).
12. d12 av1 sustained third sample: CLOSED (tiebreak2: isolated quiet median-of-3 2070.8 (2070.8/1974.6/2214.0), spread 11.6% readback-noise class, F4 231.8 x3, depth 4 filled; leg GREEN +92% vs 1076, grading stays full).
