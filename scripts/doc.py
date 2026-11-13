#!/usr/bin/env python3
"""API docs for the workspace, no dependency docs, warnings denied."""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

COMMANDS = [
    ["cargo", "doc", "--workspace", "--exclude", "rust-video-sw",
     "--no-deps", "--all-features"],
    ["cargo", "doc", "-p", "rust-video-sw", "--no-deps",
     "--features", "h264-openh264"],
]


def main() -> int:
    env = dict(os.environ, RUSTDOCFLAGS="-D warnings")
    for args in COMMANDS:
        completed = subprocess.run(args, cwd=ROOT, env=env)
        if completed.returncode != 0:
            return completed.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
