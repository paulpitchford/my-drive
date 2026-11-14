# WP-002: Core state model

- **Status:** done (#3)
- **Milestone:** M0
- **Wave:** 1
- **Depends on:** —
- **Gates:** —
- **Owned paths:** `crates/mydrive-core/src/state.rs`
- **ADRs:** ADR-0008
- **Flows / journeys:** F13, F06

## Goal

Implement the three-axis item state model and the single function that derives what the user
sees from it. Every other component (store, engine, VFS, IPC, adapters) depends on these
types meaning exactly what `docs/architecture/overview.md` says.

## In scope

- Enums `Hydration` (`Placeholder`, `Partial`, `Full`), `Pin` (`Inherit`, `Pinned`,
  `Unpinned`), `Sync` (`InSync`, `PendingDownload`, `PendingUpload`, `Uploading`, `Conflict`,
  `Error`, `Blocked`), and `ItemState` holding one of each.
- `EffectivePin` (`Pinned` or `Unpinned`) and a function resolving it from an item's own pin
  and its ancestors' pins, nearest first; the root default is `Unpinned`.
- `DisplayStatus` (`Blocked`, `Error`, `Syncing`, `Pinned`, `Local`, `OnlineOnly`) and
  `Modifiers { read_only, shortcut }`.
- One function deriving `DisplayStatus` from `ItemState` and `EffectivePin`, following the
  priority table in the overview exactly.
- A function deciding whether an item may be dehydrated, returning a typed refusal reason
  (pinned; not in sync) or success.
- Stable string forms for every enum, used by the xattr mirror and the IPC protocol:
  lowercase kebab-case of the names in the overview (`online-only`, `pending-upload`, ...),
  with `Display`, `FromStr` and `serde` agreeing.

## Out of scope

- Transitions between states caused by events (engine WPs in M1 and M2).
- Persisting state (WP-006).

## Acceptance criteria

- AC1: For every one of the 42 combinations of hydration, effective pin and sync, the
  derived display status matches the overview's table, including its priority order.
- AC2: Effective pin: own `Pinned`/`Unpinned` wins; `Inherit` takes the nearest ancestor that
  is not `Inherit`; with none, the result is `Unpinned`.
- AC3: Dehydration is refused when effective pin is `Pinned`, and refused when sync is
  anything other than `InSync`; each refusal carries its reason.
- AC4: Dehydration is never permitted for `PendingUpload`, `Uploading` or `Conflict`,
  whatever the other axes say.
- AC5: Every enum value round-trips through its string form and through serde, and
  unknown strings are rejected with a typed error.
- AC6: The display derivation exists in exactly one function.

## Required tests

- An exhaustive table-driven test for AC1 written out explicitly (expected values listed in
  the test, not computed by the code under test).
- Property test for AC2 over arbitrary ancestor chains.
- A test for AC4 doc-commented as the data-safety rule it protects.
- Tests doc-commented with each ADR-0008 rule they cover.

## Notes

The axes are stored separately and collapsed only for display. Do not add a combined enum to
"simplify" storage or the protocol.

## Open questions

None.
