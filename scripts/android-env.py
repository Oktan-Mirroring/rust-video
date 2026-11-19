#!/usr/bin/env python3
"""Run cargo with the Android NDK linker resolved from the environment.

Finds the NDK (ANDROID_NDK_HOME, else ANDROID_HOME/ANDROID_SDK_ROOT,
else the platform default SDK path), takes the newest side-by-side
version, and execs cargo with CARGO_TARGET_<triple>_LINKER set for
both Android triples. The linker pins API 29: ndk-sys needs
android_get_device_api_level, __INTRODUCED_IN(29), so API 28 cannot
link. Binaries need API 29+ devices. Fails loudly.

Usage: android-env.py <cargo args...> (must include --target).
"""

import os
import shutil
import sys
from pathlib import Path

API = "29"
TRIPLES = ("aarch64-linux-android", "x86_64-linux-android")


def resolve():
    """NDK linker per triple; exits naming every tried root."""
    tried = []
    if "ANDROID_NDK_HOME" in os.environ:
        roots = [Path(os.environ["ANDROID_NDK_HOME"])]
    else:
        sdk = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
        if sdk is None:
            sdk = (
                Path(os.environ["LOCALAPPDATA"]) / "Android" / "Sdk"
                if os.name == "nt"
                else Path.home() / "Android" / "Sdk"
            )
        roots = sorted(Path(sdk).glob("ndk/*"), reverse=True)
        tried.append(f"{sdk}/ndk/* (none)" if not roots else f"{sdk}/ndk/*")
    hosts = (
        ["windows-x86_64"]
        if os.name == "nt"
        else ["linux-x86_64"] if sys.platform != "darwin" else ["darwin-arm64", "darwin-x86_64"]
    )
    ext = ".cmd" if os.name == "nt" else ""
    for root in roots:
        for host in hosts:
            linkers = {
                triple: root / "toolchains" / "llvm" / "prebuilt" / host / "bin" / f"{triple}{API}-clang{ext}"
                for triple in TRIPLES
            }
            if all(path.is_file() for path in linkers.values()):
                return linkers
            tried.append(str(root))
    sys.exit(f"android-env.py: no NDK with API-{API} linkers; tried: {', '.join(tried)}")


def main(args):
    if "--target" not in args and not any(arg.startswith("--target=") for arg in args):
        sys.exit("android-env.py: cargo args must include --target")
    found = resolve()
    env = os.environ.copy()
    for triple, linker in found.items():
        env[f"CARGO_TARGET_{triple.upper().replace('-', '_')}_LINKER"] = str(linker)
    cargo = shutil.which("cargo")
    if cargo is None:
        sys.exit("android-env.py: cargo not on PATH")
    os.execvpe(cargo, ["cargo", *args], env)


if __name__ == "__main__":
    main(sys.argv[1:])
