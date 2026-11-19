<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-11-19; author: rust-video -->
# Goldens

93 committed hash files pin decode output for four codecs. Each file
traces to exactly one required-matrix line. A tier claiming a codec
matches those hashes byte-exact or declines with a typed error. No
third outcome exists: silent wrong output and panics both fail.

The binding matrix lives in `crates/rust-video-harness/src/bin/golden_gen.rs`.
`just goldens --list-required` prints every line, its clause, and how
it resolves (synth recipe, pinned fetch, or BLOCKED with a reason).
The gate fails on any non-blocked line without a vector.

## Content suite

Two canonical clips feed every encode. Both are 120 frames of
1080p30 from lavfi recipes, and `just goldens --synth` proves each
deterministic before building anything: it renders the clip twice,
compares SHA-256 hashes, and rejects the recipe on mismatch.

- `motion`: `testsrc=s=1920x1080:r=30:d=4`. Moving pattern; exercises
  inter prediction.
- `noise`: `nullsrc=s=1920x1080:r=30:d=4,geq=random(1)*255`.
  Adversarial noise; stresses entropy coding and rate control.

Matrix encodes keep all 120 frames and vary only scale, pixel format,
encoder, and container. Anchors and noise lines run at 1080p, size
lines at 4K, and the remaining lines at 640x360. Ultrafast-class
presets throughout: vectors exercise syntax, not quality. Encoder
threads stay pinned at 8 in every recipe, since output bits vary
with thread layout and committed goldens must verify on any core
count. There are no movie vectors anywhere; the 320x240 decode corpus
and the 1080p bench corpus are separate concerns and untouched.

## Vectors

`just vectors` fetches the single pinned vector the matrix names but
no encoder produces: `vp90-2-20-big_superframe-01.webm` (SHA-1
`f97088c7359fc8d3d5aa5eafe57bc7308b3ee124`), real encoder-emitted
superframes, remuxed stream-copy to IVF and kept as
`corpus/vp9/vp90-2-20-big_superframe-01.ivf`. Everything else is
synthesized by `just goldens --synth`: lavfi encodes plus byte-level
crafts derived from them (Annex B surgery, IVF/OBU edits,
concatenations). Vectors are never committed; `corpus/` is gitignored.

Three crafts carry verified constraints worth knowing before touching
them. Set-id gaps keep PPS id 0 active with SPS id 2 plus an inert
PPS id 3, because libav emits MB errors on bit-exact PPS-id
renumbers. Frame-number gaps flip the SPS gaps flag on (x264 forbids
gaps) and shift subsequent numbers forward by 2, since gap pictures
occupy DPB slots and this stream's level-limited DPB holds 4.
Truncation vectors pair complete sets with a poison tail element,
because clean prefixes decode instead of failing.

## Golden files

`crates/rust-video-harness/goldens/<codec>/<line>.sha256`. One lowercase
hex SHA-256 per decoded frame in output order, over raw frame bytes at
the probed pixel format. The v1 header carries the vector path
(forward slashes), the expectation, the ffmpeg version, the probe
line, the frame count, and the recipe. Three expectations exist:

| expect | meaning |
|---|---|
| `match` | tiers decode byte-exact |
| `either <reason>` | tiers decode byte-exact or decline cleanly |
| `decline <reason>` | tiers must return `Err`; zero hashes follow |

Decline goldens assert mechanically: the generator refuses to write
one unless ffmpeg itself fails the vector, and runners fail any
decode that succeeds. Either lines cover baseline accept-or-decline
clauses (extra profiles, depths, formats, interlaced) plus two
tier-relative findings: ffmpeg tolerates slices-before-sets while the
vulkan tier strictly declines, and swapped-slice ASO decodes with
logged errors under ffmpeg.

## Generation and gate

`just goldens --synth` proves the clips, encodes every recipe,
derives every craft, oracle-verifies each vector, then writes goldens
fresh-path (new tree, swap, delete old). Oracle verification runs
ffmpeg with `-xerror` and requires at least one decoded frame: valid
vectors must decode cleanly, decline vectors must fail. Matrix
vectors that fail stop generation loudly; only non-matrix fetched
extras skip. The gate then matches each matrix line to its vector
stem and fails listing every uncovered line with its fix. Encoder
versions also shift bits, so the header records the ffmpeg version
that produced each golden; a toolchain upgrade means regenerating
and recommitting the set in the same change. Blocked
lines print with reasons and never fail: 20 today across encoder gaps
(no MMCO/PAFF/BLA/long-term/Rext/SVC options here), craft gaps
(switch frames, show-existing, non-uniform tiles, explicit spacing,
deblock override, conformance-adjacent work), and oracle findings
(ASO decodes with errors, no encoder emits tile-group OBUs, this
libaom lacks monochrome/superres/loop-filter/compound toggles).

## Runners

Both end-to-end suites walk the goldens directory. No per-case test
function exists; the matrix plus the corpus is the test list. Each
line resolves its vector from the `# vector:` header (re-rooted under
`RUST_VIDEO_CORPUS` when set), decodes through a fresh decoder, and
enforces its expectation. Fresh decoders per line are load-bearing:
parser, surface, and DPB state visibly leaks across vectors on shared
instances.

The sw suite (`cargo test -p rust-video-sw --features ffmpeg --test
e2e`, needs linkable libavcodec/libavutil) covers h264, h265,
vp9, and av1. MP4 demuxes to samples, IVF to frames, Annex B and OBU
ride as one packet each. Match lines compare every hash; either lines
compare on success and log declines; decline lines fail on success.

The vulkan suite (`cargo test -p rust-video-vulkan --test e2e`, needs
`./corpus` and a GPU) covers h264 and h265 over raw Annex B, remuxing
mp4 vectors through the codec bitstream filter with a workdir cache.
It enforces match strictly on the universal subset (8-bit 4:2:0
non-interlaced lines; raw Annex B reports no interlace signal and
every such vector is progressive by construction). Off-subset lines
assert decode-or-decline without hash comparison, since readback
converts everything to 8-bit 4:2:0. VRAM exhaustion declines loudly
with live budget numbers. Three lines sit on the `VULKAN_TIER_GAPS`
skip list with the tier-bug reason each: frame-number gaps (wrong
frames then stall), Baseline profile (one frame repeated; Main
matches), and VUI-absent output order (VUI carries
`max_num_reorder_frames`, which the tier mis-defaults). The sw runner
still pins all three; remove entries as the tier is fixed.
