# Recipes run under PowerShell on Windows; sh is not on PATH there.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

export RUSTDOCFLAGS := "-D warnings"

python := if os() == "windows" { "python" } else { "python3" }

# Format the workspace.
fmt:
    cargo fmt --all

# Clippy over the workspace, every cargo-only feature, warnings denied.
# rust-video-sw leaves the --all-features line: its ffmpeg feature links
# system libavcodec, so sw lints with h264-openh264 and e2e covers ffmpeg.
check:
    cargo clippy --workspace --exclude rust-video-sw --all-targets --all-features -- -D warnings
    cargo clippy -p rust-video-sw --all-targets --features h264-openh264 -- -D warnings

# Unit tests across the workspace: default, no-default, sw codec.
# The livepack ffmpeg oracle stays out: it needs --features
# rust-video-livepack/oracle, which only the e2e path passes.
# Builds stay parallel; test binaries run serially so throughput bars
# measure an unloaded box instead of cargo-sibling contention.
test:
    cargo test --workspace --no-run
    cargo test --workspace --no-default-features --no-run
    cargo test -p rust-video-sw --features h264-openh264 --no-run
    cargo test --workspace -j1
    cargo test --workspace --no-default-features -j1
    cargo test -p rust-video-sw --features h264-openh264 -j1

# Debug build of the workspace.
build:
    cargo build --workspace

# API docs for the workspace, no dependency docs.
doc:
    cargo doc --workspace --exclude rust-video-sw --no-deps --all-features
    cargo doc -p rust-video-sw --no-deps --features h264-openh264

# Formatting gate for CI.
fmt-check:
    cargo fmt --all --check

# Full gate: format, lints, build, tests, docs, spec tables.
ci: fmt-check check build test doc spec-check

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
    cargo run -p rust-video-harness --bin golden_gen -- {{args}}

# Commit gate hook entry point; wiring lives in .agents/hooks/README.md.
commit-lint-gate:
    {{python}} .agents/hooks/commit-lint-gate.py

# MSRV gate: check and test on the pinned 1.87.0 toolchain. sw/openh264
# stays out: its transitive safe_arch/wide need rustc 1.89.
msrv:
    cargo +1.87.0 clippy --workspace --exclude rust-video-sw --all-targets --all-features -- -D warnings
    cargo +1.87.0 clippy -p rust-video-sw --all-targets -- -D warnings
    cargo +1.87.0 test --workspace --no-run
    cargo +1.87.0 test --workspace --no-default-features --no-run
    cargo +1.87.0 test --workspace -j1
    cargo +1.87.0 test --workspace --no-default-features -j1
