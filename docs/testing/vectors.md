<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Vector sources

The corpus lives in `corpus/<codec>/`, fetched by `just vectors`, never
committed. Each source below carries its verification state.

| Codec | Source | State |
|---|---|---|
| VP9, VP8 | libvpx `test/test-data.sha1` manifest plus `downloads.webmproject.org/test_data/libvpx/` files, SHA-1 verified | fetched, hashes verified |
| H.264, H.265 | ITU `wftp3` JCT-VC bitstream exchange directory | manual drop-in, reachability and file URLs unverified |
| raw YUV | Xiph `media.xiph.org/video/derf/` | manual drop-in, reachability and file names unverified |
| AV1 | AOM test clips | manual drop-in, source unverified |
| AV2 | AVM corpus | manual drop-in, source unverified |

Manual drop-in: place vector files under `corpus/<codec>/` (`h264`, `h265`,
`av1`, `vp9`, `vp8`, `av2`); unknown directory names are ignored by the suite
scan. A fetched source graduates from manual only after its exact file URLs
return 200 and its hashes verify.

Fetch normalization: `vp80-03-segmentation-01..04.ivf` ship FourCC `I420`
over VP8 payloads; `scripts/fetch-vectors.py` rewrites bytes 8-11 to `VP80`
after SHA-1 verification.
