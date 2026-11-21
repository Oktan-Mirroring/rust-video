# Depth and thread tuning

Manual knobs in the ffmpeg model: the caller configures, a bad number is
the caller's problem. Defaults hold the landed behavior on every tier.
Measure your own box before changing them; the numbers below fit one
machine only.

## Knobs

| Tier | Knob | Shape | Default | Max | Cap source |
|---|---|---|---|---|---|
| d3d11 | depth, per decoder | `open_depth(device, depth)` | `DEFAULT_DEPTH` 4 | `MAX_DEPTH` 8 | readback lanes |
| d3d12 | bound | `open(device, max_inflight)` | `MAX_INFLIGHT` 4 | `MAX_INFLIGHT` 4 | bitstream ring |
| vulkan | depth, per decoder | `open_depth(device, depth)` | `DEFAULT_DEPTH` 3 | `MAX_DEPTH` 8 | ring allocation |
| sw | pipeline depth | `PipelineConfig::new(depth)` | `DEFAULT_DEPTH` 4 | `MAX_DEPTH` 16 | held frames |
| sw | worker threads | `open(..., threads)` | 4 in benches, 0 is backend default | none | manual value wins |

Every depth knob clamps to its range; out-of-range input lands on the
nearest bound. `open()` keeps the default everywhere.

## Measured knees

Sustained 1080p IP vectors, quiet box, median-of-2/3. Machine: RTX 5080,
Ryzen 9 9950X3D (16C/32T), 2026-11-20.

| Tier | Codec | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | Knee | Min |
|---|---|---|---|---|---|---|---|---|---|---|---|
| d11 | h264 | 1672 | 1950 | 2017 | 2006 | 2004 | 2008 | 2003 | 1911 | 3 | 1 |
| d11 | hevc/av1/vp9 | — | — | — | — | — | — | — | — | — | 1 |
| d12 | h264 | 1232 | 2400 | 2447 | 2428 | — | — | — | — | 3 | 1 |
| d12 | hevc | STALL | ok | — | — | — | — | — | — | — | 2 |
| d12 | av1 | ok | ok | — | — | — | — | — | — | — | 1 |
| vk | h264 | 1714 | 2534 | 2951 | 2994 | 2843 | 2822 | — | — | 3 | 1 |
| vk | h265 | STALL | STALL | 1741 | 1700 | 1731 | 1675 | — | — | 3 | 3 |
| vk | av1 | 1360 | 1877 | 1893 | 2002 | 2055 | 1962 | — | — | 5 | 1 |

Fps per depth; STALL is a 5s completion timeout, ok completes
without a timed sample. Dashes are unprobed: depths past the tier
cap, past clear deeper-worse, or codecs whose knees were not swept
(same pump mechanics as the swept codec on their tier).

Notes:

- d11: depths 3-7 sit within 1% (2003-2017); depth 8 costs 5%.
  Default 4 stands.
- d12: bound 2 keeps 99% of bound-4 fps (2400 vs 2428) at half the
  latency (785 vs 1590us medians). Default 4 stands for max fps.
- vk h264: depth 4 matches 3 (+1%, noise); 5-6 cost 4%.
  Default 3 stands.
- vk h265: depths 1-2 deadlock on inter content (see below).
  Default 3 stands.
- vk av1: depth 5 beats default 3 by 9% (2055 vs 1893 medians);
  gains shrink past it (+6% at 4, +3% at 5, -5% at 6). Depth 5 is
  the recommended av1 value on this box.
- sw openh264 encode, threads 1/2/4/8: 72/146/272/269fps. Scaling
  stops dead past 4 (8 matches 4). Default 4 stands.
- sw ffmpeg decode, threads 1/4/8/16/24/32/auto: gold scales
  1283/3399/5666/6500/6489/5982/6203fps on true-IPB 18000f; 32
  regresses 8% vs 16 (SMT plus cross-CCD plus frame-sync
  overhead). Sweet spot 8-16; auto lands near 16.
- sw multi-instance: N parallel 1t decodes aggregate flat ~6k
  from N=16 to 300 (p50 18/stream at N=300). Cap is memory
  bandwidth (~78GB/s needed vs ~75 wall) with CPU saturated;
  ceiling ~80-90 soft 1080p60 streams, 300 needs 3x box or HW.

## Minimum usable depth

Overlap depth must exceed the stream's reorder hold. Below it the
pump refuses sends while the drain waits for an emission that needs
another submit (vulkan), or parks on a wakeup that only a send
delivers (d3d12). Measured minimums on inter content: vulkan h265
needs 3 (holds 2); d3d12 hevc needs 2 (holds 1); everything else
measured completes at 1. d3d11 completes at every depth on every
codec measured. Sync decode was not observed to stall: its drains
wait on completion rather than demand emission.

## How to measure your own box

1. Quiet the box: CPU idle, no builds running, GPU near idle.
   Discard any sample taken under a spike and re-run it.
2. Sweep the knob over the sustained 1080p IP vector for the codec:
   depths 1 through the tier max, 2-3 samples each, fresh pump per
   sample after the standard untimed prime.
3. Take medians. The knee is the last depth with a material gain;
   past it expect flat-then-worse (queueing inflates spans without
   adding throughput). The minimum is the smallest depth that
   completes; below it some codecs stall (see above).
4. Keep the default unless the knee beats it by a margin you can
   reproduce twice. Never set a value from a single sample.

## Deferred tiers

mediacodec (`PipelineLedger::DEFAULT_DEPTH` 16) and vaapi
(`PendingQueue::DEFAULT_DEPTH` 4) need phone/lab runs; the sweep
shape above applies unchanged. sw pipeline depth is lab-gated with
them (no real engine on this box); sw thread counts are measured
above.
