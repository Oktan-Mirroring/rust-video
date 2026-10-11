<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Goldens

One vector per option, plus the hot combinations. That is the rule, and
the coverage gate enforces it: `just goldens` fails on any required
option no vector exercises.

## The rule

Every codec option the types enforce gets at least one vector: each
profile, each pixel format, each bit depth the matrix names. On top of
that, hot combinations exercise the paths production actually hits
(1080p High, 4K Main 10, AV1 film grain). No combinatorial explosion:
one per option plus the hot list. A new option without a vector is an
incomplete change; the gate says so. A codec landing is not done
until every golden runs green against it; stale goldens get
regenerated in the same change.

Tests stay broad and data-driven. One runner walks the corpus and the
required matrix; no per-case test function exists to update. The matrix
plus the corpus is the test list.

## Generation

`just goldens` probes every vector with ffprobe, decodes with ffmpeg,
hashes each raw frame with SHA-256, and writes one `.sha256` per
vector. Goldens are committed; vectors are not. Each golden header
names its vector, the ffmpeg version, the probe line, and the synth
recipe for generated vectors, so anyone reproduces the set with `just
vectors` plus `just goldens --synth`.

`--synth` fills gaps the corpus cannot: profiles, depths, and formats
ffmpeg can encode go through lavfi test sources. Vectors no encoder
produces (VP8, AV2 today) come from fetched corpora only. Options
with no vector and no encoder stay explicitly BLOCKED, listed on every
run, never silent.

Regeneration follows fresh-path discipline: new tree, swap, delete
old. Vectors ffmpeg cannot probe or decode (the corpus keeps
deliberately invalid files) skip loudly by name; the floor is at
least one golden plus the skip count. A failed run leaves the
previous goldens intact and exits nonzero. Non-`.sha256` files in the
directory (this README's sibling) survive regens.
