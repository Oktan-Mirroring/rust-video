<!-- source: AOMedia AV1 specification, 681pp PDF; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# AV1

Tier 1 throughout. All cites are the AOMedia AV1 specification.
Anchors L4.0 and L5.1 cross-checked against an independent second source.

## Profiles (Annex A.2, S6.4.1)

seq_profile 0 Main: 8/10-bit, 4:2:0, monochrome allowed. 1 High: 8/10-bit
4:4:4, no monochrome. 2 Professional: 8/10-bit 4:2:2 plus monochrome,
12-bit any of 4:2:0/4:2:2/4:4:4 plus monochrome. Decoder tiers are
cumulative: High decodes up to 1, Professional up to 2.

## Levels (Annex A.3)

seq_level_idx 0-23 maps to 2.0-7.3 by X=2+(idx>>2), Y=idx&3. Defined
levels: 2.0, 2.1, 3.0, 3.1, 4.0, 4.1, 5.0-5.3, 6.0-6.3. Idx 24-30 are
reserved; idx 31 carries maximum parameters with no level constraints.

| idx | Level | MaxPic | Max W | Max H | DispRate | DecRate |
|---|---|---|---|---|---|---|
| 0 | 2.0 | 147456 | 2048 | 1152 | 4423680 | 5529600 |
| 1 | 2.1 | 278784 | 2816 | 1584 | 8363520 | 10454400 |
| 4 | 3.0 | 665856 | 4352 | 2448 | 19975680 | 24969600 |
| 5 | 3.1 | 1065024 | 5504 | 3096 | 31950720 | 39938400 |
| 8 | 4.0 | 2359296 | 6144 | 3456 | 70778880 | 77856768 |
| 9 | 4.1 | 2359296 | 6144 | 3456 | 141557760 | 155713536 |
| 12 | 5.0 | 8912896 | 8192 | 4352 | 267386880 | 273715200 |
| 13 | 5.1 | 8912896 | 8192 | 4352 | 534773760 | 547430400 |
| 14 | 5.2 | 8912896 | 8192 | 4352 | 1069547520 | 1094860800 |
| 15 | 5.3 | 8912896 | 8192 | 4352 | 1069547520 | 1176502272 |
| 16 | 6.0 | 35651584 | 16384 | 8704 | 1069547520 | 1176502272 |
| 17 | 6.1 | 35651584 | 16384 | 8704 | 2139095040 | 2189721600 |
| 18 | 6.2 | 35651584 | 16384 | 8704 | 4278190080 | 4379443200 |
| 19 | 6.3 | 35651584 | 16384 | 8704 | 4278190080 | 4706009088 |

| idx | HdrRate | Mbps-M | Mbps-H | CR-M | CR-H | Tiles | TileCols |
|---|---|---|---|---|---|---|---|
| 0 | 150 | 1500 | - | 2 | - | 8 | 4 |
| 1 | 150 | 3000 | - | 2 | - | 8 | 4 |
| 4 | 150 | 6000 | - | 2 | - | 16 | 6 |
| 5 | 150 | 10000 | - | 2 | - | 16 | 6 |
| 8 | 300 | 12000 | 30000 | 4 | 4 | 32 | 8 |
| 9 | 300 | 20000 | 50000 | 4 | 4 | 32 | 8 |
| 12 | 300 | 30000 | 100000 | 6 | 4 | 64 | 8 |
| 13 | 300 | 40000 | 160000 | 8 | 4 | 64 | 8 |
| 14 | 300 | 60000 | 240000 | 8 | 4 | 64 | 8 |
| 15 | 300 | 60000 | 240000 | 8 | 4 | 64 | 8 |
| 16 | 300 | 60000 | 240000 | 8 | 4 | 128 | 16 |
| 17 | 300 | 100000 | 480000 | 8 | 4 | 128 | 16 |
| 18 | 300 | 160000 | 800000 | 8 | 4 | 128 | 16 |
| 19 | 300 | 160000 | 800000 | 8 | 4 | 128 | 16 |

Bitrates in kbps. seq_tier 1 (HighMbps/HighCR) signals only at L4.0 and
up. Tiles/s at most MaxTiles x 120. Level cap on resolution is
16384x8704; syntax absolute max is 65536x65536; minimum frame is 16x16.

## Color (S5.5.3, S6.4.2)

12-bit only in profile 2. 4:2:2 subsampling only at 12-bit.
Monochrome only in profiles 0 and 2. Plane count is 1 for monochrome,
3 otherwise.

## Quirks

Film grain (S6.8.20, S7.18) synthesizes onto output arrays after decode
and is never stored to references; absent grain parameters require
bit-identical output. Superres (S5.9.8, S6.8.7) scales horizontally
only with denominator 9-16 over 8; level constraints use the upscaled
width; rightmost tile minimum width is 128 with superres, 64 without;
each tile covers at most 4096x2304. Switch frames (type 3, S6.8.2)
overwrite all references without forcing intra. Eight reference slots
(S4); no HEVC-style DPB size table. Parallelism guard:
MaxTileSize x NumHeaders x TPden / TPnum at most 588251136 (Annex A.3).
Max luma tile width 4096 samples (S4).

## OBU (S5.3.2, S6.2.2)

Header: forbidden bit, 4-bit type, extension flag, size flag, reserved.
Types: 0 reserved, 1 sequence header, 2 temporal delimiter, 3 frame
header, 4 tile group, 5 metadata, 6 frame, 7 redundant frame header,
8 tile list, 9-14 reserved, 15 padding. Extension carries 3-bit temporal
id plus 2-bit spatial id (S5.3.3).

## Decoder model (S5.5.4-5, Annex E)

Operating parameters per operating point carry buffer delays, bitrate,
and buffer size. Conformance is the Annex E smoothing-buffer model with
decode/output timing and overflow/underflow rules (E.6). MaxHeaderRate
semantics beyond the table are unmapped.
