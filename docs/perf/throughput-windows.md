# Windows throughput vs FFmpeg

d11 h264 surface runs 5429 fps against gold 1461 (+272%) on identical bytes; d12 h264 sustained reads 2503 (+79%) on the primed steady method. Encode inverts 6/6 on shared noise (d11 h264 388 vs 92), pace only, settings and quality unmatched. F7 is dead: the 96 fps floor came from a C-only openh264 link, the nasm build runs 256.

rv tree 4e92a7c (+ held mc/va diffs, outside the Windows binaries). verify2 quiet medians/means (2026-11-20) stand where the tree did not move them; post-fix rounds refresh the DX sync/surface/sustained cells (`*` marks below-protocol statistics, kinds in Method). Gold medians one era (4e92a7c): 1 discarded warmup, 3 timed HW runs, 5 timed SW runs; `_ip` confirms within 6% of rebaseline.

## Environment

| Item | Value |
|---|---|
| OS | Windows 11 Pro N, 10.0.26200 |
| CPU | Ryzen 9 9950X3D, 16-core, 4.3 GHz pinned, Ultimate Performance plan |
| FFmpeg | 9.0-full_build-www.gyan.dev (binaries only); +libx264 +libx265 +libvpx +libsvtav1 +libdav1d +cuvid +nvenc +d3d11va; no libopenh264, no linkable libav* |
| rustc | 1.100.0 (0ef1bc386 2026-11-09) |
| NVIDIA GPU | GeForce RTX 5080, driver 621.24, Vulkan 1.4.357, idle 31C unrestrained |
| AMD GPU | Radeon integrated, Vulkan 1.3.287 (present, unselected) |
| rv tree | 4e92a7c |

## Method

- rv capture: `just benches-release` (green proof), then per tier `cargo test --release -p rust-video-<tier> --test throughput -- --nocapture` with tier `rust-video-d3d11`, `rust-video-d3d12`, `rust-video-vulkan`, and `rust-video-sw --features h264-openh264`. d11 grades median-of-3, d12/vk/sw mean-of-2. Quiet box: no builds, CPU idle, GPU near idle; discard samples taken under spikes and re-run them.
- `*` marks a below-protocol rv statistic: commit best (median pending), isolated pace (full-binary pending), or single sample; each table names the kind. The re-grade round in Gaps graduates them.
- Isolated legs run the same binary filtered to the leg, 3x, median. Full-binary collapses d11 av1 decode runs to ~100fps and discounts clean ones ~20%; livepack parallel alloc-churn costs 16-29%; d12 h264/av1 sustained and h264/av1 sync have no full-binary sample on the steady method yet.
- Sustained legs run one 120-frame IP pass per overlap pump. d11 all four and d12 all three prime untimed (steady state); vk times the cold pass. d11 counts frames then drops them (byte parity stays in the overlap tests); frame counts exact, queue drops zero.
- Sync/surface legs: d11 warms 100ms with untimed trailing flush; d12 sync primes 4 passes; d11/d12 print submit/engine/copy phase medians.
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
| d3d11 h264 surface | 1461 | 5429* | +271.6% |
| d3d12 h264 surface | 1461 | 4955 | +239.2% |
| vulkan h264 surface | 1461 | 3724 | +154.9% |
| d3d11 h265 surface | 1120 | 1962* | +75.2% |
| d3d12 h265 surface | 1120 | 2009 | +79.4% |
| vulkan h265 surface | 1120 | 1479 | +32.1% |
| d3d11 av1 surface | 819 | 1324* | +61.7% |
| d3d12 av1 surface | 819 | 1330 | +62.4% |
| vulkan av1 surface | 819 | 1270 | +55.1% |
| d3d11 vp9 surface | 250.2 | 268 | +7.1% |

Every pair shares bytes: gold decodes the 120x single-frame concat. d3d11 vp9 decode is NVDEC both sides (ours via the D3D11 VP9 decoder profile); software MFT applies to the VP9 encoder only. `*` = commit best, median pending (observed ranges: h264 4913-5453, h265 1767-1962, av1 1322-1324). h265 gold 1120 pools two medians (1074/1167, 18% pooled spread).

Sustained overlap, 120-frame IP both sides:

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| vulkan h264 sustained | 1398 | 3021 | +116.1% |
| vulkan h265 sustained | 1293 | 1769 | +36.8% |
| vulkan av1 sustained | 1197 | 2014 | +68.3% |
| d3d11 h264 sustained | 1398 | 1969 | +40.8% |
| d3d11 h265 sustained | 1293 | 1730 | +33.8% |
| d3d11 av1 sustained | 1197 | 2113 | +76.5% |
| d3d11 vp9 sustained | 1017 | 1494 | +46.9% |
| d3d12 h264 sustained | 1398 | 2503* | +79.0% |
| d3d12 h265 sustained | 1293 | 2078 | +60.7% |
| d3d12 av1 sustained | 1197 | 2465* | +105.9% |

