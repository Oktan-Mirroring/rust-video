<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Testing

Three tiers. What the behaviour reaches picks the tier.

| Behaviour | Tier | Home | Command |
|---|---|---|---|
| arithmetic, parsing, hash and report logic | unit | beside the module | `just test` |
| decode and encode against vectors and goldens | e2e | `crates/rust-video/tests/e2e.rs` | `just e2e` |
| rate-control and quality scoring | e2e | harness report, planned | `just e2e` |
| device encoder or link behaviour | device | planned | planned |

`just ci` stays network-free: it runs unit gates only. Pixel proof lives in
`just e2e`, which needs the vector corpus from `just vectors` and fails loudly
without it. Corpus-gated tests compile out when the corpus is absent (the
facade build script sets `have_corpus`); nothing skips silently.

`docs/testing/manifest.toml` registers every capability. `docs/testing/vectors.md`
names the vector sources and their verification state. `docs/testing/oracles.toml`
maps each behaviour id to the test that proves it.
