#!/usr/bin/env python3
"""End-to-end suites; needs ./corpus, fails loud without it."""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"

FEATURES = ["rust-video-sw/ffmpeg", "rust-video-livepack/oracle"]

TESTS = [
    "e2e",
    "tier_xcode",
    "fmp4_ffmpeg",
    "vp_packing",
    "probe",
    "throughput",
    "pictures",
    "crop",
    "decode",
]


def main() -> int:
    if not CORPUS.is_dir():
        print("corpus missing: run 'just vectors' first")
        return 1
    env = dict(os.environ, RUST_VIDEO_CORPUS=str(CORPUS))
    args = ["cargo", "test", "--workspace", "--features", ",".join(FEATURES)]
    for name in TESTS:
        args += ["--test", name]
    completed = subprocess.run(args, cwd=ROOT, env=env)
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
