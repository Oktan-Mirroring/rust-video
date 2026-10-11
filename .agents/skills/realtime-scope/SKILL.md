---
name: realtime-scope
description: Use when adding or scoping any rust-video behaviour. MANDATORY, load before the first edit. Realtime encode/decode only; demux, subtitles, filters, audio stay out.
---

# Realtime scope

rust-video covers live frames and nothing else: codec encode/decode, platform tiers, livepack packing between them. Everything else belongs in another crate.

## In

- Codec backends: H.264, H.265, AV1, VP9, AV2.
- Platform tiers: Vulkan, VA-API, D3D11, MediaCodec, software.
- Frame packing and transport glue between backend and tier.

## Out

- Archival demuxing, subtitles, filters, audio.
- General media framework surface.

## Maturity

Backend lands only when it holds realtime throughput on its tier, measured on the clock. Pure-Rust loses to tuned C until it wins there. No toy backend behind this facade.

## Rule

New behaviour names its codec or tier crate in the change. No owner crate fits, behaviour stays out. Scope question unanswered in this file, ask owner before writing code.
