#!/usr/bin/env python3
"""Fetch the pinned vector corpus into ./corpus.

Only vectors the golden matrix names but no encoder produces are
fetched, each pinned by SHA-1 and verified after download. Everything
else is synthesized by `just goldens --synth` from lavfi recipes.
Fails loudly.
"""
import hashlib
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"
DOWNLOAD_URL = "http://downloads.webmproject.org/test_data/libvpx/{}"

# (file, sha1, codec dir): the golden matrix's fetched vectors.
# Superframes: real encoder-emitted multi-frame packets; the synth
# crafts could not reproduce a decodable superframe index, so the
# conformance vector stands in (remuxed to IVF below, stream copy).
PINNED = [
    (
        "vp90-2-20-big_superframe-01.webm",
        "f97088c7359fc8d3d5aa5eafe57bc7308b3ee124",
        "vp9",
    ),
]

MANUAL = """== manual drop-in (blocked matrix lines only)
H.264/H.265: ITU wftp3 JCT-VC bitstream exchange
  https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/
  Place files under corpus/h264/ and corpus/h265/.
  Wanted: MMCO incl MMCO6, PAFF frame/field mixing, BLA, long-term
  refs, Rext-tagged streams, explicit tile spacing.
Raw YUV: Xiph derf
  https://media.xiph.org/video/derf/
AV1/AV2: AOM test clips and AVM corpus, sources unverified."""


def fetch(url: str, timeout: int) -> bytes | None:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return response.read()
    except OSError as error:
        print(f"fetch failed: {url}: {error}", file=sys.stderr)
        return None


def remux_ivf(src: Path, dest: Path) -> bool:
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None:
        print("FAIL: ffmpeg not on PATH, needed for the IVF remux", file=sys.stderr)
        return False
    proc = subprocess.run(
        [ffmpeg, "-v", "error", "-y", "-i", str(src),
         "-c", "copy", "-f", "ivf", str(dest)],
        capture_output=True, text=True)
    if proc.returncode != 0:
        print(f"FAIL: IVF remux of {src.name}: {proc.stderr.strip()}",
              file=sys.stderr)
        dest.unlink(missing_ok=True)
        return False
    return True


def main() -> int:
    for codec in ("h264", "h265", "av1", "vp9"):
        (CORPUS / codec).mkdir(parents=True, exist_ok=True)

    fail = False
    for file, sha, codec in PINNED:
        dest = CORPUS / codec / file
        final = dest.with_suffix(".ivf")
        if final.is_file():
            print(f"present: {final.relative_to(ROOT)}")
            continue
        payload = fetch(DOWNLOAD_URL.format(file), 120)
        if payload is None:
            print(f"FAIL: {file} unreachable", file=sys.stderr)
            fail = True
            continue
        if hashlib.sha1(payload).hexdigest() != sha:
            print(f"FAIL: {file} hash mismatch", file=sys.stderr)
            fail = True
            continue
        try:
            dest.write_bytes(payload)
        except OSError:
            print(f"FAIL: {file} not writable", file=sys.stderr)
            dest.unlink(missing_ok=True)
            fail = True
            continue
        # Canonical vector form for vp9/av1 is IVF (the sw runner's
        # framed path); the remux is a stream copy, packets preserved.
        if dest.suffix == ".webm" and not remux_ivf(dest, final):
            fail = True
            continue
        dest.unlink(missing_ok=True)
        print(f"fetched: {final.relative_to(ROOT)}")

    print(MANUAL)
    count = sum(1 for _ in CORPUS.rglob("*") if _.is_file())
    print(f"corpus files: {count}")
    if fail:
        print("vector fetch failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
