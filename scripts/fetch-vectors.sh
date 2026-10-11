#!/usr/bin/env bash
# Fetch the vector corpus into ./corpus. Verified sources download with hash
# checks; everything else prints manual drop-in instructions. Fails loudly.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
corpus="$root/corpus"
mkdir -p "$corpus/h264" "$corpus/h265" "$corpus/av1" "$corpus/vp9" "$corpus/vp8" "$corpus/av2"

fail=0
fetched=0
max="${VECTORS_MAX:-0}"

echo "== libvpx VP9/VP8 vectors"
manifest="$(curl -sL --max-time 60 'https://raw.githubusercontent.com/webmproject/libvpx/main/test/test-data.sha1')"
if [ -z "$manifest" ]; then
  echo "FAIL: libvpx manifest unreachable" >&2
  fail=1
else
  while read -r sha file; do
    case "$file" in
      *.ivf|*.webm) ;;
      *) continue ;;
    esac
    if [ "$max" -gt 0 ] && [ "$fetched" -ge "$max" ]; then
      break
    fi
    case "$file" in
      vp80-*) dest="$corpus/vp8/$file" ;;
      *) dest="$corpus/vp9/$file" ;;
    esac
    if [ -f "$dest" ]; then
      fetched=$((fetched+1))
      continue
    fi
    if ! curl -sL --max-time 120 -o "$dest" "http://downloads.webmproject.org/test_data/libvpx/$file"; then
      echo "FAIL: $file unreachable" >&2
      rm -f "$dest"
      fail=1
      continue
    fi
    got="$(sha1sum "$dest" | cut -d' ' -f1)"
    if [ "$got" != "$sha" ]; then
      echo "FAIL: $file hash mismatch" >&2
      rm -f "$dest"
      fail=1
    fi
    fetched=$((fetched+1))
  done <<< "$(printf '%s' "$manifest" | tr -d '*')"
fi

cat <<'MANUAL'
== manual drop-in
H.264/H.265: ITU wftp3 JCT-VC bitstream exchange
  https://www.itu.int/wftp3/av-arch/jctvc-site/bitstream_exchange/
  Place files under corpus/h264/ and corpus/h265/.
Raw YUV: Xiph derf
  https://media.xiph.org/video/derf/
AV1/AV2: AOM test clips and AVM corpus, sources unverified.
MANUAL

count="$(find "$corpus" -type f | wc -l)"
echo "corpus files: $count"
[ "$fail" -eq 0 ] || { echo "vector fetch failed" >&2; exit 1; }
