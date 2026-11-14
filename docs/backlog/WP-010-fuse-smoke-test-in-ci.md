# WP-010: FUSE smoke test in CI

- **Status:** ready
- **Milestone:** M0
- **Wave:** 1
- **Depends on:** —
- **Gates:** —
- **Owned paths:** `crates/mydrive-vfs/**`, `.github/workflows/ci.yml`,
  `docs/process/ci-fuse.md`
- **ADRs:** ADR-0005, ADR-0017, ADR-0003
- **Flows / journeys:** —

## Goal

Find out, before any real filesystem code is written, whether FUSE mounts can be tested in
pull-request CI, and leave a working harness behind. ADR-0017 assumes VFS tests mount a real
filesystem; if hosted runners cannot do that, the test strategy needs changing now, not in M2.

## In scope

- A minimal read-only filesystem using `fuser`, inside the test code only: one directory, one
  file with fixed content.
- An integration test that mounts it unprivileged on a temporary directory, lists the
  directory, reads the file, and unmounts cleanly, including when the test fails.
- A CI job that installs what is needed and runs the test on a GitHub-hosted runner.
- `docs/process/ci-fuse.md`: what was found, how the job is set up, and how to run the test
  locally.

## Out of scope

- Any part of the real filesystem (M2).

## Acceptance criteria

- AC1: The test passes locally through `just test`.
- AC2: The test leaves no mount behind, whether it passes, fails or panics.
- AC3: The test is skipped with a clear message, not failed, when `/dev/fuse` or
  `fusermount3` is unavailable, and CI is configured so that a skip there is a failure.
- AC4: The CI job's result on a GitHub-hosted runner is recorded in `docs/process/ci-fuse.md`
  as observed fact. If it cannot work on hosted runners, the document says so and proposes
  alternatives; the WP is still complete.
- AC5: Any `unsafe` has a `SAFETY:` comment; none is expected.

## Required tests

- The mount test itself.

## Notes

You cannot observe the hosted runner from your worktree. Prepare the job, state in your
report that AC4 is pending a CI run, and the lead will run it and record the result with you
or in a follow-up.

## Open questions

None.
