# rust-video-av2

AV2 codec namespace: the `Av2` unit struct and nothing else. No backend ships in any tier yet.

## Decode and encode

Nowhere. The candidate is `rusty_av2d` 0.2.11, gated behind measurement per `../../docs/matrix.md`, with no promise until it holds the frame budget. The facade exposes this crate as the `av2` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
