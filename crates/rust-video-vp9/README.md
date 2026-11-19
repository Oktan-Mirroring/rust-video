# rust-video-vp9

VP9 grammar shared by every tier: uncompressed frame headers plus tile assembly.

## Decode

`Ingest` parses frame headers and splits tiles, generic over the tier error type: each `Fault` converts into it through `From`. Tiers map faults into their own errors and build submit structures locally.

## Encode and software decode

VP9 software decode and encode ship in `../rust-video-sw`, behind its `ffmpeg` feature. The facade exposes this crate as the `vp9` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
