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

python := if os() == "windows" { "python" } else { "python3" }

# Diff spec doc tables against caps const tables.
spec-check:
    {{python}} scripts/spec-check.py

# Fetch the vector corpus into ./corpus (network).
vectors:
    {{python}} scripts/fetch-vectors.py

# End-to-end suites; needs ./corpus, fails loud without it.
e2e:
    {{python}} scripts/e2e.py

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