Every leg shares gold's exact bytes; gold stays resident while rv legs hand CPU frames back: each percent favors gold. `*` = isolated pace, full-binary pending. Downloaded pairs per codec are the shape-matched d3d legs: gold 810/765/825/792 against rv d3d11 sustained 1969/1730/2113/1494 (h264/h265/av1/vp9), gold with a 373 MB file write, rv with CPU frames in hand.

Sync, downloaded both sides (`†` = adjacent bytes, unpercented):

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 † | 810 | 3870* | adjacent |
| d3d11 h265 | 873 | 1436* | +64.5% |
| d3d11 av1 | 837 | 1047* | +25.1% |
| d3d11 vp9 | 262.8 | 244 | -7.2% |
| d3d12 h264 † | 810 | 2648* | adjacent |
| d3d12 h265 | 873 | 1409 | +61.4% |
| d3d12 av1 | 837 | 1107* | +32.3% |
| vulkan h264 † | 810 | 2853 | adjacent |
| vulkan h265 | 873 | 1431 | +63.9% |
| vulkan av1 | 837 | 1226 | +46.5% |

Gold downloads the 120x single-frame concat to a file; h264 rows show the `_ip` downloaded number (gold cannot download synth bytes). `*` = commit best (d11 h264/h265, observed ranges 3597-3870 and 1243-1436), isolated pace (d12 h264/av1), or one isolated sample (d11 av1). d11 av1 decode stays isolated-graded: full-binary runs collapse it to ~100fps and discount clean runs ~20% (contention, reproduced across rounds).

### Encode, fps

Gold runs intra CQP qp 28 (AV1 IP g4) on each tier's own noise dump; rv runs MFT/Video-Encode/VkVideo defaults on the same frames. Settings differ: percents diagnose only, no quality claim.

| Leg | FFmpeg | rv | Delta |
|---|---|---|---|
| d3d11 h264 | 92.1 | 388 | +321.3% |
| d3d11 h265 | 141.6 | 690 | +387.3% |
| d3d11 av1 | 157.8 | 547 | +246.6% |
| d3d12 h264 | 92.4 | 103 | +11.5% |
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

- d3d11 surface encode via rv, no ffmpeg counterpart: h264 686, h265 957, av1 724.
- Transcode via rv, no ffmpeg counterpart: d3d11 424, d3d12 622.
- h264 openh264 via rv, absent from the ffmpeg build: 256 (nasm ASM path; cycled seeds alias pairwise, distinct-content probe 145).
- d3d11 vp9 software encode via rv, no NVENC VP9: 69.
- livepack via rv, no ffmpeg counterpart (isolated singles): TsMux 502432, TsDemux 502015, FragMux 166447.

## Floors

Proof tags: perf-final = `.tmp/perf-final.md`, verify = `.tmp/perf-verify.md`, verify2 = `.tmp/perf-verify2.md`, ladder = `.tmp/engine-ladder.md`, roofline = `.tmp/roofline.md`; bare hashes are commits.

