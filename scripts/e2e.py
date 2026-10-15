#!/usr/bin/env python3
"""End-to-end suites; needs ./corpus, fails loud without it."""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"


def main() -> int:
    if not CORPUS.is_dir():
        print("corpus missing: run 'just vectors' first")
        return 1
    env = dict(os.environ, RUST_VIDEO_CORPUS=str(CORPUS))
    completed = subprocess.run(
        [
            "cargo", "test", "--workspace", "-j24",
            "--features", "rust-video-sw/ffmpeg", "--test", "e2e",
        ],
        cwd=ROOT,
        env=env,
    )
    return completed.returncode


if __name__ == "__main__":
    sys.exit(main())
