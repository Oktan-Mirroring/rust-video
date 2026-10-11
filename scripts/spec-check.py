#!/usr/bin/env python3
"""Diff docs/specs level tables against rust-video-caps const tables.

Each pair names the doc tables (by header marker, with label columns to
drop) and the Rust struct whose literal rows must carry the same number
sequence. Integers, floats, `-` and `None` (both zero), `Some(x)` (x),
and `NxM` cells (N, M) all flatten in order. See docs/sourcing.md.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (spec doc, [(header marker, drop columns)], caps source, struct name)
PAIRS = [
    ("docs/specs/h264.md", [("MBPS", [0])],
     "crates/rust-video-caps/src/h264.rs", "LevelCaps"),
    ("docs/specs/h265.md", [("MaxLumaPs", [0]), ("MaxLumaSr", [0])],
     "crates/rust-video-caps/src/h265.rs", "LevelCaps"),
    ("docs/specs/av1.md", [("MaxPic", [1]), ("HdrRate", [0])],
     "crates/rust-video-caps/src/av1.rs", "LevelCaps"),
    ("docs/specs/vp9.md", [("SampleRate", [0])],
     "crates/rust-video-caps/src/vp9.rs", "LevelCaps"),
    ("docs/specs/av2.md", [("DecRate", [1]), ("HdrRate", [0])],
     "crates/rust-video-caps/src/av2.rs", "LevelCaps"),
    ("docs/specs/av2.md", [("Scale", [])],
     "crates/rust-video-caps/src/av2.rs", "SubCaps"),
]

NUM = re.compile(r"-?\d[\d_]*(?:\.\d+)?")


def md_tables(path):
    """Map header text to data rows for each markdown table."""
    tables = {}
    header, rows, in_table = None, None, False
    for line in (ROOT / path).read_text().splitlines():
        if line.startswith("|"):
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if not in_table:
                header, rows, in_table = cells, [], True
            elif set("".join(cells)) <= set("|-: "):
                continue
            else:
                rows.append(cells)
        elif in_table:
            tables[" ".join(header)] = rows
            header, rows, in_table = None, None, False
    if in_table:
        tables[" ".join(header)] = rows
    return tables


def cell_numbers(cell):
    if cell == "-":
        return [0.0]
    out = []
    for found in NUM.findall(cell.replace("x", " ")):
        try:
            out.append(float(found.replace("_", "")))
        except ValueError:
            pass
    return out


def table_rows(path, marker, drop):
    tables = md_tables(path)
    hits = [(h, rows) for h, rows in tables.items() if marker in h]
    if len(hits) != 1:
        raise SystemExit(f"{path}: marker {marker!r} hits {len(hits)} tables")
    out = []
    for row in hits[0][1]:
        seq = []
        for i, cell in enumerate(row):
            if i in drop:
                continue
            seq.extend(cell_numbers(cell))
        out.append(seq)
    return out


def md_numbers(path, markers):
    picked = [table_rows(path, m, d) for m, d in markers]
    counts = {len(p) for p in picked}
    if len(counts) != 1:
        raise SystemExit(f"{path}: row counts disagree {counts}")
    zipped = []
    for rows in zip(*picked):
        for seq in rows:
            zipped.extend(seq)
    return zipped


def rs_numbers(path, struct):
    seq = []
    for line in (ROOT / path).read_text().splitlines():
        if struct + " {" not in line:
            continue
        body = line.split("{", 1)[1]
        body = re.sub(r"\bNone\b", "0", body)
        body = re.sub(r"Some\(([^)]*)\)", r"\1", body)
        for found in NUM.findall(body):
            seq.append(float(found.replace("_", "")))
    return seq


def main():
    failures = 0
    for doc, markers, src, struct in PAIRS:
        want = md_numbers(doc, markers)
        got = rs_numbers(src, struct)
        ok = want == got
        print(f"{doc} vs {src} [{struct}]: {'OK' if ok else 'DRIFT'} ({len(want)}/{len(got)})")
        if not ok:
            failures += 1
            for i, (a, b) in enumerate(zip(want, got)):
                if a != b:
                    print(f"  first divergence at #{i}: doc={a} code={b}")
                    break
            if len(want) != len(got):
                print(f"  length differs: doc={len(want)} code={len(got)}")
    return failures


if __name__ == "__main__":
    sys.exit(1 if main() else 0)
