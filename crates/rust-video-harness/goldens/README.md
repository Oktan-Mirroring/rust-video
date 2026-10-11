<!-- source: hand-written for rust-video; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Goldens

One file per case: `<case>.sha256`, one lowercase hex SHA-256 per line in
frame output order. Blank lines and `#` comments are skipped. The hash covers
raw decoded frame bytes. Generation tooling lands with the first decoder
backend; until then this directory holds the format only.
