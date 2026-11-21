# Backlog

- SW threading deep-opt: cut per-frame fixed costs + batch submit APIs + instance-per-stream serving on the thread-sweep tables (.tmp/thread-sweep.md); fires on sweep delivery.
- Encode 500+ fps every codec every tier: d12 h264 104 vs d11 h264 393 same bytes same NVENC falsifies F3-as-physics; settings audit first (RC/QP/GOP/B-frames/slices per leg), then API path, then silicon re-proof.
- F3 re-proof: d12 h264 encode floor torn down by contradiction; new floor needs same-content gap-grade evidence on audited settings, else the leg stands on the audit target.
- F17/F18 re-proof: vk h264/h265 sustained floors torn down, _ip bytes replaced with IPB; re-derive on the new bytes.
- vk zero-copy surface: export copy costs 27us queue + 5-10us record CPU per the gap record; DPB-backed frames against the export-plane contract buy ~12% surface.
- Surface floors F15/F16/F21/F22: re-adjudicate at xport landing, the ports may move the measured paths.
