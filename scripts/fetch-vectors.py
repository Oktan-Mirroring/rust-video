#!/usr/bin/env python3
"""Fetch the vector corpus into ./corpus.

Verified sources download with hash checks; everything else prints manual
drop-in instructions. Fails loudly.
"""
import hashlib
import os
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"
MANIFEST_URL = "https://raw.githubusercontent.com/webmproject/libvpx/main/test/test-data.sha1"
DOWNLOAD_URL = "http://downloads.webmproject.org/test_data/libvpx/{}"

MANUAL = """== manual drop-in
H.264/H.265: ITU wftp3 JCT-VC bitstream exchange
  https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/
  Place files under corpus/h264/ and corpus/h265/.
Raw YUV: Xiph derf
  https://media.xiph.org/video/derf/
AV1/AV2: AOM test clips and AVM corpus, sources unverified."""


def normalize_ivf(dest: Path) -> None:
    """Rewrite a non-VP80 IVF FourCC to VP80, mirroring the ffmpeg oracle.

    Upstream libvpx vp80-03-segmentation-01..04.ivf ship FourCC I420 over VP8
    payloads; the ffmpeg oracle trusts the FourCC. No-op for non-IVF paths,
    missing files, and non-DKIF content.
    """
    if dest.suffix != ".ivf" or not dest.is_file():
        return
    try:
        data = dest.read_bytes()
    except OSError:
        return
    if data[:4] != b"DKIF" or data[8:12] == b"VP80":
        return
    fourcc = data[8:12].hex()
    fixed = bytearray(data)
    if len(fixed) < 12:
        fixed.extend(b"\0" * (12 - len(fixed)))
    fixed[8:12] = b"VP80"
    tmp = dest.with_name(dest.name + ".normalize")
    try:
        tmp.write_bytes(fixed)
    except OSError:
        tmp.unlink(missing_ok=True)
        return
    print(f"NORMALIZED IVF FourCC {fourcc} -> 56503830 (VP80): {dest}")
    try:
        tmp.replace(dest)
    except OSError:
        pass


def fetch(url: str, timeout: int) -> bytes | None:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return response.read()
    except OSError as error:
        print(f"fetch failed: {url}: {error}", file=sys.stderr)
        return None


def main() -> int:
    for codec in ("h264", "h265", "av1", "vp9", "vp8", "av2"):
        (CORPUS / codec).mkdir(parents=True, exist_ok=True)
    try:
        cap = int(os.environ.get("VECTORS_MAX", "0"))
    except ValueError:
        cap = 0

    fail = False
    fetched = 0
    print("== libvpx VP9/VP8 vectors")
    manifest = fetch(MANIFEST_URL, 60)
    if not manifest:
        print("FAIL: libvpx manifest unreachable", file=sys.stderr)
        fail = True
    else:
        for line in manifest.decode("utf-8", "replace").replace("*", "").splitlines():
            parts = line.split(None, 1)
            if len(parts) != 2:
                continue
            sha, file = parts
            if not (file.endswith(".ivf") or file.endswith(".webm")):
                continue
            if cap > 0 and fetched >= cap:
                break
            dest = CORPUS / ("vp8" if file.startswith("vp80-") else "vp9") / file
            if dest.is_file():
                if file.startswith("vp80-"):
                    normalize_ivf(dest)
                fetched += 1
                continue
            payload = fetch(DOWNLOAD_URL.format(file), 120)
            if payload is None:
                print(f"FAIL: {file} unreachable", file=sys.stderr)
                dest.unlink(missing_ok=True)
                fail = True
            elif hashlib.sha1(payload).hexdigest() != sha:
                print(f"FAIL: {file} hash mismatch", file=sys.stderr)
                dest.unlink(missing_ok=True)
                fail = True
            else:
                try:
                    dest.write_bytes(payload)
                except OSError:
                    print(f"FAIL: {file} not writable", file=sys.stderr)
                    dest.unlink(missing_ok=True)
                    fail = True
                    fetched += 1
                    continue
                if file.startswith("vp80-"):
                    normalize_ivf(dest)
            fetched += 1

    print(MANUAL)
    count = sum(1 for _ in CORPUS.rglob("*") if _.is_file())
    print(f"corpus files: {count}")
    if fail:
        print("vector fetch failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
