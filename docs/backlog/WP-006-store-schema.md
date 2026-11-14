# WP-006: State store schema v1

- **Status:** blocked on wave 1
- **Milestone:** M0
- **Wave:** 2
- **Depends on:** WP-001, WP-002
- **Gates:** —
- **Owned paths:** `crates/mydrive-store/**`, `docs/architecture/store-schema.md`
- **ADRs:** ADR-0006, ADR-0007, ADR-0008, ADR-0012
- **Flows / journeys:** F02, F13

## Goal

Create the SQLite state store that is the source of truth for everything the daemon knows:
accounts, mounts, items, their state, and which byte ranges of each file are local. If this
is wrong, nothing above it can be data-safe.

## In scope

- Opening the store at a given path: WAL mode, foreign keys on, `synchronous=FULL`.
- Forward-only migrations tracked with `user_version`; opening a store newer than the code
  understands fails with a typed error and changes nothing.
- Schema v1: accounts; mounts (mount ID, account, drive ID, root item ID, mount point, delta
  token, read-only flag, blocked reason); items keyed by (`drive_id`, `item_id`) with parent
  item ID, name, kind, size, timestamps, eTag, cTag (nullable), server hash, local hash, the
  three state axes, own pin, last error; hydrated byte ranges per item.
- Typed operations: upsert and fetch an item by key; list children; delete an item and its
  ranges; record and merge hydrated ranges; query whether a range is fully present; clear an
  item's ranges; read and write a mount's delta token; resolve an item's path within its
  mount by walking parents; list items by sync state.
- A transaction API so a caller can apply a batch (for example one delta page plus its new
  token) atomically.
- `docs/architecture/store-schema.md`: tables, columns, invariants, migration rules.

## Out of scope

- Any decision about when state changes (engine).
- The xattr mirror (VFS, M2).

## Acceptance criteria

- AC1: Items are addressed only by (`drive_id`, `item_id`); no public function takes a path
  as an item's identity.
- AC2: A batch applied in a transaction is fully visible after commit and fully absent if the
  transaction is dropped or the process is killed before commit (tested by dropping without
  commit and by reopening the database).
- AC3: Hydrated ranges merge when adjacent or overlapping; "is range present" is true only
  when every byte of the query range is covered; zero-length and out-of-order inputs are
  handled; ranges cannot exceed the item's size.
- AC4: Renaming or moving a folder is one row update, and every descendant's resolved path
  changes accordingly.
- AC5: Deleting an item removes its ranges; deleting a folder with children is refused unless
  the caller asks for a recursive delete explicitly.
- AC6: Opening a database with a higher schema version fails without modifying it.
- AC7: State axes are stored in three separate columns using the string forms from
  `mydrive-core`.
- AC8: Path resolution terminates on a parent cycle and reports it as an error.

## Required tests

- Property test for AC3 against a simple model (a set of bytes).
- A migration test: create at version 0, migrate, verify schema.
- Tests doc-commented with each ADR-0006 rule.

## Notes

Use the `bundled` feature of `rusqlite` in this crate's manifest so behaviour does not
depend on the system SQLite. The daemon will be the only process opening the store, with a
single writer.

## Open questions

None.
