# Backlog

- SW threading deep-opt: cut per-frame fixed costs + batch submit APIs + instance-per-stream serving on the thread-sweep tables (.tmp/thread-sweep.md); sweep delivered, rv column waits on vendor-ffmpeg or lab.
- Encode 500+ fps every codec every tier: audit done, d11 664/707/590 and vk h264 505 clear 500; remaining are d12 h264 237 (+2.3ms path), vk h265 225 (driver), sw 260 (knee at 4, x264 backend buys 930-class).
- F3 re-proof: d12 h264 encode floor torn down by contradiction; new floor needs same-content gap-grade evidence on audited settings, else the leg stands on the audit target.
- F17/F18 re-proof: vk h264/h265 sustained floors torn down, _ip bytes replaced with IPB; re-derive on the new bytes.
- vk zero-copy surface: export copy costs 27us queue + 5-10us record CPU per the gap record; DPB-backed frames against the export-plane contract buy ~12% surface.
- d11 h264 surface submit/wait split + stretch dec%: firms the 40-45us Vulkan-vs-DXVA piece in xport-vk-gap, currently resting on a sync proxy.
- d11 vp9 engine headroom ≤371us/f if submit-side else 0: per-call split in submit_frame_split plus surface_frame plus fence wait.
- d12 sync worker-thread tighten: holds unsound on the video-copy interlock (black frames without CPU wait_video); the win is worker-side, not a port.
- Unify tail-counting convention across tiers: d11 counts held tails, d12 times flush; cross-tier sync ordering is convention-contaminated until unified.
