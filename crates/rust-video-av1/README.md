# rust-video-av1

AV1 grammar shared by every decode tier: OBU splitting, sequence and frame
headers, and tile assembly, generic over the tier error type.

## Decode and encode

Hardware decode ships in the platform tiers (`../rust-video-d3d11`,
`../rust-video-d3d12`, `../rust-video-vulkan`); software decode and encode
ship in `../rust-video-sw`, behind its `ffmpeg` feature. The facade exposes
this crate as the `av1` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
