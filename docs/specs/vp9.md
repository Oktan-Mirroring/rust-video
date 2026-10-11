<!-- source: VP9 bitstream spec v0.7 plus webmproject.org/vp9/levels/; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# VP9

Tier 1 throughout. Normative caps come from the v0.7 specification plus
the webmproject levels page, fetched 2026-10-11.

## Profiles (spec S7.2, S6.2)

P0: 8-bit 4:2:0. P1: 8-bit 4:2:2/4:4:0/4:4:4. P2: 10/12-bit 4:2:0. P3:
10/12-bit 4:2:2/4:4:0/4:4:4. Signaled via profile_low/high_bit (S6.2)
and ten_or_twelve_bit (S6.2.2). Levels cover P0 and P2 only.

## Levels (levels page)

Columns: luma sample rate, luma picture size, bitrate (kbps), CPB
(kbits, TBD on the page from 5.2 up), MinCR, max tiles, min alt-ref
distance, max reference frames, max width/height.

| Level | SampleRate | PicSize | BR | CPB | MinCR | Tiles | AltRef | Refs | MaxDim |
|---|---|---|---|---|---|---|---|---|---|
| L1 | 829440 | 36864 | 200 | 400 | 2 | 1 | 4 | 8 | 512 |
| L1.1 | 2764800 | 73728 | 800 | 1000 | 2 | 1 | 4 | 8 | 768 |
| L2 | 4608000 | 122880 | 1800 | 1500 | 2 | 1 | 4 | 8 | 960 |
| L2.1 | 9216000 | 245760 | 3600 | 2800 | 2 | 2 | 4 | 8 | 1344 |
| L3 | 20736000 | 552960 | 7200 | 6000 | 2 | 4 | 4 | 8 | 2048 |
| L3.1 | 36864000 | 983040 | 12000 | 10000 | 2 | 4 | 4 | 8 | 2752 |
| L4 | 83558400 | 2228224 | 18000 | 16000 | 4 | 4 | 4 | 8 | 4160 |
| L4.1 | 160432128 | 2228224 | 30000 | 18000 | 4 | 4 | 5 | 6 | 4160 |
| L5 | 311951360 | 8912896 | 60000 | 36000 | 6 | 8 | 6 | 4 | 8384 |
| L5.1 | 588251136 | 8912896 | 120000 | 46000 | 8 | 8 | 10 | 4 | 8384 |
| L5.2 | 1176502272 | 8912896 | 180000 | - | 8 | 8 | 10 | 4 | 8384 |
| L6 | 1176502272 | 35651584 | 180000 | - | 8 | 16 | 10 | 4 | 16832 |
| L6.1 | 2353004544 | 35651584 | 240000 | - | 8 | 16 | 10 | 4 | 16832 |
| L6.2 | 4706009088 | 35651584 | 480000 | - | 8 | 16 | 10 | 4 | 16832 |

Level 0 means undefined. Max references drop 8 (L1-4) to 6 (L4.1) to 4
(L5 and up).

## Geometry

Syntax frame dimensions are 16-bit (S6.2, S7.2.3), up to 65536 per
axis; the conformance cap is the level's max dimension, 16832 at L6.2.
Inter frames must satisfy 2xW >= RefW and W <= 16xRefW (S7.2.5).

## Tiles (S6.2.13-14, S3)

Column tiles are bounded by tile width 4-64 superblocks (256-4096 px);
rows run 1-4 via tile_rows_log2; the level caps column tiles at 1-16.
frame_parallel_decoding_mode is signaled per frame (S6.2).

## Superframe (S5.26, Annex B)

Optional multi-frame chunk with an index of at most 34 bytes, marker
0b110, and at most 8 frames (frames_minus_1 is 3 bits). Presence is
tested on the chunk's final byte (B.4); a plain coded frame's final
byte must not contain the marker.

## Scalability (RFC 9628)

The v0.7 bitstream has no SVC syntax. Scalability rides the reference
structure (8 buffers, RFC 9628 S3) plus RTP signaling (S4.2): up to 8
spatial layers, per-layer dimensions in the SS packet, picture-group
dependencies. The 2013 draft-grange snapshot carries no caps and is not
a normative source.
