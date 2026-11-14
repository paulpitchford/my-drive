# WP-013: Wave 2 test follow-ups

- **Status:** done (#20 store, #21 sign-in, #22 protocol)
- **Milestone:** M0
- **Wave:** 3
- **Depends on:** WP-005, WP-006, WP-009
- **Gates:** —
- **Owned paths:** `crates/mydrive-ipc/**`, `crates/mydrive-store/**`,
  `crates/mydrive-graph/src/auth/**`, `crates/mydrive-graph/tests/auth_*.rs`,
  `docs/protocol/**`, `.github/workflows/ci.yml`
- **ADRs:** ADR-0006, ADR-0009, ADR-0010, ADR-0019, ADR-0020, ADR-0017
- **Flows / journeys:** —

## Goal

The final verifications of wave 2 passed, but each left a short list of places where a test
does not fully prove what it claims, or would hang instead of fail. None is a known defect.
Close them before more code is built on these crates, so a future regression fails a test.

## In scope

Protocol (`mydrive-ipc`):

- A test for the `item.status` path-count limit on its own (10,001 one-byte paths), separate
  from the byte limit.
- The bounds test must also fail for a method that exists in the request type but is missing
  from the method list.
- The README-examples test must not skip silently in CI: CI installs the tools it needs and
  sets a variable that turns a skip into a failure.
- A forged mount or account cursor naming a key that cannot exist is refused.
- Catalogue wording: the shell loop streams entries, so a consumer must check its exit
  status; the Python loop reports a non-JSON reply cleanly; cursor-size figures state the
  assumptions they rest on; `seq`, `bytes_freed` and `skipped_count` are documented as
  integers a client may receive above 2^53; `seq` is optional and the tie rule is stated;
  the refused numeric forms of an id are listed; duplicate keys in a request are documented.

State store (`mydrive-store`):

- `complete_upload` atomicity: a refusal after the re-key step leaves nothing changed.
- The `complete_upload` re-key test includes a child item and its ranges.
- A table-rebuild migration run through the real `Store::open` path, so the
  foreign-keys-off step is covered before any version 2 migration exists.

Sign-in (`mydrive-graph` auth):

- `LoopbackSignIn::start` refusing a `[::1]` redirect host when IPv6 loopback cannot be
  bound, tested through `start` (needs a test hook for "IPv6 unavailable").
- `close()` waiting for a stopped writer, asserted without relying on yields.
- The configured prompt timeout is asserted, not merely exercised.
- Every test that can only fail by hanging gets a bounded wait, so CI shows a failure and
  not a stuck job.
- Document on `close()` that a refresh already in flight still returns its access token to
  waiting callers and that its rolled refresh token is not stored.

## Out of scope

- Any behaviour change. If closing a gap reveals a defect, stop and report it.

## Acceptance criteria

- AC1: Each item above has a test, and each test is shown to fail when the behaviour it
  covers is removed.
- AC2: No test in the three crates can hang without a bound.
- AC3: `just check` passes, and CI runs the README examples without skipping.
- AC4: No production behaviour changes; any production line touched is listed in the pull
  request with the reason.

## Required tests

- As listed.

## Notes

Three implementers can take one crate each; the owned paths do not overlap between crates.

## Open questions

None.
