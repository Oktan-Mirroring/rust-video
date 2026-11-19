# rust-video-h264

H.264 grammar: sets, slices, and picture description. One parser behind
every tier, generic over the tier error; tier caps arrive as gates.

## Decode and encode

H.264 software decode and encode ship in `../rust-video-sw`, behind its
`ffmpeg` and `h264-openh264` features. The facade exposes this crate as
the `h264` feature.

## Docs

Codec matrix in `../../README.md`, backend policy in `../../docs/matrix.md`.
