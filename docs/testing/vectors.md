<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Vector sources

The corpus lives in `corpus/<codec>/`, fetched by `just vectors`, never
committed. Each source below carries its verification state.

| Codec | Source | State |
|---|---|---|
| VP9, VP8 | libvpx `test/test-data.sha1` manifest plus `downloads.webmproject.org/test_data/libvpx/` files, SHA-1 verified | fetched, hashes verified |
| H.264, H.265 | ITU `wftp3` JCT-VC bitstream exchange directory, reachable | manual drop-in, file URLs unverified |
| raw YUV | Xiph `media.xiph.org/video/derf/`, reachable | manual drop-in, file names unverified |
| AV1 | AOM test clips | manual drop-in, source unverified |
| AV2 | AVM corpus | manual drop-in, source unverified |

Manual drop-in: place vector files under `corpus/<codec>/` (`h264`, `h265`,
`av1`, `vp9`, `vp8`, `av2`); unknown directory names are ignored by the suite
scan. A fetched source graduates from manual only after its exact file URLs
return 200 and its hashes verify.
