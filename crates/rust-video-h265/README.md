# rust-video-h265

H.265 codec namespace: the `H265` unit struct and nothing else. No modules, no dependencies, no behavior.

## Decode and encode

H.265 software decode and encode ship in `../rust-video-sw`, behind its `ffmpeg` and `gpl` features. The facade exposes this crate as the `h265` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
