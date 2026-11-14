# State store schema

The state store is the SQLite database at `$XDG_STATE_HOME/mydrive/state.db`. It is the
source of truth for accounts, mounts, items, their state and which bytes of each file are
local (ADR-0006). This file describes schema version 1, built by WP-006 in
`crates/mydrive-store`. The migration SQL in `crates/mydrive-store/src/schema.rs` is
authoritative; this document must change in the same pull request as any migration.

## Connection settings

Set by `Store::open` on every open:

| Setting | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | ADR-0006 R2. Readers never block the single writer. |
| `synchronous` | `FULL` | A commit is on disk before it returns, so the engine's durable dirty mark (ADR-0012 R3) survives power loss as well as a crash. |
| `foreign_keys` | `ON` | Hydrated ranges cannot outlive their item, and follow it when it is re-keyed. |
| `busy_timeout` | 5 s | Tolerates a brief lock from someone inspecting the file with `sqlite3`. |

The file is marked with `PRAGMA application_id = 0x4D594456` (ASCII `MYDV`), set by
migration 1.

Only `mydrived` opens the store, and within it only the engine writes (ADR-0006 R1). The
store does not enforce this at runtime: a `Store` owns one connection, and in the
overview's crate graph only `mydrive-engine` depends on `mydrive-store`.

## Tables

All tables are `STRICT`. Text identifiers are never empty. Every `drive_id` column has a
`CHECK (drive_id = lower(drive_id))`, the stored form of ADR-0007 R1.

### `accounts`

| Column | Type | Null | Notes |
|---|---|---|---|
| `account_id` | TEXT | no | Primary key. |
| `tenant_id` | TEXT | yes | `NULL` for a personal account. |

### `mounts`

| Column | Type | Null | Notes |
|---|---|---|---|
| `mount_id` | TEXT | no | Primary key. Names the backing directory (ADR-0007 R5). |
| `account_id` | TEXT | no | References `accounts`. |
| `drive_id` | TEXT | no | Lowercase. |
| `root_item_id` | TEXT | no | The drive root, or a folder within the drive. |
| `mount_point` | TEXT | no | UTF-8 path the user sees. Not a key. |
| `delta_token` | TEXT | yes | The mount's one delta token (ADR-0007 R2). `NULL`: none saved yet. Never logged; `Mount`'s `Debug` redacts it. |
| `read_only` | INTEGER | no | 0 or 1. |
| `blocked_reason` | TEXT | yes | Why policy blocks the mount; `NULL` when not blocked. |

`UNIQUE (drive_id, root_item_id)`: the same remote folder cannot be mounted twice
(ADR-0007 R4). The store refuses it with an error naming the existing mount.

The mount point has no unique constraint: paths are never keys (ADR-0006 R4, ADR-0007 R6).
Refusing a mount point that is in use, nested or overlapping is the engine's check.

### `items`

Primary key (`drive_id`, `item_id`) (ADR-0006 R4). `WITHOUT ROWID`.

