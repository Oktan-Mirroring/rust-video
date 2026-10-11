# rust-video

Safe Rust interface over realtime video encode and decode. Each codec and
tier carries the fastest implementation that holds the frame budget: platform
hardware, tuned C behind an isolated sys crate, or pure Rust once it wins on
the clock. One dependency for live frames.

Status: scaffold. The workspace compiles and the gates run, but no backend
encodes or decodes anything yet.

## Scope

In: realtime frame encode/decode for H.264, H.265, AV1, VP9, and AV2, the
platform tiers that accelerate them, and the livepack frame packing between the
two.

Out: archival demuxing, subtitles, filters, and audio. A general media framework
already exists; this crate stays on the live path.

## Layout

| Path | Holds |
|---|---|
| `crates/rust-video` | Facade: codec and tier enums |
| `crates/rust-video-h264` … `-av2` | Codec backends, one crate each |
| `crates/rust-video-vulkan` … `-mediacodec` | Platform tiers, one crate each |
| `crates/rust-video-livepack` | Livepack frame packing |
| `crates/rust-video-harness` | Vector suites and golden oracles |
| `crates/rust-video-caps` | Codec profiles, levels, and caps as types |
| `docs/` | Agent routing and design docs |
| `scripts/` | Vector fetch and probes |
| `corpus/` | Vector corpus, gitignored, via `just vectors` |

## Build

```bash
just build    # cargo build --workspace
just check    # clippy, warnings denied
just test     # cargo test --workspace
just doc      # API docs, no dependency docs
just ci       # format check plus all of the above
```

Windows compiles in CI only; see `.github/workflows/ci.yml`.

## References

- ITU-T H.264 (01/2012) and H.265 V10 (07/2024): profiles, tiers, levels.
- AOMedia AV1 specification: profiles, levels, OBU, decoder model.
- AOMedia AV2 specification v1.0.0: <https://av2.aomedia.org/v1.0.0/>
- VP9 v0.7 specification and levels: <https://www.webmproject.org/vp9/levels/>
- RFC 6386: VP8 data format.

## License

MIT or Apache 2.0, at your choice. See `LICENSE-MIT` and `LICENSE-APACHE`.
