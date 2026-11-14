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
| `synchronous` | `FULL` | A commit is on disk before it returns, so the durable dirty mark (ADR-0012 R3) survives power loss as well as a crash. |
| `foreign_keys` | `ON` (off only while migrating) | Hydrated ranges cannot outlive their item, and follow it when it is re-keyed. |
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
| `parent_item_id` | TEXT | yes | server / local | Parent in the same drive. `NULL` for the drive root. No foreign key: see invariants. |
| `name` | TEXT | no | server / local | Not empty, not `.` or `..`, no `/` or NUL. May exceed 255 bytes (F02 stores such items with sync `error`). |
| `kind` | TEXT | no | server | `file` or `folder`. Validated by the Rust type, not by a SQL `CHECK`, so a new kind needs no table rebuild. |
| `size` | INTEGER | no | server / local | Bytes, `>= 0`. On a clean item, the server's size. On a dirty item, the local size; only the local-write path changes it. |
| `created_at` | INTEGER | no | server | Unix seconds, UTC. Whole seconds (ADR-0011 R5). Not written while dirty. |
| `modified_at` | INTEGER | no | server | Unix seconds, UTC. Not written while dirty. |
| `etag` | TEXT | yes | server | `NULL` until the server has the item. On a dirty item, the eTag of the version the local change is based on (the next upload's `If-Match`). |
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
| `remote_changed` | INTEGER | no | local | 1 when the server reported different content while the item was dirty, so the server columns were not updated. Durable, so conflict handling survives a restart. Default 0. |
| `remote_etag`, `remote_ctag`, `remote_hash`, `remote_size` | TEXT, TEXT, TEXT, INTEGER | yes | local | While dirty, the last content identity the server reported for the item. Set from the server columns when the item becomes dirty (the base version, with its server size), replaced by every remote report on the dirty item, and `NULL` on a clean item. |

Table CHECKs, so these hold whatever writes the row:

- `is_provisional = 0 OR local_dirty = 1`: a provisional item is always dirty.
- `local_dirty = 0 OR sync NOT IN ('in-sync', 'pending-download')`: a dirty item is never
  reported in sync, and no download is scheduled over it.
- `local_dirty = 1 OR remote_size IS NULL`: a clean item has no recorded remote identity.
- `local_dirty = 0 OR kind = 'folder' OR hydration = 'full'`: a dirty file is fully local.
  The store also keeps its ranges exactly `[0, size)` (invariant 5).
- `remote_changed = 0 OR sync <> 'in-sync'`: an item the server has moved past is never
  reported in sync until fresh metadata is applied.

The three state columns hold the wire forms of `mydrive-core`'s `Hydration`, `Pin` and
`Sync`, each in its own column (ADR-0006 R5, ADR-0008 R1). A `CHECK` lists the allowed
values; a unit test keeps those lists equal to `mydrive-core`'s `WIRE_FORMS`. No column holds
a display status.

Indexes:

- `items_by_parent (drive_id, parent_item_id)` for children, deletes and path resolution.
- `items_by_sync (sync)` for listing items by sync state.
- `items_by_dirty (local_dirty) WHERE local_dirty = 1`, a partial index, for `dirty_items`.

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
| `insert_item` | Every column of a new item. Either a clean server item, or a provisional one, which must be dirty, `pending-upload` and (for a file) `full`; its range `[0, size)` is recorded. | The key exists; a provisional item not dirty and `pending-upload`; a dirty item that is not provisional; a dirty file that is not `full`; any `remote_changed` or recorded remote identity. |
| `apply_remote_metadata` | Server-owned columns, as below. | Unknown item; a folder with children would become a file. |
| `mark_local_change` | Sets `local_dirty`, bumps `local_generation`, moves `in-sync` or `pending-download` to `pending-upload`, and on the clean-to-dirty transition records the base identity. Commit it before the change touches the backing file (ADR-0012 R3). | Unknown item; a file that is not fully local (`full` with ranges covering `[0, size)`): the engine hydrates first (ADR-0012 R1). |
| `set_local_size` | Marks a local change, sets `size` and makes the ranges exactly `[0, size)`: growing extends the range (the grown bytes are local by definition), shrinking trims it. | Folders; a file that is not fully local. |
| `truncate_local` | To 0 (`O_TRUNC`, ADR-0012 R2), on any file: clears the ranges, sets hydration `full` and size 0, marks the change. To any other size: as `set_local_size`. | Folders; a non-zero size on a file that is not fully local. |
| `record_hydrated_range` | Merges a downloaded range into a clean file's map. | Dirty files: their ranges are already `[0, size)`, and a record could only describe a download over unsynced bytes. |
| `complete_upload` | See "Uploads". | The item is not dirty (a replayed completion); no server ID for a provisional item; a response for another item; the server ID already stored. |
| `discard_local_changes(key, current)` | Takes the server's current metadata and makes the item a clean placeholder of that version, in one savepoint: all server columns (size included), ranges cleared, hydration `placeholder`, sync `in-sync`, `local_dirty`, `remote_changed` and the recorded identity cleared. The only other way to clear `local_dirty`. | Provisional items (delete them with `DiscardNeverUploaded`); metadata for another item; stale metadata: `remote_changed` is set and `current` is still the version the stored server columns describe. |
| `dehydrate` | Clears the ranges and sets hydration `placeholder`, together. | The item is dirty, not `in-sync`, or a folder. Effective pin is the engine's check (ADR-0008 R5). |
| `clear_hydrated_ranges` | Clears the ranges. | The item is dirty. |
| `set_hydration` | `hydration`. | Lowering a dirty item to `placeholder` or `partial`. |
| `set_sync` | `sync`, as a compare-and-set against the value the caller expects. | The stored value differs; setting a dirty item `in-sync` or `pending-download`; setting an item with `remote_changed` `in-sync` (only applying fresh metadata clears the flag). |
| `set_pin`, `set_last_error` | That one column. | Unknown item. |
| `move_item_local` | `parent_item_id` and `name` for a local rename or move; works on provisional items. See the rename gap under "Not in version 1". | Invalid name. |
| `rekey_item` | A provisional item's `item_id`, its children's `parent_item_id` and its ranges' `item_id` (by `ON UPDATE CASCADE`), together, and clears `is_provisional`. | Not provisional; the new key exists. |
| `delete_item(key, mode, pending)` | Removes the item (or subtree) and its ranges. | Children without `Recursive`; any mount root in the delete, always; any dirty item in the delete, except the named item under an explicit mode below. |

`delete_item`'s `pending` argument:

| Mode | Exempts | Further refusals |
|---|---|---|
| `Refuse` | nothing | |
| `ServerConfirmedLocalDelete { expected_generation }` | the named item only | The named item is provisional; its generation moved on. Any other dirty item in the subtree is still refused and named. |
| `DiscardNeverUploaded { expected_generation }` | the named item only | The named item is not provisional; its generation moved on. Same for other dirty items. |

### Remote metadata

`apply_remote_metadata` decides whether the content identity changed by comparing the
report with the version the store last knew: the server columns of a clean item, or the
recorded remote identity of a dirty one. Two identities name the same content when their
sizes are equal and: the hashes are equal if either side has a hash; else the cTags are
equal if both have one (a rename changes the eTag but not the cTag); else the eTags are
equal. A change of kind always counts.

| Item | Content | Effect | Result |
|---|---|---|---|
| clean | unchanged | All server-owned columns written; `remote_changed` cleared. | `ContentUnchanged` |
| clean | changed | As above, and ranges cleared and hydration set to `placeholder` (`full` for a folder), in one savepoint. | `ContentReplaced` |
| dirty | unchanged | Parent and name written; the report recorded as the last remote identity. | `ContentUnchanged` |
| dirty | changed | Parent and name written; the report recorded; `remote_changed` set. | `ContentChangedUnderLocalState` |

On a dirty item nothing describing content is written: not size, timestamps, eTag, cTag or
server hash. Size and ranges keep describing the local bytes, and the stored eTag stays the
one the local change was based on, so the next upload fails with 412 (ADR-0012 R6, R7) if
the server content moved on, even if the engine crashes before it handles the conflict.
A remote rename of a dirty item also changes its eTag on the server, so that upload can
meet a 412 and produce a conflict copy that was not strictly needed. That is safe, not
lossy.

### Uploads

`complete_upload(key, expected_generation, uploaded_local_hash, response, new_item_id)`
runs in one savepoint:

1. The item must be dirty; a replayed completion on a clean item is refused, so it cannot
   overwrite newer server identity.
2. A provisional item takes `new_item_id` (as `rekey_item`). A provisional item without
   one, a non-provisional item with a different one, or a `response` for another key is
   refused.
3. `local_hash` becomes the uploaded hash; eTag, cTag, server hash, parent and name come
   from `response`. The next upload's `If-Match` is therefore the uploaded version's eTag.
4. The server has moved on if `remote_changed` is set and the recorded remote identity
   names different content from `response`. If it names the same content, the delta
   was our own upload echoing back, possibly with newer metadata (renamed, a later eTag):
   then the row keeps its current parent and name and takes eTag and cTag from the
   recorded identity, instead of the older response.
5. If `local_generation == expected_generation`, our current bytes are on the server, so
   the item becomes clean: size and timestamps come from `response`, ranges are trimmed to
   the server size, the recorded identity is cleared. If the server has not moved on, sync
   is set `in-sync` and `remote_changed` cleared (`Synced`). If it has, sync is set
   `pending-download` and `remote_changed` kept (`UploadedButRemoteMovedOn`): the engine
   fetches the current metadata and applies it, which replaces the content as for any
   clean item. Dehydration stays refused meanwhile, because the item is not `in-sync`.
6. Otherwise the user changed the file while it was uploading. Size, timestamps and ranges
   keep describing the local content, the item stays dirty and `pending-upload`
   (`LocalChangedSince`), and the uploaded version becomes the recorded identity unless
   the server moved on, in which case the newer report and `remote_changed` are kept.
   Upload 1 completing can never mark version 2 as synced.

### Restart recovery

`dirty_items()` is the restart-recovery listing: every item with `local_dirty`, in every
drive, whatever its sync state. It does not depend on `sync`, so a mount that is blocked or
in error cannot hide a dirty item, and an item marked dirty just before a crash is listed
even if nothing else was written. Because the mark also moves the item to `pending-upload`,
`items_with_sync(PendingUpload)` finds such items as well, but recovery uses
`dirty_items()`.

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

## What the engine must do

The store enforces everything it can see. It never sees the bytes in a backing file, the
network, or the order in which the engine fetched things, so these rules are the engine's.

- **Mark before every write.** Commit `mark_local_change` (or `set_local_size` /
  `truncate_local`, which mark too) before each write, truncate or other modification
  reaches the backing file (ADR-0012 R3). A write landing while an upload may be reading
  the file must bump the generation again, with its own committed mark, so that upload's
  completion is `LocalChangedSince` rather than `Synced`.
- **Hydrate before a local change.** The store refuses to mark a file that is not fully
  local, so before a partial write (or any write) to a `placeholder` or `partial` file the
  engine fetches the missing ranges and records them (ADR-0012 R1). Only `O_TRUNC` needs no
  download: use `truncate_local(key, 0)`.
- **Read the generation before the bytes.** Before reading the file to upload it, read
  `local_generation`, then read the bytes, then pass that generation to
  `complete_upload`. Reading it afterwards could attribute newer bytes to the upload.
- **Send `If-Match` with the stored eTag** on every content upload of an existing item
  (ADR-0012 R6). On a dirty item the stored eTag is the base version's, which is what
  makes a remote change surface as 412.
- **Use fresh server metadata.** Pass freshly fetched metadata to `discard_local_changes`
  (stale metadata is refused when the store can tell, but not always), and refetch the
  item's metadata after `UploadedButRemoteMovedOn` and apply it. Never apply a delta page
  fetched before a `complete_upload` after that `complete_upload` has committed: it can
  describe an older version as current.
- **On restart, process both `dirty_items()`** (resume or retry every upload, ADR-0012 R11)
  **and the items in `pending-download`** (including those left by
  `UploadedButRemoteMovedOn`, whose current metadata must be refetched).
- **Conflict copies in this order:** make the copy's bytes durable in the backing
  directory, insert the copy as a provisional item (dirty, so it is uploaded), and only
  then `discard_local_changes` on the original with the server's current metadata. A crash
  between any two steps leaves the user's bytes in at least one dirty item.
- **Effective pin.** `dehydrate` checks only that the item is clean and `in-sync`; the
  engine refuses it when the effective pin is `Pinned` (ADR-0008 R5), and re-hydrates a
  pinned item whose content was replaced (ADR-0008 R6).
- **Respect the gaps** listed under "Not in version 1". In particular, do not use
  `ServerConfirmedLocalDelete` or `discard_local_changes` in place of the missing
  "remote deleted while locally modified" and "adopt server item" operations: a confirmed
  delete is only for a delete the user made locally and the server confirmed, and
  discarding throws away the user's bytes.

### Known costs

Accepted for version 1; neither loses data.

- **A response without a hash after an echo with one.** If the delta echo of our upload
  carried a hash and the upload response does not, the two identities do not match, so the
  completion reports `UploadedButRemoteMovedOn` and the engine refetches and re-downloads
  content it already has.
- **`DiscardNeverUploaded` on a provisional folder** with `Recursive` also removes the rows
  of clean server items the user moved into it locally, because local moves are not
  recorded until M1's pending-operations records. Their content is clean (on the server),
  so nothing unsynced is lost, but the items disappear locally until the next full
  enumeration.

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
   the ranges are bytes of the version the server columns identify; when the server
   reports a different version, the ranges are cleared in the same operation that records
   it. For a dirty item, the ranges and `size` describe the local content, the server
   columns describe the version the local change was based on (or the last version
   uploaded), and the recorded remote identity is the last version the server reported.
5. **Unsynced local content is never released, forgotten or reported in sync.**
   `local_dirty` is set, and committed, before a local change touches the backing file,
   and is cleared only by a `complete_upload` of the current generation or by
   `discard_local_changes`. While it is set: the item is never `in-sync` or
   `pending-download` (a table CHECK); ranges cannot be cleared, hydration cannot be
   lowered, the item cannot be dehydrated; remote metadata cannot change its size,
   timestamps or content identity; and it cannot be deleted except as the named item of an
   explicit mode. A provisional item is always dirty (a table CHECK). Every guard keys on
   `local_dirty`, never on `sync`.
   **A dirty file is always fully local:** its hydration is `full` (a table CHECK) and its
   ranges are exactly `[0, size)`. A file becomes dirty only when it is already fully local
   (or by truncation to 0), every local-write operation keeps the ranges `[0, size)`, and
   no separate range can be recorded on it. So no "hydrate the missing range" can ever
   download over the user's bytes.
6. **A remote change seen while dirty is never forgotten.** It sets `remote_changed`
   durably; `complete_upload` clears it only when the change was our own upload echoing
   back, and otherwise leaves the item `pending-download` (or still dirty) with the flag
   set. An item with `remote_changed` is never `in-sync` (a table CHECK); only applying
   fresh metadata clears the flag.
7. **Ranges are stored merged.** For one item no two ranges overlap or touch. Recording a
   range replaces every stored range it overlaps or touches with their union, in one
   savepoint. A zero-length range records nothing. "Is this range present" is true only if
   one stored range covers every byte of it; a zero-length query is trivially present.
8. **Ranges fit the item.** Every range ends at or before the item's `size`; a range past
   the end is refused, and so is a query past the end. Folders have no ranges.
   `set_local_size` and `truncate_local` set them to exactly `[0, size)`; a remote size
   change on a clean item clears them (invariant 4) and is not applied to a dirty one.
9. **Deletes.** Deleting an item deletes its ranges. Deleting an item with children is
   refused unless the caller asks for a recursive delete, which removes the whole subtree
   and its ranges and terminates on a parent cycle. A delete that would remove a dirty
   item, whatever its sync state, is refused with an error naming every such item, except
   the named item under `ServerConfirmedLocalDelete` or `DiscardNeverUploaded` with a
   matching generation. A delete that would remove a mount's root item is always refused.
   Deleting rows never touches a backing directory.
10. **Every operation is atomic.** An operation that runs several statements runs them in
    its own savepoint, so it applies fully or not at all, whether called on its own or
    inside a caller's transaction. A refused operation leaves the caller's transaction
    usable. If rolling back to the savepoint itself fails, the whole transaction is rolled
    back, so the caller's `Transaction` reports `TransactionAborted` from then on.
11. **Transactions.** A caller's batch (for example a delta page plus its next token) runs
    in one `BEGIN IMMEDIATE` transaction. It is visible only after commit, and absent if
    the transaction is dropped or the process dies first. If SQLite rolls the transaction
    back by itself (`SQLITE_FULL`, an I/O error, out of memory), every later call on it,
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
- **Table rebuilds are allowed.** SQLite cannot alter most constraints in place, so a
  migration may rebuild a table the documented way: create the new table, copy the rows,
  drop the old one, rename the new one, recreate its indexes. Foreign keys are turned off
  around the migration transaction (they cannot change inside one), because dropping
  `items` with them on fails. After each migration, `PRAGMA foreign_key_check` must report
  nothing, or the whole migration is rolled back (`MigrationForeignKeys`). Foreign keys are
  turned back on before the store is used. A unit test runs such a rebuild of `items`.
- Before anything is written, the file is checked over a read-only connection, which never
  checkpoints a WAL into the main file. It is refused, byte-identical, when:
  - it is not SQLite, its `application_id` is not ours, or it is at version 0 with tables
    in it (`NotAStateStore`);
  - its `user_version` is greater than the build's `SCHEMA_VERSION` (`SchemaTooNew`).
- WAL mode is set only after those checks.

| Version | Change |
|---|---|
| 1 | `application_id`; `accounts`, `mounts`, `items`, `hydrated_ranges`; indexes `items_by_parent`, `items_by_sync`, `items_by_dirty`. |

## Not in version 1

To be added by a version 2 migration in the M1 engine work, not by editing version 1:

- Mount state (`active`, `paused`, `blocked`, `resyncing`) and account state
  (`signed-in`, `needs-sign-in`). Version 1 stores only a mount's blocked reason.
- F02's saved `nextLink` and the pending-versus-live delta token. Version 1 has one
  `delta_token` per mount.
- F02's per-item "materialised" flag.
- F13's pending-operations records (create, update content, move, delete), with base
  eTags. **The rename/move gap until then:** a local rename or move
  (`move_item_local`) is not recorded as pending, and three operations write parent and
  name from the server: `apply_remote_metadata` (on clean and dirty items alike),
  `complete_upload` (from the upload response) and `discard_local_changes`. So a local
  rename made while an upload is in flight, or before the next delta, can be reverted to
  the server's name and parent. No content is lost, but the user's rename is.
- An error retry count.
- A read-only reader connection for the VFS.
- An "adopt server item" operation that merges a server row into a provisional one (see
  "Recovery" above).
- "Remote deleted while locally modified": the server deleted an item that is dirty here.
  The intended operation re-creates it as a new provisional item, keeping the local bytes,
  and uploads it as a new item. Version 1 has no operation for it; the item stays dirty
  and its delete is refused. Do not use `ServerConfirmedLocalDelete` (the user did not
  delete it) or `discard_local_changes` (it throws the bytes away) instead.
- Restoring an item after a refused or reverted delete.
- Mount removal (deleting a mount row and its items without touching remote content,
  ADR-0015).

Also not in version 1: per-folder counters for aggregate folder status (F13 open
question 1).