| Column | Type | Null | Owner | Notes |
|---|---|---|---|---|
| `drive_id` | TEXT | no | key | Lowercase. |
| `item_id` | TEXT | no | key | Opaque, stored exactly as Graph gives it, or a provisional value the engine generated (see `is_provisional`). |
| `parent_item_id` | TEXT | yes | server | Parent in the same drive. `NULL` for the drive root. No foreign key: see invariants. |
| `name` | TEXT | no | server | Not empty, not `.` or `..`, no `/` or NUL. May exceed 255 bytes (F02 stores such items with sync `error`). |
| `kind` | TEXT | no | server | `file` or `folder`. Validated by the Rust type, not by a SQL `CHECK`, so a new kind needs no table rebuild. |
| `size` | INTEGER | no | server | Bytes, `>= 0`. Also set by the local-write path. |
| `created_at` | INTEGER | no | server | Unix seconds, UTC. Whole seconds (ADR-0011 R5). |
| `modified_at` | INTEGER | no | server | Unix seconds, UTC. |
| `etag` | TEXT | yes | server | `NULL` until the server has the item. |
| `ctag` | TEXT | yes | server | Business delta omits it. |
| `server_hash` | TEXT | yes | server | quickXorHash the server reports, base64 as Graph sends it (ADR-0012). |
| `local_hash` | TEXT | yes | local | quickXorHash of the bytes we uploaded, base64 (ADR-0012). |
| `hydration` | TEXT | no | local | `placeholder`, `partial`, `full`. |
| `pin` | TEXT | no | local | The item's own pin: `inherit`, `pinned`, `unpinned`. Never the effective pin. |
| `sync` | TEXT | no | local | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked`. |
| `last_error` | TEXT | yes | local | Most recent error for the item. |
| `is_provisional` | INTEGER | no | local | 1 for an item created locally that has no server ID yet. Default 0. |

The three state columns hold the wire forms of `mydrive-core`'s `Hydration`, `Pin` and
`Sync`, each in its own column (ADR-0006 R5, ADR-0008 R1). A `CHECK` lists the allowed
values; a unit test keeps those lists equal to `mydrive-core`'s `WIRE_FORMS`. No column holds
a display status.

Indexes: `items_by_parent (drive_id, parent_item_id)` for children, deletes and path
resolution; `items_by_sync (sync)` for listing items by sync state, for example every
`pending-upload` item at start-up (ADR-0012 R11).

### `hydrated_ranges`

The authoritative map of which bytes of a file are local (ADR-0006 R6). Primary key
(`drive_id`, `item_id`, `range_start`). `WITHOUT ROWID`.

| Column | Type | Null | Notes |
|---|---|---|---|
| `drive_id` | TEXT | no | With `item_id`, references `items` `ON UPDATE CASCADE`. |
| `item_id` | TEXT | no | |
| `range_start` | INTEGER | no | First byte, `>= 0`. |
| `range_end` | INTEGER | no | One past the last byte; `> range_start`. |

Ranges are half-open, `[range_start, range_end)`.

## Writing items

There is no general upsert, because one would overwrite local-only columns with whatever
the caller happened to hold.

| Operation | Writes |
|---|---|
| `insert_item` | Every column of a new item. Refused if the key exists. |
| `apply_remote_metadata` | Server-owned columns only (parent, name, kind, size, timestamps, eTag, cTag, server hash), as described below. |
| `set_sync` | `sync`, as a compare-and-set against the value the caller expects. |
| `set_pin`, `set_hydration`, `set_last_error` | That one column. |
| `set_hashes` | `local_hash` and `server_hash`, for example after an upload, before the upload response is applied with `apply_remote_metadata`. |
| `set_local_size` | `size`, on the local-write path. Refused if recorded ranges reach past the new size. |
| `rekey_item` | A provisional item's `item_id`, its children's `parent_item_id` and its ranges' `item_id`, together, and clears `is_provisional`. Refused if the item is not provisional or the new key exists. |

`apply_remote_metadata` first decides whether the content identity changed: the server
hash differs, or, where neither side has a hash, the eTag or size differs; a change of kind
also counts. Then:

| Content | Item's sync | Effect | Result |
|---|---|---|---|
| unchanged | any | All server-owned columns written. | `ContentUnchanged` |
| changed | `in-sync`, `pending-download` | All server-owned columns written; ranges cleared; hydration set to `placeholder` (`full` for a folder). One savepoint. | `ContentReplaced` |
| changed | `pending-upload`, `uploading`, `conflict`, `error`, `blocked` | Only parent and name written. Kind, size, timestamps, eTag, cTag, server hash, ranges and every local column are kept. | `ContentChangedUnderLocalState` |

In the last case the stored eTag stays the old one, so the next upload still fails with
412 (ADR-0012 R6, R7) even if the engine crashes before it handles the conflict. `error`
and `blocked` are treated as pending because version 1 has no pending-operation records to
say whether a local change hides behind them.

A folder that has children is never turned into a file.

## Invariants

The store maintains these; nothing else writes the tables.

1. **Items are identified by (`drive_id`, `item_id`) only.** No operation takes a path as an
   item's identity. A path is derived on demand by walking `parent_item_id` up to a mount's
   `root_item_id`; it is never stored.
2. **A rename or move is one row update** of the item's `name` or `parent_item_id`.
   Descendants are untouched, and their resolved paths follow.
3. **No parent foreign key.** Delta does not deliver parents before children, pages commit
   separately, and a folder mount's root has a parent outside the mount. A parent may
   therefore be missing for a while, and a batch of moves may pass through a transient
   cycle. Path resolution reports a missing ancestor, an item outside the mount and a
   parent cycle as errors; it always terminates.
4. **Ranges describe the content version the item's hashes describe.** For an item with no
   local change pending, the ranges are bytes of the version `server_hash` (or, without a
   hash, `etag` and `size`) identifies; when the server reports a different version, the
   ranges are cleared in the same operation that records it. For an item with a local
   change pending, the ranges are bytes of the local content, and the stored server
   columns stay those of the version that local change was based on.
5. **Ranges are stored merged.** For one item no two ranges overlap or touch. Recording a
   range replaces every stored range it overlaps or touches with their union, in one
   savepoint. A zero-length range records nothing. "Is this range present" is true only if
   one stored range covers every byte of it; a zero-length query is trivially present.
6. **Ranges fit the item.** Every range ends at or before the item's `size`; a range past
   the end is refused, and so is a query past the end. Folders have no ranges. A local size
   change that would strand ranges is refused; a remote one clears them (invariant 4) or
   is not applied.
7. **Deletes.** Deleting an item deletes its ranges. Deleting an item with children is
   refused unless the caller asks for a recursive delete, which removes the whole subtree
   and its ranges and terminates on a parent cycle. A delete that would remove any item
   whose sync is `pending-upload`, `uploading` or `conflict` is refused with an error
   naming every such item, unless the caller passes `ServerConfirmedLocalDelete`: the
   server confirmed a delete the user made locally. Deleting rows never touches a backing
   directory.
8. **Every operation is atomic.** An operation that runs several statements runs them in
   its own savepoint, so it applies fully or not at all, whether called on its own or
   inside a caller's transaction. A refused operation leaves the caller's transaction
   usable.
9. **Transactions.** A caller's batch (for example a delta page plus its next token) runs
   in one `BEGIN IMMEDIATE` transaction. It is visible only after commit, and absent if the
   transaction is dropped or the process dies first. If SQLite rolls the transaction back
   by itself (`SQLITE_FULL`, an I/O error, out of memory), every later call on it,
   including commit, fails with `TransactionAborted`, so no later statement can commit on
   its own; the caller retries the batch.

## Migrations

- The schema version is SQLite's `user_version`. Version 0 is an empty database.
- Migrations are numbered from 1; each number is greater than every earlier one
  (ADR-0006 R3). There are no down-migrations.
- A migration's SQL is frozen once released. A schema change, including a new state value
  in a `CHECK` list, is a new migration with a test that runs it on a copy of the previous
  schema.
- On open, every migration newer than the database runs in one transaction together with
  the `user_version` update, so a crash leaves the old version untouched or the new version
  complete.
- Before anything is written, the file is checked over a read-only connection, which never
  checkpoints a WAL into the main file. It is refused, byte-identical, when:
  - it is not SQLite, its `application_id` is not ours, or it is at version 0 with tables
    in it (`NotAStateStore`);
  - its `user_version` is greater than the build's `SCHEMA_VERSION` (`SchemaTooNew`).
- WAL mode is set only after those checks.

| Version | Change |
|---|---|
| 1 | `application_id`; `accounts`, `mounts`, `items`, `hydrated_ranges`; indexes `items_by_parent`, `items_by_sync`. |

## Not in version 1

To be added by a version 2 migration in the M1 engine work, not by editing version 1:

- Mount state (`active`, `paused`, `blocked`, `resyncing`) and account state
  (`signed-in`, `needs-sign-in`). Version 1 stores only a mount's blocked reason.
- F02's saved `nextLink` and the pending-versus-live delta token. Version 1 has one
  `delta_token` per mount.
- F02's per-item "materialised" flag.
- F13's pending-operations table, with base eTags.
- An error retry count.
- A read-only reader connection for the VFS.

Also not in version 1: per-folder counters for aggregate folder status (F13 open
question 1).
