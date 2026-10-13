# Format the workspace.
fmt:
    cargo fmt --all

# Clippy over the workspace, warnings denied.
check:
    cargo clippy --workspace --all-targets -j24 -- -D warnings

# Unit tests across the workspace.
test:
    cargo test --workspace -j24

# Debug build of the workspace.
build:
    cargo build --workspace -j24

# API docs for the workspace, no dependency docs.
doc:
    cargo doc --workspace --no-deps

# Formatting gate for CI.
fmt-check:
    cargo fmt --all --check

# Full gate: format, lints, tests, docs, spec tables.
ci: fmt-check check test doc spec-check

# Diff spec doc tables against caps const tables.
spec-check:
    python3 scripts/spec-check.py

# Fetch the vector corpus into ./corpus (network).
vectors:
    bash scripts/fetch-vectors.sh

# End-to-end suites; needs ./corpus, fails loud without it.
e2e:
    test -d corpus || (echo "corpus missing: run 'just vectors' first" && exit 1)
    RUST_VIDEO_CORPUS={{justfile_directory()}}/corpus cargo test --workspace -j24 --test e2e

# Generate goldens with the ffmpeg oracle; fails on uncovered options.
goldens *args:
    cargo run -j24 -p rust-video-harness --bin golden_gen -- {{args}}

# Commit gate hook entry point.
commit-lint-gate:
    bash .claude/hooks/commit-lint-gate.sh

# MSRV gate: check and test on the pinned 1.87.0 toolchain.
msrv:
    cargo +1.87.0 clippy --workspace --all-targets -j24 -- -D warnings
    cargo +1.87.0 test --workspace -j24
