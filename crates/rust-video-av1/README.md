# rust-video-av1

AV1 codec namespace: the `Av1` unit struct and nothing else. No modules, no dependencies, no behavior.

## Decode and encode

AV1 software decode and encode ship in `../rust-video-sw`, behind its `ffmpeg` feature. The facade exposes this crate as the `av1` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
