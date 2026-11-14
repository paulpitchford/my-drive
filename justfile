# Developer tasks. Run `just` to list them. `just check` is what CI runs.

default:
    @just --list

# Format all code
fmt:
    cargo fmt --all

# Check formatting without changing files
fmt-check:
    cargo fmt --all --check

# Lint with warnings denied
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run all tests
test:
    cargo test --workspace --locked

# Audit dependencies: advisories, licences, sources
deny:
    cargo deny check

# Check relative links and cross-references in the docs
docs-check:
    python3 scripts/check_docs.py

# Everything CI runs
check: fmt-check lint test deny docs-check
