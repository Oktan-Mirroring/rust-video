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
test:
    cargo test --workspace
    cargo test --workspace --no-default-features
    cargo test -p rust-video-sw --features h264-openh264

# Throughput bars, one binary at a time: bars measure an unloaded box.
benches:
    {{python}} scripts/check-bench-legs.py
    cargo test -p rust-video-d3d11 --test throughput
    cargo test -p rust-video-d3d11 --test throughput -- --ignored --exact encode_vp9_1080p_measures_realtime
    cargo test -p rust-video-d3d12 --test throughput
    cargo test -p rust-video-vulkan --test throughput
    cargo test -p rust-video-vaapi --test throughput
    cargo test -p rust-video-mediacodec --test throughput
    cargo test -p rust-video-sw --test throughput
    cargo test -p rust-video-sw --features h264-openh264 --test throughput
    cargo test -p rust-video-livepack --test throughput

# Release throughput headline, one binary at a time: same legs as benches
# with --release. Debug benches stay the enforced CI floor (see ci).
benches-release:
    {{python}} scripts/check-bench-legs.py
    cargo test --release -p rust-video-d3d11 --test throughput
    cargo test --release -p rust-video-d3d11 --test throughput -- --ignored --exact encode_vp9_1080p_measures_realtime
    cargo test --release -p rust-video-d3d12 --test throughput
    cargo test --release -p rust-video-vulkan --test throughput
    cargo test --release -p rust-video-vaapi --test throughput
    cargo test --release -p rust-video-mediacodec --test throughput
    cargo test --release -p rust-video-sw --test throughput
    cargo test --release -p rust-video-sw --features h264-openh264 --test throughput
    cargo test --release -p rust-video-livepack --test throughput

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

# Full gate: format, lints, build, tests, benches, docs, spec tables.
ci: fmt-check check build test benches doc spec-check

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

# Cargo for an Android target; resolves the NDK linker from the
# environment (API 29 floor: ndk-sys cannot link 28). Needs --target.
android *args:
    {{python}} scripts/android-env.py {{args}}

# Commit gate hook entry point; wiring lives in .agents/hooks/README.md.
commit-lint-gate:
    {{python}} .agents/hooks/commit-lint-gate.py

# MSRV gate: check and test on the pinned 1.87.0 toolchain. sw/openh264
# stays out: its transitive safe_arch/wide need rustc 1.89. Throughput
# legs run before the full suites, on a cool box.
msrv:
    cargo +1.87.0 clippy --workspace --exclude rust-video-sw --all-targets --all-features -- -D warnings
    cargo +1.87.0 clippy -p rust-video-sw --all-targets -- -D warnings
    cargo +1.87.0 test -p rust-video-d3d11 --test throughput
    cargo +1.87.0 test -p rust-video-d3d11 --test throughput -- --ignored --exact encode_vp9_1080p_measures_realtime
    cargo +1.87.0 test -p rust-video-d3d12 --test throughput
    cargo +1.87.0 test -p rust-video-vulkan --test throughput
    cargo +1.87.0 test -p rust-video-vaapi --test throughput
    cargo +1.87.0 test -p rust-video-mediacodec --test throughput
    cargo +1.87.0 test -p rust-video-sw --test throughput
    cargo +1.87.0 test --workspace
    cargo +1.87.0 test --workspace --no-default-features
    cargo +1.87.0 test -p rust-video-livepack --test throughput
