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

# Full gate: format, lints, tests, docs.
ci: fmt-check check test doc
