# ADR-0017: Testing: a Graph simulator, fault injection, journey-based acceptance tests

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

## Context

The overview's first two principles are that the client never loses or corrupts data and
never fails silently. The failures that matter most are hard to reach against the real
service: write-path corruption on save and rename (onedriver's top issue class,
[issues index](https://issues.ecosyste.ms/hosts/GitHub/repositories/jstaf%2Fonedriver/issues?page=2)),
`410 Gone` resyncs, repeated items in a delta feed
([delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)),
throttling with `Retry-After`
([throttling guidance](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)),
a server that ignores `Range` and returns 200
([download](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)),
SharePoint rewriting uploaded files ([rclone OneDrive docs](https://rclone.org/onedrive/)),
and policy-blocked or read-only libraries. Live tests are also limited by throttling and by
needing real accounts, which the owner must approve (ADR-0018).

The code is built by agents in work packages (ADR-0018), so every rule in an ADR needs a test
a reviewer can point to. The overview names a dev-only crate, `mydrive-graph-sim`, that no
non-test code may depend on.

Whether GitHub-hosted runners permit FUSE mounts is not known; it is to be verified in M0.

## Decision

Tests are organised in six layers.

1. **Unit and property tests** for pure logic in `mydrive-core`, using `proptest` where the
   input space is large: display-status derivation from the three axes, pin inheritance, name
   validation and conflict naming.
2. **Graph simulator.** `mydrive-graph-sim` is an in-process fake Graph server used only by
   tests. It implements delta (including both `410 Gone` resync variants and last-occurrence
   semantics), content download with and without `Range` support, upload sessions,
   eTag/`If-Match` conflicts, 429 with `Retry-After`, SharePoint-style post-upload rewriting,
   read-only and blocked libraries, and scripted fault injection (dropped connection, delay,
   5xx, truncated body). Engine integration tests run against it and never against the real
   service.
3. **VFS tests** mount a real FUSE filesystem over a backing directory and exercise it with
   ordinary POSIX calls. Whether these run on GitHub-hosted runners is decided in M0.
4. **Crash tests** kill the daemon at injected points, restart it, and assert invariants: no
   local change is lost, and no placeholder is ever readable as zeros.
5. **Journey acceptance tests** map one-to-one to the acceptance-criteria IDs in
   `docs/journeys/`.
6. **Live tests** run against a dedicated real test account, manually or nightly, never in
   pull-request CI.

Every item under "Rules for implementers" in an ADR has at least one test that names it in the
form `ADR-0012 R3`, in the test name or a comment on it.

There is no coverage percentage target.

CI runs `cargo fmt --check`, `cargo clippy` with warnings denied, the test suite, and
`cargo deny`.

## Alternatives considered

**Recorded HTTP fixtures instead of a simulator.** Cheap to start, but cannot express stateful
behaviour such as delta tokens, upload sessions and eTag changes, or inject faults mid-stream.
Rejected.

**Integration tests against the real service in CI.** Slow, throttled, needs secrets in CI and
cannot produce the rare responses on demand. Kept to the live layer only.

**A coverage percentage target.** Measures lines run, not rules checked. Rejected in favour of
the rule-to-test mapping.

## Consequences

- `mydrive-graph-sim` is a substantial component that must track the real API; a divergence
  between the simulator and Graph is a bug that live tests are there to find.
- If FUSE mounts are not allowed on hosted runners, VFS and crash tests need a self-hosted or
  privileged runner, or run outside pull-request CI.
- Reviewers can check an ADR rule by searching for its tag.
- Open concern: the simulator's behaviour for unknowns (Range on authenticated `/content`,
  errors under unmanaged-device policy) is a guess until gates G1 and G2 report; those cases
  must be marked as provisional in the simulator.

## Rules for implementers

- **R1.** No crate outside tests and dev-dependencies depends on `mydrive-graph-sim`.
- **R2.** No test in layers 1 to 5 makes a network request outside the machine.
- **R3.** Every "Rules for implementers" item in `docs/adr/` is named by at least one test as
  `ADR-NNNN Rn`; a CI check lists any rule with no matching test.
- **R4.** Every acceptance-criteria ID in `docs/journeys/` is named by exactly one journey test.
- **R5.** The simulator's fault injection is scriptable per test and deterministic for a given
  script.
- **R6.** Crash tests assert, after restart, that every local change made before the kill is either
  on the simulated server or recorded as pending, and that no read of a placeholder returns
  zeros in place of content.
- **R7.** Live tests are excluded from the default `cargo test` run and need an explicit opt-in.
- **R8.** CI fails on any `cargo fmt`, `cargo clippy -- -D warnings`, test or `cargo deny` failure.

## Revisit when

- M0 finds out whether hosted runners can mount FUSE.
- Gates G1 or G2 report behaviour the simulator gets wrong.
- Live tests find a Graph behaviour the simulator does not model.
