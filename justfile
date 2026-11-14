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

# Check crate dependencies against the overview's crate diagram, and forbid(unsafe_code)
arch-check:
    python3 scripts/check_arch.py

# List every ADR rule with the tests that name it (report only, never fails on coverage)
rule-coverage:
    python3 scripts/rule_coverage.py

# Run the tests of the scripts in scripts/
scripts-test:
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py'

# Everything CI runs
check: fmt-check lint test deny docs-check arch-check scripts-test rule-coverage
