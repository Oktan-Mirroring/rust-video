# rust-video-vp9

VP9 codec namespace: the `Vp9` unit struct and nothing else. No modules, no dependencies, no behavior.

## Decode and encode

VP9 software decode and encode ship in `../rust-video-sw`, behind its `ffmpeg` feature. The facade exposes this crate as the `vp9` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
