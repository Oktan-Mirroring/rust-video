<!-- source: Khronos headers, libva va.h, MSDN D3D11VA, AOSP MediaCodec; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Hardware tiers

Tier 1 throughout, one rule: where the vendor publishes no fixed cap,
the build is a runtime query, not a constant.

## Vulkan Video (Khronos headers, video.xml)

Decode ops for H.264, H.265, AV1, VP9; encode ops for H.264, H.265,
AV1; no VP9 encode. H.264 profiles Baseline through High 4:4:4
Predictive, levels 1.0-6.2. H.265 profiles Main, Main 10, Main Still,
format-range and screen-content extensions, levels 1.0-6.2. AV1
profiles Main/High/Professional, levels 2.0-7.3. VP9 profiles 0-3
with no level enum. No fixed resolution cap: per-device maxCodedExtent.
DPB via maxDpbSlots and maxActiveReferencePictures. Rate control modes:
default, disabled, CBR, VBR.

## VA-API (libva va.h)

Profiles span MPEG-2/4, VC-1, H.263, JPEG, H.264 (Constrained Baseline
through High 4:4:4 Predictive, multiview, stereo), VP8, HEVC (Main
through 4:4:4-12 and screen-content variants), VP9 P0-3, AV1 P0-2, and
VVC. Entry points cover decode, encode slices and pictures, low-power
encode, front-end ingest, video processing, and statistics. Resolution
limits are driver-reported; over-limit decode returns resolution-not-
supported.

## D3D11VA (MSDN)

H.264 A-F (motion compensation, IDCT, VLD with and without film grain),
HEVC VLD Main/Main 10, MPEG-2, VC-1, WMV families. VP9 VLD Profile 0
per the DXVA VPx specification. No published maximum resolution:
per-adapter enumeration, and the basic display adapter reports no
decoders. Decoder GUIDs come from the windows-crate constants, never
hand-copied hex; the unmapped-hex gap is closed by rule.

## MediaCodec (AOSP)

MIME types cover AVC, HEVC, AV1, VP8, VP9, MPEG-4, H.263, MPEG-2,
Dolby Vision, and APV. AVC profiles Baseline through High 4:4:4 with
levels 1-6.2; HEVC Main/10/Still with Main/High tiers to L6.2; VP9
P0-3 with levels; AV1 Main 8/10-bit with levels 2-7.3. Maximum
resolution and bitrate are per-device only: supported widths, heights,
bitrate ranges, and performance points, with presets reaching
3840x2160 at 240 fps. Vendor variance is real: performance points may
be null on stale vendor codecs. Certification minimum floors are
unmapped.
