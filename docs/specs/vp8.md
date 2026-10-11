<!-- source: RFC 6386 (VP8 data format and decoding guide); tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# VP8

Tier 1 throughout, single source: RFC 6386.

VP8 has no profiles, no levels, and no tiles. The frame-tag version
(0-3) selects the reconstruction and loop filter only; decoding is
unaffected (S9.1). Pixel format is 8-bit YUV 4:2:0 exclusively (S1).
Maximum frame is 16384x16384 from 14-bit dimensions plus 2-bit scale
(S9.1). Parallelism is 1/2/4/8 token partitions (S19.1, S20). There is
no superframe syntax; unshown frames use show_frame 0 (S9.1).
