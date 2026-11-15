# WP-017: State store schema version 2

- **Status:** blocked on WP-014
- **Milestone:** M1
- **Wave:** 6
- **Depends on:** WP-014
- **Gates:** —
- **Owned paths:** `crates/mydrive-store/**`, `docs/architecture/store-schema.md`
- **ADRs:** ADR-0006, ADR-0007, ADR-0011, ADR-0015, ADR-0020, ADR-0021, ADR-0022
- **Flows / journeys:** F02, F05, F12, F13

## Goal

Migrate the store to version 2: the local ID that names content files, the facts the engine
needs about accounts and mounts, durable staging for delta runs, and listing queries that
page by name. Pending-operation records are the next package (WP-018), deliberately separate
so each migration can be reviewed properly.

## In scope

- **Local ID (ADR-0021).** Every item row has a `local_id`: assigned at insert, unique,
  unchanged by `rekey_item`, `complete_upload` and local moves, and never reused after a row
  is removed, across restarts. Existing rows receive one in the migration.
- **Accounts.** Kind (personal or work), user name, display name, organisation name, a
  needs-sign-in flag, and a flag recording that the tenant refuses `#` and `%` in names.
  A listing of accounts.
- **Mounts.** Drive type and display name; paused and paused-until; blocked reason as a code
  with a separate message; a read-only reason beside the existing flag; delta run state (which
  kind of run is in progress, the next link, and a staged delta link awaiting apply) beside
  the live token. Invariants as table constraints. No stored "mount state" column: provide
  one pure function that derives the state from these facts.
- **Delta staging (ADR-0022).** A staging table keyed by mount and item ID holding one entry
  per item for the run in progress (server columns, deleted flag, facet, root flag, sequence,
  and a problem marker for unusable entries); operations to begin a run, stage a page with its
  next link atomically, count staged rows, walk the staged set parents first, list items the
  staged set does not contain (for resync), and finish a run (remove the staged rows, clear
  the run state and set the live token, together).
- **Listing.** An index on (drive, parent, name); `children_after(parent, after_name, limit)`
  in byte order of name; `child_by_name`; an item count per drive.
- **Mount removal.** Remove a mount's rows; refused, naming the items, while any item of the
  mount is dirty, unless the caller states that local-only content is being kept elsewhere.
- **Item kinds.** Accept `package` as a kind (validated in Rust, as now).
- `docs/architecture/store-schema.md` updated throughout.

## Out of scope

- Pending operations, upload session records, local delete, adopt and detach operations,
  retry bookkeeping (WP-018).
- Any engine logic.

## Acceptance criteria

- AC1: A version 1 database containing accounts, mounts, items in every state and hydrated
  ranges opens at version 2 with every row and range intact, every item holding a distinct
  `local_id`, and `PRAGMA foreign_key_check` empty. A version 1 database built by the released
  version 1 code path (not hand-written SQL) is used for this test.
- AC2: `local_id` is never reused: after inserting and removing items across several opens of
  the store, a new item never receives an ID any earlier row held (ADR-0021 R1). It is
  unchanged by `rekey_item`, `complete_upload` with a new server ID and `move_item_local`
  (ADR-0021 R2).
- AC3: `children_after` returns names strictly after the given name in byte order, uses the
  new index (shown by `EXPLAIN QUERY PLAN`), and a 256-row paging loop over 50,000 children
  delivers each child that exists throughout exactly once while other children are inserted,
  renamed and removed between pages (ADR-0020 R1, R2).
- AC4: `child_by_name` is exact and byte-wise.
- AC5: Staging a page commits its entries and next link together; a dropped transaction
  leaves neither (ADR-0022 R1). Staging the same item twice in one run keeps the later entry
  (ADR-0022 R2). Staging changes no item row (ADR-0022 R3).
- AC6: Finishing a run sets the live token, clears run state and removes the staged rows in
  one operation; the live token is changed by nothing else (ADR-0022 R4). Killing the process
  at any point leaves either the run still in progress with its staged set intact, or the run
  finished.
- AC7: The mount constraints hold: no next link or staged link without a run; never both
  links; no blocked message without a blocked reason; no paused-until without paused.
  Violations through the API are typed errors, not SQL errors.
- AC8: Mount removal is refused, naming the dirty items, when any item of the mount is dirty,
  and never changes rows of another drive.
- AC9: The derived mount state function returns blocked, paused, resyncing or active by the
  documented precedence, tested for every combination of the facts.
- AC10: The existing model-based and crash tests still pass, extended to cover every new
  mutating operation with their invariants.

## Required tests

- A migration test for AC1 and a kill test for AC6.
- Tests doc-commented with each ADR-0021 and ADR-0022 rule implemented here.

## Notes

Read `docs/architecture/store-schema.md` first, especially the invariants and "What the
engine must do". The design sketch is in `docs/backlog/drafts/M1-plan.md` section 3; where it
and this file differ, this file wins. The migration runner already supports table rebuilds
with foreign keys off. If a property cannot be added by `ALTER TABLE`, prove it with a small
test before choosing a rebuild, and say which you did.

## Open questions

None.
