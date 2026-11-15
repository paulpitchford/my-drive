#!/usr/bin/env bash
# Regenerates state-v1.db, the schema version 1 fixture for the version 2 migration test
# (WP-017 AC1), from the released version 1 code path.
#
# It checks out the last commit with schema version 1 into a temporary worktree, copies
# generate_v1_fixture.rs into that checkout's store tests, runs it, and copies the
# database it wrote next to this script. The fixture is committed, so this only needs to
# run again if the fixture itself must change.
#
# Usage: crates/mydrive-store/tests/fixtures/v1/generate.sh [scratch-dir]
# The scratch directory (default /var/tmp) holds the worktree and its build directory.
set -euo pipefail

V1_COMMIT=4673830
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
scratch="${1:-/var/tmp}"
work="$(mktemp -d "$scratch/mydrive-v1-fixture.XXXXXX")"
tree="$work/tree"
out="$work/state-v1.db"

cleanup() {
    git -C "$repo" worktree remove --force "$tree" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

git -C "$repo" worktree add --detach "$tree" "$V1_COMMIT" >/dev/null
cp "$here/generate_v1_fixture.rs" "$tree/crates/mydrive-store/tests/generate_v1_fixture.rs"
(
    cd "$tree"
    MYDRIVE_V1_FIXTURE_OUT="$out" CARGO_TARGET_DIR="$work/target" \
        mise exec -- cargo test -p mydrive-store --test generate_v1_fixture -- \
        --ignored --exact generate_v1_fixture
)
# The store closed cleanly, so the WAL was checkpointed into the main file.
test ! -e "$out-wal" || { echo "unexpected WAL file next to the fixture" >&2; exit 1; }
cp "$out" "$here/state-v1.db"
echo "wrote $here/state-v1.db"
