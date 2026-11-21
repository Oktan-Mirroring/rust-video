# Windows throughput vs FFmpeg

d11 h264 surface medians 5421 fps against gold 1461 (+271%) on identical bytes; d12 h264 sustained reads 2435 (+74%) on the primed steady method. Encode inverts 6/6 on shared noise (d11 h264 393 vs 92), pace only, settings and quality unmatched. F7 is dead: the 96 fps floor came from a C-only openh264 link, the nasm build runs 256.

rv tree 1c3a1e8 (+ held mc/va diffs, outside the Windows binaries). The handoff-close round (2026-11-21, `.tmp/perf-handoff.md`) graduates every d11 cell to quiet median-of-3 and the d12 cells to quiet means on a zero-python box, closing the verifymax handoffs; vk cells keep verify2 (confirm-only). Gold medians one era (4e92a7c): 1 discarded warmup, 3 timed HW runs, 5 timed SW runs; `_ip` confirms within 6% of rebaseline.

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

- rv capture: `just benches-release` (green proof), then per tier `cargo test --release -p rust-video-<tier> --test throughput -- --nocapture` with tier `rust-video-d3d11`, `rust-video-d3d12`, `rust-video-vulkan`, and `rust-video-sw --features h264-openh264`. d11 grades median-of-3, d12/vk/sw mean-of-2. Quiet box: no builds, CPU idle, GPU near idle; discard samples taken under spikes and re-run them.
- Verifymax era-tagging: late runs carry an F4-quietometer tag (vk h265 encode single, driver floor 229.6): lull runs (F4 228.3+) certify GPU-bound legs; host-heavy legs (sustains, MFT pumps, sw) keep a residual tax even in lull and need CPU-quiet (zero-python). Graduations use quiet pairs or era-agreeing clusters only; a mean-of-2 with a wide spread keeps the tighter pair with a note.
- Isolated legs run the same binary filtered to the leg, 3x, median. Isolated grading applies only on full-binary collapse or a >=10% full discount with the budget at stake: d11 av1 decode qualifies (collapse 1/3, clean runs -16% quiet), d11 hevc decode does not (no collapse, -5%, clears the budget full) and grades full; livepack parallel alloc-churn costs 16-29%.
- Sustained legs run one 120-frame IP pass per overlap pump. d11 all four and d12 all three prime untimed (steady state); vk times the cold pass. d11 counts frames then drops them (byte parity stays in the overlap tests); frame counts exact, queue drops zero.
- Sync/surface legs: d11 warms 100ms with untimed trailing flush; d12 sync primes 4 passes; d11/d12 print submit/engine/copy phase medians, d11 surfaces print submit/wait medians plus the argmin call.
- Gold decode: cuvid resident (`-f null`) plus d3d11va downloaded (hwdownload + rawvideo file) per codec, all on our bytes: 120x single-frame concats for sync/surface shape, `_ip` 120f for sustained. H264 synth runs cuvid resident only: d3d11va reports a corrupt frame and writes nothing, cuvid plus hwdownload is unimplemented in this build.
- Gold encode: NVENC p1 intra CQP qp 28 (AV1 IP g4; all-intra refused) on per-tier 120f noise dumps (4x concat of each tier's timed frames; va native 120f). SW encode runs the 10x sw dump (300f yuv420p) at 5 Mbps nominal, vpx realtime.
- Encode dumps: `cargo run -p rust-video-harness --bin dump_vectors -- .tmp/dump-vectors` (byte-compares file against in-process build at write time); `--check` re-verifies. Recipes and hashes: bench PROVENANCE.
- Content alias: `Synth::noise` seeds `seed | 1`, so each even/odd seed pair emits identical bytes (the sw leg cycles 4 seeds = 2 distinct frames; the 30-seed d11/d12 dumps hold 15 distinct pairs). Both sides share the dumps, so pairs stay fair; the dumps are frozen (gold-paired). Distinct-content pace reads lower (sw distinct-content probe 145 vs 256).
- fps divides actual output frames by wall both sides; ffmpeg parses `speed=` against the 30fps input. Validity: the media clock implies 30fps output within 5% and frame counts are exact (`elapsed=` truncates to 2 decimals, unusable under ~100ms).
- Software decode gold runs 600f our bytes: 5x `_ip` cats (vp8 600x single-frame, no `_ip` exists). The h264 IP-only legs are dropped: no our-file counterpart.
- Devices: d3d11/d3d12 open the default hardware adapter (RTX 5080 here); rv-vulkan takes the first enumerated adapter passing the op (NVIDIA here); ffmpeg pins cuvid/nvenc/d3d11va explicitly.
- Knobs are manual in the ffmpeg model; defaults hold landed behavior. See [tuning](../tuning.md).
- Compare against the Linux sheet on counting discipline only: build, corpus, and vectors differ.

## Hardware

### Decode, fps

Surface, gpu-resident both sides:

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

Every pair shares bytes: gold decodes the 120x single-frame concat. d3d11 vp9 decode is NVDEC both sides (ours via the D3D11 VP9 decoder profile); software MFT applies to the VP9 encoder only. d11 cells are quiet median-of-3 (h264 best 5657, range 4960-5657; h265 range 1797-1915; av1 range 1276-1298); d12 cells are quiet means (h265 thin: 1916/2000, submit-luck spread, tiebreak owed). h265 gold 1120 pools two medians (1074/1167, 18% pooled spread).

Sustained overlap, 120-frame IP both sides:

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

Every leg shares gold's exact bytes; gold stays resident while rv legs hand CPU frames back: each percent favors gold. d12 sustains are full-binary means, depth 4 filled (h264/hevc quiet; av1 keeps the tighter lull 2305 over the 15%-spread quiet pair, isolated median 2372, full-B 2473 confirms the 2465 endpoint). Downloaded pairs per codec are the shape-matched d3d legs: gold 810/765/825/792 against rv d3d11 sustained 1954/1719/2095/1501 (h264/h265/av1/vp9), gold with a 373 MB file write, rv with CPU frames in hand.

Sync, downloaded both sides (`†` = adjacent bytes, unpercented):

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

Gold downloads the 120x single-frame concat to a file; h264 rows show the `_ip` downloaded number (gold cannot download synth bytes). d11 cells are quiet median-of-3 (h264 range 3267-3845; h265 range 1195-1377, isolated median 1391 headroom); d12 syncs are quiet means. d11 av1 decode stays isolated-graded (quiet median 1038): full-binary runs collapse it to ~100fps and discount clean runs ~16% (reproduced quiet); hevc grades full (no collapse, -5%, clears the budget).

### Encode, fps

Gold runs intra CQP qp 28 (AV1 IP g4) on each tier's own noise dump; rv runs MFT/Video-Encode/VkVideo defaults on the same frames. Settings differ: percents diagnose only, no quality claim.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 92.1 | 393 | +326.7% |
| d3d11 h265 | 141.6 | 686 | +384.5% |
| d3d11 av1 | 157.8 | 538 | +240.9% |
| d3d12 h264 | 92.4 | 104 | +12.6% |
| vulkan h264 | 92.4 | 498 | +439.0% |
| vulkan h265 | 141.6 | 229 | +61.7% |

Noise intra is brutal for CQP: gold falls from 549/456/618 on testsrc to 92/142/158 on noise, and every rv leg outpaces it per frame at unknown quality. d3d12 h264 clears gold by 11.5% near the noise entropy-coding floor (~125); vulkan h265 clears it by 61.7% above the NVIDIA VkVideo submit floor (4.4 ms, ffmpeg-confirmed). Dumps alias pairwise (see Method); both sides share them.

## Engine occupancy

| Tier | Engine signal | Wall that arrives first |
|---|---|---|
| d11 | vp9 surface tracks pixels linearly to 4K (~30% NVDEC mean); vp9 pipe NVDEC 25-47%, 1.73ms service | av1 host cliff (14-16x collapse, engine idle); h265 sync readback floor; h265 surface non-monotonic; NVENC capped (F14) or software (F5) |
| d12 | av1 decode/surface/sustained linear to 4K at low duty; av1 sustained engine share 49-90% (393us gap-implied) | h265 4K parse wall; NVENC h264 physics-saturated in-leg (F3); h265 encode no HW path |
| vk | all legs 2.8-3.7x for 4x pixels, top duty sustained h265 ~22% mean | NVENC driver floor (F4) plus ladder cap (F8); no wall in sight at 4K |

NVDEC duty-100% is unobserved at any tested resolution on any tier; host, parse, and cap walls arrive first. Method: release single-leg runs with dmon sampling against scratch res vectors; dmon columns are window means, peaks carry the signal (ladder). The ladder's d12 h264 session-build wall predates the 5c0eaeb prime.

## Software

### Decode, fps

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

rv has no legs: the ffmpeg/gpl backends need linkable libav*, absent from the binaries-only build (Windows link requires `FFMPEG_LIB_DIR` per the sw build script).

### Encode, fps

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

No two-sided legs. OpenH264 runs rv-only; see One-sided reference.

## One-sided reference, fps

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 653, h265 922, av1 722 (new surface content).
- Transcode via rv, no ffmpeg counterpart: d3d11 454, d3d12 622 (quiet median/mean, noisy legs).
- h264 openh264 via rv, absent from the ffmpeg build: 256 (nasm ASM path; cycled seeds alias pairwise, distinct-content probe 145).
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
11. d12 h265 surface C-sample tiebreak: thin GREEN (+1.7%, spread 4.3% > margin); one quiet full run firms it.
12. d12 av1 sustained third sample: 15% A/B spread (readback-noise copy 1507 vs 1148us) keeps the lull 2305; a third quiet run graduates the mean.

## Comparability notes

- Content: every pair shares bytes except h264 sync/downloaded (gold `_ip` vs rv synth: adjacent) and dropped h264-IP SW legs. Encode dumps alias pairwise (`seed | 1`) and are frozen gold-paired. Resident pairs match shape (surface); downloaded pairs match shape (sync, sustained-vs-downloaded); sustained-vs-resident favors gold (rv hands CPU frames back).
- vp9 gold: the 120f file supersedes the 30f 194 (same bytes, startup amortizes); short vp9 gold runs understate. cuvid-resident and d3d11va-downloaded golds use different decoders: not comparable to each other.
- Settings: encode legs do not match settings or bitrate and carry no quality claim.
- Structural: AV1 all-intra NVENC refused (IP g4 stands in); aom cpu10 errors both sides (cpu-used range 0-8, crate pins cpu8); h264-synth gold download impossible in this build (d3d11va corrupt-frame, cuvid+hwdownload unimplemented).
- d12 trim adjudication (quiet): engines improved on all syncs (av1 649-669 vs 675-685 baseline); av1 sync wall 1073.8 lands between baseline 1031 and claim 1107 (submit 100 -> 80us tracks the wall 1:1, claim retired as submit-luck); hevc surface submit draws 29-53us (luck is the spread).
- Cross-OS: the Linux sheet differs in build, corpus, and vectors: same counting discipline only.
