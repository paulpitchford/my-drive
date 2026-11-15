# WP-015: Clock and conflict-copy names

- **Status:** blocked on WP-014
- **Milestone:** M1
- **Wave:** 6
- **Depends on:** WP-014
- **Gates:** —
- **Owned paths:** `crates/mydrive-core/src/clock.rs`, `crates/mydrive-core/src/conflict.rs`
- **ADRs:** ADR-0012, ADR-0017
- **Flows / journeys:** F07, J10

## Goal

Two small pure pieces the engine needs everywhere: an injected clock, so every timer in the
engine can be tested without real time, and the rule that names a conflict copy.

## In scope

- `clock.rs`: a `Clock` trait with a monotonic "now", a wall-clock "now" in whole Unix
  seconds, and a way to wait until a monotonic instant that returns a boxed future (no async
  runtime in core). A `ManualClock` whose `advance` wakes waiters whose deadline has passed.
- `conflict.rs`: a pure function that, given a file name, a host name, the name context and a
  predicate saying whether a candidate name is taken, returns the conflict-copy name.

## Out of scope

- A real system clock (the engine provides one on tokio, WP-029).
- Deciding when a conflict exists (WP-036).

## Acceptance criteria

- AC1: `report.docx` with host `laptop` gives `report-laptop.docx`; `Makefile` gives
  `Makefile-laptop`; `archive.tar.gz` gives `archive.tar-laptop.gz` (the split is at the last
  dot); a leading-dot name such as `.profile` gives `.profile-laptop`.
- AC2: If the candidate is taken the result is `…-laptop-2`, then `-3`, and so on; the result
  is never a name the predicate reports as taken.
- AC3: Characters in the host name that `names::validate_name` forbids are replaced with `_`;
  a host with no usable character becomes `device`.
- AC4: For any host string and any name that passes `validate_name` in a context, the result
  passes `validate_name` in the same context and is at most 255 bytes of UTF-8, shortened at a
  character boundary in the stem (never in the extension or the suffix).
- AC5: `ManualClock`: a waiter for instant T completes exactly when `advance` reaches or
  passes T and not before; two waiters complete in deadline order; advancing wall time and
  monotonic time are independent.
- AC6: No dependency is added; nothing in either file does I/O or reads the system time.

## Required tests

- Property tests for AC2 and AC4.
- A test doc-commented `ADR-0012 R7` for the naming rule.

## Notes

The 255-byte bound is a local file-name limit, not a OneDrive rule; say so in the doc
comment. `validate_name` and `NameContext` are in `crates/mydrive-core/src/names.rs`.

## Open questions

None.
