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
| `size` | INTEGER | no | server / local | Bytes, `>= 0`. While the item is dirty it describes the local content and only the local-write path changes it. |
| `created_at` | INTEGER | no | server | Unix seconds, UTC. Whole seconds (ADR-0011 R5). Not written while dirty. |
| `modified_at` | INTEGER | no | server | Unix seconds, UTC. Not written while dirty. |
| `etag` | TEXT | yes | server | `NULL` until the server has the item. |
| `ctag` | TEXT | yes | server | Business delta omits it. |
| `server_hash` | TEXT | yes | server | quickXorHash the server reports, base64 as Graph sends it (ADR-0012). |
| `local_hash` | TEXT | yes | local | quickXorHash of the bytes we uploaded, base64 (ADR-0012). |
| `hydration` | TEXT | no | local | `placeholder`, `partial`, `full`. |
| `pin` | TEXT | no | local | The item's own pin: `inherit`, `pinned`, `unpinned`. Never the effective pin. |
| `sync` | TEXT | no | local | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked`. |
| `last_error` | TEXT | yes | local | Most recent error for the item. |
| `is_provisional` | INTEGER | no | local | 1 for an item created locally that has no server ID yet. Default 0. |
| `local_dirty` | INTEGER | no | local | 1 while the backing file holds content that has not reached the server. Every guard against losing local content keys on this, never on `sync`. Default 0. |
| `local_generation` | INTEGER | no | local | Bumped on every local content change. An upload records the generation it sent, so completing it cannot mark a later version as synced. Default 0. |
| `remote_changed` | INTEGER | no | local | 1 when the server reported different content while the item was dirty, so the server columns were not updated and still describe the version the local change is based on. Durable, so conflict handling survives a restart. Default 0. |

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

| Operation | Writes | Refused when |
|---|---|---|
| `insert_item` | Every column of a new item. | The key exists. |
| `apply_remote_metadata` | Server-owned columns, as below. | Unknown item; a folder with children would become a file. |
| `mark_local_change` | Sets `local_dirty`, bumps `local_generation`. Commit it before the change touches the backing file (ADR-0012 R3). | Unknown item. |
| `set_local_size` | Marks a local change and sets `size` (a write that grows or keeps the size). | Recorded ranges reach past the new size; folders. |
| `truncate_local` | Marks a local change, sets `size` and trims the ranges to it. | Folders. |
| `complete_upload` | Re-keys a provisional item, writes `local_hash` and the uploaded version's server columns, and clears `local_dirty` only if the generation still matches. See below. | No server ID for a provisional item; a response for another item; the server ID already stored. |
| `discard_local_changes` | Clears `local_dirty` and the ranges, sets hydration `placeholder` and sync `in-sync`; keeps `remote_changed`. The only other way to clear `local_dirty`. | Unknown item. |
| `dehydrate` | Clears the ranges and sets hydration `placeholder`, together. | The item is dirty, not `in-sync`, or a folder. Effective pin is the engine's check (ADR-0008 R5). |
| `clear_hydrated_ranges` | Clears the ranges. | The item is dirty. |
| `set_hydration` | `hydration`. | Lowering a dirty item to `placeholder` or `partial`. |
| `set_sync` | `sync`, as a compare-and-set against the value the caller expects. | The stored value differs. |
| `set_pin`, `set_last_error` | That one column. | Unknown item. |
| `move_item_local` | `parent_item_id` and `name` for a local rename or move; works on provisional items. | Invalid name. |
| `rekey_item` | A provisional item's `item_id`, its children's `parent_item_id` and its ranges' `item_id` (by `ON UPDATE CASCADE`), together, and clears `is_provisional`. | Not provisional; the new key exists. |
| `delete_item` | Removes the item (or subtree) and its ranges. | Children without `Recursive`; any dirty item in the delete without `ServerConfirmedLocalDelete`; any mount root in the delete, always. |

### Remote metadata

`apply_remote_metadata` first decides whether the content identity changed, comparing the
report with the stored server columns: the server hash differs; or, where neither side has
a hash, the cTag differs when both have one (a rename changes the eTag but not the cTag),
otherwise the eTag differs; or the size differs; a change of kind also counts. Then:

| Item | Content | Effect | Result |
|---|---|---|---|
| clean | unchanged | All server-owned columns written; `remote_changed` cleared. | `ContentUnchanged` |
| clean | changed | As above, and ranges cleared and hydration set to `placeholder` (`full` for a folder), in one savepoint. | `ContentReplaced` |
| dirty | unchanged | Only parent and name written. | `ContentUnchanged` |
| dirty | changed | Only parent and name written; `remote_changed` set. | `ContentChangedUnderLocalState` |

On a dirty item nothing describing content is written: not size, timestamps, eTag, cTag or
server hash. Size and ranges keep describing the local bytes, and the stored eTag stays the
one the local change was based on, so the next upload fails with 412 (ADR-0012 R6, R7) if
the server content moved on, even if the engine crashes before it handles the conflict.
The cost: a remote rename of a dirty item also changes its eTag on the server, so that
upload can meet a 412 and produce a conflict copy that was not strictly needed. That is
safe, not lossy.

### Uploads

`complete_upload(key, expected_generation, uploaded_local_hash, response, new_item_id)`
runs in one savepoint:

1. A provisional item takes `new_item_id` (as `rekey_item`). A provisional item without
   one, a non-provisional item with a different one, or a `response` for another key is
   refused.
2. `local_hash` becomes the uploaded hash; eTag, cTag, server hash, parent and name come
   from `response`; `remote_changed` is cleared. The next upload's `If-Match` is therefore
   the uploaded version's eTag.
3. If `local_generation == expected_generation`: size and timestamps come from `response`,
   ranges are trimmed to the server size, `local_dirty` is cleared and sync set `in-sync`
   (`Synced`).
4. Otherwise the user changed the file while it was uploading: size, timestamps and ranges
   keep describing the local content, the item stays dirty, and sync is set
   `pending-upload` (`LocalChangedSince`). Upload 1 completing can never mark version 2 as
   synced.

### Recovery: the server item arrives before the re-key

If the daemon dies after the server accepted the upload of a provisional item but before
`complete_upload` committed, the next delta can report the new server item. The engine
sees an unknown key and inserts it as a clean item, so the store then holds the provisional
row (dirty, with the content) and the server row (clean). `complete_upload` for the
provisional row is then refused with `ItemExists`, and nothing changes. Recovery, until
the M1 "adopt server item" operation exists: the server row is clean, so the engine deletes
it with `delete_item(.., ItemOnly, Refuse)` (the guard confirms it is clean), then calls
`complete_upload` again with the response, or re-uploads if the response was lost. Because
the provisional row stays dirty throughout, no step can lose its content.

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
4. **Ranges describe the content version the item's hashes describe.** For a clean item,
   the ranges are bytes of the version `server_hash` (or, without a hash, `ctag`/`etag` and
   `size`) identifies; when the server reports a different version, the ranges are cleared
   in the same operation that records it. For a dirty item, the ranges and `size` describe
   the local content, and the stored server columns stay those of the version the local
   change was based on (or of the last version uploaded); `remote_changed` says the server
   has moved on since.
5. **Unsynced local content is never released or forgotten.** `local_dirty` is set, and
   committed, before a local change touches the backing file, and is cleared only by a
   `complete_upload` of the current generation or by `discard_local_changes`. While it is
   set: ranges cannot be cleared, hydration cannot be lowered, the item cannot be
   dehydrated, remote metadata cannot change its size, timestamps or content identity, and
   it cannot be deleted without `ServerConfirmedLocalDelete`. Every guard keys on
   `local_dirty`, never on `sync`.
6. **Ranges are stored merged.** For one item no two ranges overlap or touch. Recording a
   range replaces every stored range it overlaps or touches with their union, in one
   savepoint. A zero-length range records nothing. "Is this range present" is true only if
   one stored range covers every byte of it; a zero-length query is trivially present.
7. **Ranges fit the item.** Every range ends at or before the item's `size`; a range past
   the end is refused, and so is a query past the end. Folders have no ranges.
   `set_local_size` refuses to strand ranges; `truncate_local` trims them; a remote size
   change on a clean item clears them (invariant 4) and is not applied to a dirty one.
8. **Deletes.** Deleting an item deletes its ranges. Deleting an item with children is
   refused unless the caller asks for a recursive delete, which removes the whole subtree
   and its ranges and terminates on a parent cycle. A delete that would remove any dirty
   item, whatever its sync state, is refused with an error naming every such item, unless
   the caller passes `ServerConfirmedLocalDelete`: the server confirmed a delete the user
   made locally. A delete that would remove a mount's root item is always refused.
   Deleting rows never touches a backing directory.
9. **Every operation is atomic.** An operation that runs several statements runs them in
   its own savepoint, so it applies fully or not at all, whether called on its own or
   inside a caller's transaction. A refused operation leaves the caller's transaction
   usable. If rolling back to the savepoint itself fails, the whole transaction is rolled
   back, so the caller's `Transaction` reports `TransactionAborted` from then on.
10. **Transactions.** A caller's batch (for example a delta page plus its next token) runs
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
- F13's pending-operations records (create, update content, move, delete), with base
  eTags. Until then a local rename (`move_item_local`) or local create is not itself
  recorded as pending, and a clean item's local move can be overwritten by a remote
  `apply_remote_metadata` before it is uploaded.
- An error retry count.
- A read-only reader connection for the VFS.
- An "adopt server item" operation that merges a server row into a provisional one (see
  "Recovery" above).
- Restoring an item after a refused or reverted delete.
- Mount removal (deleting a mount row and its items without touching remote content,
  ADR-0015).

Also not in version 1: per-folder counters for aggregate folder status (F13 open
question 1).
