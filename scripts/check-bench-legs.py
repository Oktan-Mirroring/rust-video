#!/usr/bin/env python3
"""Every tests/throughput.rs needs serial legs in the justfile.

Throughput binaries stay out of parallel `cargo test` (test = false in
each manifest) and run one at a time under the benches recipe. A tier
without legs skips its bars.
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Extra feature shapes beyond the default leg, by package.
EXTRA_LEGS = {
    "rust-video-sw": ["--features h264-openh264"],
}


def main():
    justfile = (ROOT / "justfile").read_text()
    missing = []
    paths = sorted((ROOT / "crates").glob("*/tests/throughput.rs"))
    for path in paths:
        pkg = path.parts[-3]
        if f"-p {pkg} --test throughput" not in justfile:
            missing.append(pkg)
        lines = path.read_text().splitlines()
        for index, line in enumerate(lines):
            if line.strip() != "#[ignore]":
                continue
            name = next(
                (
                    following.split("fn ")[1].split("(")[0]
                    for following in lines[index + 1 : index + 3]
                    if "fn " in following
                ),
                "",
            )
            if name.endswith("_measures_realtime") and name not in justfile:
                missing.append(f"{pkg} ignored bar without a leg")
        for shape in EXTRA_LEGS.get(pkg, []):
            if shape not in justfile:
                missing.append(f"{pkg} {shape}")
    if missing:
        print("throughput binaries without benches legs: " + ", ".join(missing))
        return 1
    print(f"bench legs ok ({len(paths)} binaries)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