| # | Claim | Proof | Status |
|---|---|---|---|
| F1 | d11 h264 surface demonstrated pace ~285us | perf-final §F1; verify2 +24%; e93b61d best 5429 | VACATED, leg GREEN |
| F2 | d12 h264 surface quoted 2112 silicon max | perf-final §F2 (rejected: B-sample 2243 beats it); verify moot | MOOT, leg GREEN |
| F3 | d12 h264 encode 9.65ms physics (8ms NVENC entropy + ~1ms) | roofline; verify2 spot flat; ladder 103/103 | CONFIRMED |
| F4 | vk h265 encode 4.3ms driver floor, content-independent | roofline; verify2 spot; ladder 230.6/230.6 | CONFIRMED |
| F5 | d11 vp9 encode software MFT floor (fastest QvS landed) | roofline; verify2 spot; ladder 69/69 zero NVENC | CONFIRMED |
| F6 | d11 h264 sustained 1103-stands walk (MFT wait + locality + pack + drains) | roofline; verify VACATED (leg 1430 past 1142); verify2 +30%; ada9dea 1969 | VACATED, leg GREEN |
| F7 | sw h264 encode 10.8ms parallel floor (threads scale 1-4, lib caps at 4) | perf-final §F7 (C-only build); KILLED: nasm build 256 (sw-base logs, 5ab612f, thread sweep); openh264-sys2 build.rs has no rerun-if (stale C-only link) | DEAD (mechanism was a build artifact) |
| F8 | vk h264 encode ladder-capped (~535 fastest rung vs 750; API floor 1.22ms) | perf-final §F8; verify2 spot | CONFIRMED |
| F9-11 | d12 sync >= surface + 311us bus-min - handoff (446/761/1005us walls) | perf-final §F9-11; verify REOPENED (corrected steady arithmetic lands under budget); verify2 VACATED-MOOT (prime+pool, +72/+28/+10%) | VACATED-MOOT, legs GREEN |
| F12 | d12 h264 sustained engine-alone 707 > 657us budget | perf-final §F12 + CORRECTION (707 is span, not service); verify2 spot; SUPERSEDED by 5c0eaeb (2503, +65%) | VACATED (method retired); gap re-derivation unconfirmed, no quiet run |
| F13 | vk av1 sustained ~2100 zero-copy asymptote (slope 0.15, copy 187us) | perf-final §F13; verify2 spot (2016 under ~2100) | CONFIRMED |
| F14 | d11 MFT Auto cap (byte-exact legacy; faster settings barred by goldens) | perf-final §F14; verify2 spot (7 legs flat); adapt-d11 corroborates ±4% | CONFIRMED |
| F15 | vk h264 surface phased sums 239-244us vs 217.4 budget | verify §F15; verify2 spot | CONFIRMED |
| F16 | vk h265 surface sums 605-614us vs 505.1 budget | verify §F16; verify2 spot | CONFIRMED |
| F17 | vk h264 sustained service gaps 280-286us vs 247.8 (max 3571 vs 4036) | verify §F17; verify2 spot | CONFIRMED |
| F18 | vk h265 sustained gaps 510-517us vs 380.1 | verify §F18; verify2 spot | CONFIRMED |
| F19 | d11 h265 decode phases 75+679+61 = 815us vs 780.6 budget | verify §F19; steady best 1436 clears 1281 (+12%) | VACATED (derivation predates method; one full-binary sample under budget, re-grade confirms) |
| F20 | d11 h265 surface engine waits 572-589us vs 510.2 budget | verify §F20; steady best 1962 clears 1960 (+0.1%) | VACATED-EDGE (re-grade median decides; re-walk if RED) |
| F21 | d11 av1 surface waits 835-847us vs 695.4 budget | verify §F21; steady best 1324 RED -8%; new wall 755us undercuts the old wait derivation | STALE (re-walk owed) |
| F22 | d12 av1 surface engine 691-694us vs 677.1 budget (4 samples) | verify §F22; verify2 spot | CONFIRMED (surface unmeasured since 804624b) |

## Gaps

1. Quiet-box re-grade round on HEAD: median-of-3 d11 sync/surface, mean-of-2 d12 sync/sustained, full-binary; graduates every `*` cell. Recipe: Method rv capture.
2. d12 av1 sustained full-binary sample: decides permanent isolated grading (ladder saw 2200 isolated vs 1321 full on pre-fix code; post-fix full-binary unmeasured).
3. F21 re-walk: d11 av1 surface RED -8% at 1324; capture steady-state waits on the bench vector.
4. d12 h265 encode: no HW path on this adapter (ladder SKIP on 621.24).
5. d12 VP9: no picture builder (no sync/surface/sustained legs).
6. d11 vp9 surface-encode: no surface import.
7. d12 h265 4K: parse fails with `picture without first slice` on the ffmpeg-valid NVENC stream (ladder BLOCKED).
8. d11 av1 4K host cliff: 14-16x collapse at idle engine (phase re-probe on master).
9. d11 h265 surface 4K non-monotonic: 1686 beats 1440p 1070 (NAL-structure compare).
10. sw ffmpeg/gpl legs on Windows: need linkable libav* (`FFMPEG_LIB_DIR` per the sw build script).

## Comparability notes

- Content: every pair shares bytes except h264 sync/downloaded (gold `_ip` vs rv synth: adjacent) and dropped h264-IP SW legs. Encode dumps alias pairwise (`seed | 1`) and are frozen gold-paired. Resident pairs match shape (surface); downloaded pairs match shape (sync, sustained-vs-downloaded); sustained-vs-resident favors gold (rv hands CPU frames back).
- vp9 gold: the 120f file supersedes the 30f 194 (same bytes, startup amortizes); short vp9 gold runs understate. cuvid-resident and d3d11va-downloaded golds use different decoders: not comparable to each other.
- Settings: encode legs do not match settings or bitrate and carry no quality claim.
- Structural: AV1 all-intra NVENC refused (IP g4 stands in); aom cpu10 errors both sides (cpu-used range 0-8, crate pins cpu8); h264-synth gold download impossible in this build (d3d11va corrupt-frame, cuvid+hwdownload unimplemented).
- d12 surface/xcode predate the 804624b submit trim (same submit path, not re-measured after it; strictly less work per submit, so these read same-or-low).
- Cross-OS: the Linux sheet differs in build, corpus, and vectors: same counting discipline only.
