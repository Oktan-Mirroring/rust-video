<!-- source: AOMedia AV2 specification v1.0.0 (28 May 2026); tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# AV2

Tier 1 throughout. All tables read from the v1.0.0 specification HTML,
fetched 2026-10-11.

## Profiles (Table A.1)

One toolset in v1.0.0: Main. idc 0 Main_420_10_IP0, idc 1
Main_420_10_IP1, idc 2 Main_420_10_IP2 (all 4:0:0/4:2:0, 8/10-bit);
idc 3 Main_422_10_IP1 adds 4:2:2; idc 4 Main_444_10_IP1 adds 4:4:4.
idc 5-30 are reserved; idc 31 is configurable. Interop points layer
0-2: IP0 single-layer up to IP2 with 8 layers.

## Bit depth (Table 6.3)

bit_depth_idc 0 selects 10-bit, 1 selects 8-bit; higher values are
reserved. No 12-bit profile exists in v1.0.0.

## Levels (Tables A.7-A.9)

seq_level_idx 0-21 map to 2.0-8.3; 22-30 are reserved; 31 carries
maximum parameters.

| idx | Level | MaxPic | MaxDim | DispRate | DecRate |
|---|---|---|---|---|---|
| 0 | 2.0 | 147456 | 640 | 4423680 | 5529600 |
| 1 | 2.1 | 278784 | 880 | 8363520 | 10454400 |
| 2 | 3.0 | 665856 | 1360 | 19975680 | 24969600 |
| 3 | 3.1 | 1065024 | 1720 | 31950720 | 39938400 |
| 4 | 4.0 | 2359296 | 2560 | 70778880 | 77856768 |
| 5 | 4.1 | 2359296 | 2560 | 141557760 | 155713536 |
| 6 | 5.0 | 8912896 | 4975 | 267386880 | 273715200 |
| 7 | 5.1 | 8912896 | 4975 | 534773760 | 547430400 |
| 8 | 5.2 | 8912896 | 4975 | 1069547520 | 1094860800 |
| 9 | 5.3 | 8912896 | 4975 | 1069547520 | 1176502272 |
| 10 | 6.0 | 35651584 | 9951 | 1069547520 | 1176502272 |
| 11 | 6.1 | 35651584 | 9951 | 2139095040 | 2189721600 |
| 12 | 6.2 | 35651584 | 9951 | 4278190080 | 4379443200 |
| 13 | 6.3 | 35651584 | 9951 | 4278190080 | 4706009088 |
| 14 | 7.0 | 142606336 | 19902 | 4278190080 | 4706009088 |
| 15 | 7.1 | 142606336 | 19902 | 8556380160 | 8758886400 |
| 16 | 7.2 | 142606336 | 19902 | 17112760320 | 17517772800 |
| 17 | 7.3 | 142606336 | 19902 | 17112760320 | 18824036352 |
| 18 | 8.0 | 530841600 | 38400 | 17112760320 | 18824036352 |
| 19 | 8.1 | 530841600 | 38400 | 34225520640 | 34910031052 |
| 20 | 8.2 | 530841600 | 38400 | 68451041280 | 69820062105 |
| 21 | 8.3 | 530841600 | 38400 | 68451041280 | 75296145408 |

MaxDim is the table's single combined dimension cap. Bitrates in kbps.

| idx | HdrRate | Mbps-M | Mbps-H | CR-M | CR-H | Tiles | TileCols |
|---|---|---|---|---|---|---|---|
| 0 | 150 | 1500 | - | 2 | - | 8 | 4 |
| 1 | 150 | 3000 | - | 2 | - | 8 | 4 |
| 2 | 150 | 6000 | - | 2 | - | 16 | 6 |
| 3 | 150 | 10000 | - | 2 | - | 16 | 6 |
| 4 | 300 | 12000 | 30000 | 4 | 4 | 32 | 8 |
| 5 | 300 | 20000 | 50000 | 4 | 4 | 32 | 8 |
| 6 | 300 | 30000 | 100000 | 6 | 4 | 64 | 8 |
| 7 | 300 | 40000 | 160000 | 8 | 4 | 64 | 8 |
| 8 | 300 | 60000 | 240000 | 8 | 4 | 64 | 8 |
| 9 | 300 | 60000 | 240000 | 8 | 4 | 64 | 8 |
| 10 | 300 | 60000 | 240000 | 8 | 4 | 128 | 16 |
| 11 | 300 | 100000 | 480000 | 8 | 4 | 128 | 16 |
| 12 | 300 | 160000 | 800000 | 8 | 4 | 128 | 16 |
| 13 | 300 | 160000 | 800000 | 8 | 4 | 128 | 16 |
| 14 | 960 | 160000 | 800000 | 8 | 4 | 256 | 32 |
| 15 | 960 | 200000 | 960000 | 8 | 4 | 256 | 32 |
| 16 | 960 | 320000 | 1600000 | 8 | 4 | 256 | 32 |
| 17 | 960 | 320000 | 1600000 | 8 | 4 | 256 | 32 |
| 18 | 960 | 320000 | 1600000 | 8 | 4 | 512 | 64 |
| 19 | 960 | 400000 | 1920000 | 8 | 4 | 512 | 64 |
| 20 | 960 | 640000 | 3200000 | 8 | 4 | 512 | 64 |
| 21 | 960 | 640000 | 3200000 | 8 | 4 | 512 | 64 |

seq_tier 1 signals only at L4.0 and up. Level 8.x caps MaxPic at
530841600 samples with MaxDim 38400; the top example format is
30720x17280 at 120 fps. Level-31 maximum-parameters row values are
unmapped.

## Substream levels

Scaled caps per MaxPicSize and scale factor (1.5, 4, 9): MaxVSizeX,
MaxHSizeX, MaxTileColsX, MaxHeaderRateX 132.

| MaxPicSize | Scale | MaxV | MaxH | TileCols | HdrX |
|---|---|---|---|---|
| 2359296 | 1.5 | 1600 | 896 | 7 | 132 |
| 2359296 | 4 | 960 | 576 | 4 | 132 |
| 2359296 | 9 | 640 | 384 | 3 | 132 |
| 8912896 | 1.5 | 2560 | 1472 | 7 | 132 |
| 8912896 | 4 | 1920 | 1088 | 4 | 132 |
| 8912896 | 9 | 1280 | 768 | 3 | 132 |
| 35651584 | 1.5 | 5120 | 2280 | 13 | 132 |
| 35651584 | 4 | 3840 | 2176 | 8 | 132 |
| 35651584 | 9 | 2560 | 1472 | 5 | 132 |
| 142606336 | 1.5 | 10240 | 5760 | 26 | 132 |
| 142606336 | 4 | 7680 | 4320 | 16 | 132 |
| 142606336 | 9 | 5120 | 2880 | 11 | 132 |
| 530841600 | 1.5 | 20480 | 11520 | 52 | 132 |
| 530841600 | 4 | 15360 | 8640 | 32 | 132 |
| 530841600 | 9 | 10240 | 5760 | 21 | 132 |

## Against AV1

New bitstream, not backward compatible. Superblocks reach 256x256 luma
against AV1's 128x128, with extended recursive partitioning and
semi-decoupled luma/chroma splits. Tool-level deltas beyond
partitioning (transforms, restoration, grain, tiles) are unmapped.
Decoder-model timing semantics beyond the tables are unmapped.
