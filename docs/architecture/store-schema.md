# State store schema

The state store is the SQLite database at `$XDG_STATE_HOME/mydrive/state.db`. It is the
source of truth for accounts, mounts, items, their state and which bytes of each file are
local (ADR-0006). This file describes schema version 2: version 1 was built by WP-006 in
`crates/mydrive-store`, and version 2 (WP-017) adds the local ID (ADR-0021), the facts the
engine needs about accounts and mounts, durable delta staging (ADR-0022) and listing by
name. The migration SQL in `crates/mydrive-store/src/schema.rs` is authoritative; this
document must change in the same pull request as any migration.

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
| `kind` | TEXT | no | `personal` or `work` (a CHECK list; `mydrive-core`'s `AccountKind`). |
| `username` | TEXT | yes | The sign-in name, if known. Not empty. |
| `display_name` | TEXT | yes | The name to show, if known. Not empty. |
| `org_name` | TEXT | yes | The organisation's name, for a work account. Not empty. |
| `needs_sign_in` | INTEGER | no | 1 when the refresh token was rejected. The account's state (`signed-in`, `needs-sign-in`) is derived from it, not stored. Default 0. |
| `hash_percent_refused` | INTEGER | no | 1 when the tenant refuses `#` and `%` in names (`NameContext::hash_and_percent_allowed`). Default 0. |

`accounts()` lists them in byte order of `account_id`. `upsert_account` writes every column;
`set_account_needs_sign_in` and `set_account_refuses_hash_and_percent` write one flag each.

### `mounts`

| Column | Type | Null | Notes |
|---|---|---|---|
| `mount_id` | TEXT | no | Primary key. Names the backing directory (ADR-0007 R5). |
| `account_id` | TEXT | no | References `accounts`. |
| `drive_id` | TEXT | no | Lowercase. |
| `root_item_id` | TEXT | no | The drive root, or a folder within the drive. |
| `mount_point` | TEXT | no | UTF-8 path the user sees. Not a key. |
| `drive_type` | TEXT | yes | `personal`, `business` or `document-library` (Graph's `driveType`). Required for a new mount; `NULL` only for a work account's mount added by version 1, which never recorded it (`set_mount_drive_info` fills it). |
| `display_name` | TEXT | yes | The drive's or library's name. Not empty. |
| `read_only` | INTEGER | no | 0 or 1. |
| `read_only_reason` | TEXT | yes | `library-read-only` or `no-write-permission`; only with `read_only = 1`, which may also have no recorded reason. |
| `paused` | INTEGER | no | 1 while the user has paused the mount. |
| `paused_until` | INTEGER | yes | Unix seconds at which the engine resumes it; only with `paused = 1`. |
| `blocked_reason` | TEXT | yes | `organisation-policy` or `tenant-refused` (the protocol's open set `mount.blocked_reason`); `NULL` when not blocked. |
| `blocked_message` | TEXT | yes | Text for people; only with a `blocked_reason`. |
| `delta_token` | TEXT | yes | The live token (ADR-0007 R2): the only one a new run starts from. `NULL` until the first run is finished. Changed only by `finish_delta_run` (ADR-0022 R4). |
| `delta_run` | TEXT | yes | The run in progress: `initial`, `incremental`, `resync-apply` or `resync-upload`; `NULL` when none. |
| `delta_next_link` | TEXT | yes | Where the run in progress continues (the last staged page's next link). |
| `delta_staged_link` | TEXT | yes | The delta link of a run whose staged set is complete and awaits apply. |

The delta token and both links are never logged; `Mount`'s `Debug` redacts all three.

`UNIQUE (drive_id, root_item_id)`: the same remote folder cannot be mounted twice
(ADR-0007 R4). The store refuses it with an error naming the existing mount.

Table CHECKs (WP-017 AC7):

- `delta_run IS NOT NULL OR (delta_next_link IS NULL AND delta_staged_link IS NULL)`: no
  link without a run.
- `delta_next_link IS NULL OR delta_staged_link IS NULL`: never both links.
- `blocked_message IS NULL OR blocked_reason IS NOT NULL`.
- `paused = 1 OR paused_until IS NULL`.
- `read_only = 1 OR read_only_reason IS NULL`.
- An `incremental` run has a live token; an `initial` run has none.
- Every code column holds one of its listed values; a unit test keeps each list equal to
  the Rust type's `WIRE_FORMS`. A new code is a new migration; `mounts` is small and has no
  foreign key pointing at it except `delta_staged`, so a rebuild is cheap.

Every operation checks these first and refuses a violation with
`StoreError::InvalidMount { rule: MountRule::… }`, so the caller gets a typed error, never a
SQL constraint failure. `insert_mount` takes the whole row and refuses, besides the rules
above, a mount without a drive type and a new mount with a live token or a run link (the
token is written only by finishing a run, the links only by staging), or with a run other
than `initial`. The setters take typed arguments (`MountPause`, `MountBlock`, `MountAccess`)
that cannot express a violation, except an empty text.

There is no stored mount state. `derive_mount_state` (and `Mount::state`) derives it from the
facts, in this precedence:

| State | When |
|---|---|
| `blocked` | `blocked_reason` is set |
| `paused` | else `paused = 1` (`paused_until` does not enter: the mount is paused until the engine clears it) |
| `resyncing` | else `delta_token` is `NULL` (the first run has not been finished) or `delta_run` is a resync |
| `active` | otherwise |

The mount point has no unique constraint: paths are never keys (ADR-0006 R4, ADR-0007 R6).
Refusing a mount point that is in use, nested or overlapping is the engine's check.

### `items`

Primary key (`drive_id`, `item_id`) (ADR-0006 R4). `WITHOUT ROWID`.

| Column | Type | Null | Owner | Notes |
|---|---|---|---|---|
| `drive_id` | TEXT | no | key | Lowercase. |
| `item_id` | TEXT | no | key | Opaque, stored exactly as Graph gives it, or a provisional value the engine generated (see `is_provisional`). |
| `parent_item_id` | TEXT | yes | server / local | Parent in the same drive. `NULL` for the drive root. No foreign key: see invariants. |
| `name` | TEXT | no | server / local | Not empty, not `.` or `..`, no `/` or NUL. May exceed 255 bytes (F02 stores such items with sync `error`). `COLLATE BINARY`: compared and ordered as the bytes of its UTF-8. |
| `kind` | TEXT | no | server | `file`, `folder` or `package` (Graph's `package` facet, for example a OneNote notebook). Validated by the Rust type, not by a SQL `CHECK`, so a new kind needs no table rebuild. Only a file has content: a package, like a folder, never has ranges and cannot be dehydrated or resized. |
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
| `local_id` | INTEGER | no | store | The row's local ID (ADR-0021), `UNIQUE`, `>= 1`. Assigned by `insert_item` from `local_id_allocator`; no operation writes it afterwards. Names the content file `mounts/<mount_id>/content/<local_id>`. See "Local IDs". |

Table CHECKs, so these hold whatever writes the row:

- `is_provisional = 0 OR local_dirty = 1`: a provisional item is always dirty.
- `local_dirty = 0 OR sync NOT IN ('in-sync', 'pending-download')`: a dirty item is never
  reported in sync, and no download is scheduled over it.
- `local_dirty = 1 OR remote_size IS NULL`: a clean item has no recorded remote identity.
- `local_dirty = 0 OR kind <> 'file' OR hydration = 'full'`: a dirty file is fully local.
  The store also keeps its ranges exactly `[0, size)` (invariant 5). (Version 1 wrote this
  as `kind = 'folder'`; with only `file` and `folder` the two are the same.)
- `remote_changed = 0 OR sync <> 'in-sync'`: an item the server has moved past is never
  reported in sync until fresh metadata is applied.

The three state columns hold the wire forms of `mydrive-core`'s `Hydration`, `Pin` and
`Sync`, each in its own column (ADR-0006 R5, ADR-0008 R1). A `CHECK` lists the allowed
values; a unit test keeps those lists equal to `mydrive-core`'s `WIRE_FORMS`. No column holds
a display status.

Indexes:

- `items_by_parent_name (drive_id, parent_item_id, name)` for children, deletes, path
  resolution and listing by name. Version 2 replaced version 1's
  `items_by_parent (drive_id, parent_item_id)`, whose lookups it serves as a prefix. Because
  `items` is `WITHOUT ROWID`, each index entry also carries `item_id`, so children come in
  (`name`, `item_id`) order straight from the index.
- `items_by_sync (sync)` for listing items by sync state.
- `items_by_dirty (local_dirty) WHERE local_dirty = 1`, a partial index, for `dirty_items`.
- The `UNIQUE` constraint on `local_id`.

### `local_id_allocator`

One row (`singleton = 1`, a CHECK) holding `next_local_id`, the next local ID to hand out
(`>= 1`). See "Local IDs".

### `delta_staged`

The staged set of the mount's delta run in progress (ADR-0022), one row per item: the last
entry staged for it in the run. Primary key (`mount_id`, `item_id`). `WITHOUT ROWID`.
`mount_id` references `mounts` `ON DELETE CASCADE`.

| Column | Type | Null | Notes |
|---|---|---|---|
| `mount_id` | TEXT | no | The mount. |
| `item_id` | TEXT | no | The item, in the mount's drive. |
| `seq` | INTEGER | no | Order of staging in the run: re-staging an item gives it a larger value. `UNIQUE (mount_id, seq)`. |
| `deleted` | INTEGER | no | 1 for an entry with the `deleted` facet. |
| `problem` | TEXT | yes | `NULL` for a usable entry; else `reported` (the Graph client could not use it), `parent-cycle`, `missing-parent` or `parent-deleted` (found by the store when the run is complete). |
| `problem_detail` | TEXT | yes | Why, exactly when `problem = 'reported'`. |
| `is_root` | INTEGER | no | 1 for the drive root (Graph's `root` facet). |
| `facet` | TEXT | yes | `file`, `folder`, `package` or `remote-item`; validated in Rust. |
| `parent_item_id`, `name`, `size`, `created_at`, `modified_at`, `etag`, `ctag`, `server_hash` | as in `items` | yes | The server's description. `name` is `COLLATE BINARY`. |
| `apply_order` | INTEGER | yes | Set when the run's delta link is staged; see "Delta runs". |

Table CHECKs: a delete carries nothing but its key; a usable entry (not deleted, no problem)
has a facet, name, size and both times, and a parent unless it is the drive root; the drive
root has no parent and is never a delete; `problem_detail` is present exactly for a
`reported` problem; name and facet are present together.

Rows exist only while the mount has a run (`delta_run` set): `begin_delta_run` and
`finish_delta_run` remove them, `stage_delta_page` refuses without a run, and removing the
mount removes them. SQLite cannot express this across two tables without a trigger, so the
operations enforce it and the model test checks it after every step.

Indexes: `delta_staged_by_parent (mount_id, parent_item_id)` for ordering the set, and
`delta_staged_by_order (mount_id, apply_order, item_id)` for walking it.

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
| `insert_item` | Every column of a new item, and a new `local_id`, which it returns. Either a clean server item, or a provisional one, which must be dirty, `pending-upload` and (for a file) `full`; its range `[0, size)` is recorded. | The key exists; a provisional item not dirty and `pending-upload`; a dirty item that is not provisional; a dirty file that is not `full`; any `remote_changed` or recorded remote identity. |
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
| `move_item_local` | `parent_item_id` and `name` for a local rename or move; works on provisional items. See the rename gap under "Not in version 2". | Invalid name. |
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

A folder (or package) that has children is never turned into a file.

## Local IDs

ADR-0021 R1: a `local_id` is assigned exactly once per row, is unique, and is never reused
after its row is removed, across restarts.

**Mechanism.** `local_id_allocator.next_local_id` is a counter that only counts up.
`insert_item` takes the next value (`UPDATE … SET next_local_id = next_local_id + 1
RETURNING next_local_id - 1`) and writes the row with it, in one savepoint. No operation
writes `local_id` afterwards, and nothing lowers the counter. `items.local_id` is
`NOT NULL UNIQUE CHECK (local_id >= 1)`.

Why no event can hand out an ID that a row has held:

| Event | Why the ID is not reused |
|---|---|
| The row with the largest ID is removed, or every row is | The counter is its own row, not `MAX(local_id) + 1`; deleting items never changes it. |
| Restart | The counter is durable, written in the same transaction as the row. |
| Crash between allocation and insert | They are one savepoint inside the caller's transaction: both commit or neither does. A rolled-back allocation was never held by any row, so handing that number out again reuses nothing. (Tested by a kill test.) |
| `VACUUM` | It rewrites pages, not values: `local_id` is an ordinary column of a `WITHOUT ROWID` table and the counter an ordinary column, so both survive. (Rowids of rowid tables without an `INTEGER PRIMARY KEY` could change; neither table relies on one.) |
| The version 2 migration | No version 1 row had a local ID. The migration numbers the existing rows 1, 2, … in key order and sets the counter one past the largest. |
| A later migration that rebuilds `items` | It must copy `local_id` unchanged and leave `local_id_allocator` alone. The unit test of a table rebuild checks the copy. |
| Mount removal, then adding the drive again | Removal deletes rows, never the counter; the new rows get new IDs. |

`rekey_item`, `complete_upload` with a new server ID and `move_item_local` change the key,
parent or name, never `local_id` (ADR-0021 R2). `local_id(key)` reads it.

The engine must **commit the row before it creates the content file** for it. An ID whose
insert was rolled back is free again; a content file created for it before the commit would
become the content of whichever row takes the number next.

## Listing by name

- `children_after(parent, after_name, limit)`: the children whose name is strictly after
  `after_name` (or all, from the first), ordered by name then item ID, at most `limit`
  of them, and whether more follow. Byte order means `BINARY` collation on the stored
  UTF-8: no locale, no case folding, no Unicode normalisation; `ab` comes after `a`, and
  every non-ASCII name after every ASCII one. The statement is a range scan of
  `items_by_parent_name` with no sort (a unit test reads the query plan), so each page
  costs its own rows. Two children can hold the same name only transiently (the server
  refuses it); so that the next page, which starts strictly after the last name, cannot
  skip one, a page that would end inside a run of equal names is extended to the end of
  the run, and may then exceed `limit`. Paging this way delivers every child that exists
  unchanged throughout exactly once (ADR-0020 R1, R2).
- `child_by_name(parent, name)`: the child whose name is exactly `name`, byte for byte;
  `DuplicateName` if more than one child has it.
- `item_count(drive)`: how many items the drive has.

## Delta runs

ADR-0022. A run has a kind (`delta_run`), stages pages, and is finished.

| Operation | Does | Refused when |
|---|---|---|
| `begin_delta_run(mount, kind)` | Removes any staged rows (a restart discards its staged set first), sets `delta_run`, clears both links. Returns how many rows it discarded. The live token is untouched. | `incremental` without a live token; `initial` with one. |
| `stage_delta_page(mount, entries, link)` | Upserts each entry (a later entry for an item, in the same page or a later one, replaces it and takes a new `seq`: ADR-0022 R2) and records the link, in one savepoint (R1). `PageLink::Next` sets `delta_next_link`; `PageLink::Delta` sets `delta_staged_link`, clears the next link and orders the complete set. Writes no item row (R3). | No run; the delta link already staged (`DeltaRunComplete`); an entry the table cannot hold (an invalid name, a root with a parent, a non-root without one, a deleted root, a store-only problem kind): the whole page is refused. |
| `staged_count(mount)` | How many entries are staged. | |
| `staged_entries(mount, after, limit)` | One page of the complete set in apply order, with a cursor for the next. | No run; not complete. |
| `unstaged_items(mount, after, limit)` | One page, by item ID, of the drive's items the complete set does not contain: F05's resync anti-join. An item staged as deleted counts as contained. Dirty and provisional items are listed too; `delete_item` refuses to remove them. | No run; not complete, because an item missing from a partial set is not missing from the server. |
| `finish_delta_run(mount)` | Removes the staged rows, sets `delta_token` to the staged delta link and clears the run state, in one savepoint. The only operation that writes `delta_token` (R4). | No run; not complete. |

A run interrupted while staging resumes from `delta_next_link`. A run whose first request is
not the live token or the drive root (a resync's `Location`) stages an empty page with that
link as its next link.

**Apply order.** When the delta link is staged, every entry gets an `apply_order`, stored, so
the walk needs one page in memory however large the set:

1. Each entry that is not a delete gets its depth in the staged set: 0 if its parent is not a
   staged entry (the drive root, a parent already in `items`, or no known parent), else one
   more than its parent's. One statement per depth level, each reaching only that level's
   children through `delta_staged_by_parent`.
2. An entry never reached that way has a parent chain that loops within the staged set (or
   lies below such a loop). It is marked `parent-cycle` and ordered after every other entry
   that is not a delete. The walk therefore always ends and drops nothing.
3. A depth-0 entry whose parent is staged as deleted is marked `parent-deleted`; one whose
   parent is neither staged nor in `items` is marked `missing-parent`, except the mount's
   root item, whose parent may lie outside the mount.
4. Deletes come last.

The walk pages by (`apply_order`, `item_id`) through `delta_staged_by_order`. Marked entries
keep their description, so the engine can fetch the item and resolve them (ADR-0022 R6);
they are returned like any other entry, never left out.

## Removing a mount

`remove_mount(mount, local_only)` removes, in one savepoint, the mount's row, its staged
entries and **every item of its drive** with their ranges: a mount owns its drive's rows. It
sends nothing to the server and touches no file (ADR-0015 R8). Rows of any other drive are
never changed.

| `local_only` | Refused when |
|---|---|
| `LocalOnlyContent::Refuse` | Any item of the drive is dirty: `MountHasDirtyItems` with the full count and at most `DIRTY_SAMPLE_LIMIT` (20) of their keys, in key order. |
| `LocalOnlyContent::KeptElsewhere { dirty_items }` | The number of dirty items is not `dirty_items`, the count the user confirmed (`DirtyCountChanged`): something changed since. |

It is also refused (`DriveSharedByMounts`) while another mount shows the same drive, whose
rows it would remove. There is no default: the caller must name one of the two modes.

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
- **Content files after the row.** Commit `insert_item` before creating the item's content
  file at `mounts/<mount_id>/content/<local_id>` (see "Local IDs"). Build the path only
  from `mount_id` and `local_id` (ADR-0021 R3).
- **Delta runs.** Stage each page with its link in one call (ADR-0022 R1); apply only after
  the delta link is staged, in `staged_entries` order; resolve each problem entry by
  fetching the item, never by guessing (R6); make applying an entry idempotent (R5), since a
  crashed apply is applied again; and call `finish_delta_run` only after every entry, and
  for a resync the anti-join, has been applied. The store cannot see whether the apply is
  complete. Convert an entry the store would refuse (an invalid name) into a `Reported`
  problem rather than letting it refuse the page.
- **Removing a mount.** Write every fully local file into the mount-point directory and get
  the user's confirmation for dirty items before `remove_mount`; remove the backing
  directory after it commits.
- **Respect the gaps** listed under "Not in version 2". In particular, do not use
  `ServerConfirmedLocalDelete` or `discard_local_changes` in place of the missing
  "remote deleted while locally modified" and "adopt server item" operations: a confirmed
  delete is only for a delete the user made locally and the server confirmed, and
  discarding throws away the user's bytes.

### Known costs

Accepted since version 1; neither loses data.

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
12. **Local IDs are never reused.** Every row has a `local_id` from the allocator, which
    only counts up; nothing changes a row's `local_id` (see "Local IDs").
13. **Staging writes no item row.** `begin_delta_run`, `stage_delta_page` and the walk write
    only `delta_staged` and the mount's run columns (ADR-0022 R3).
14. **The live token moves only at the end of a run.** `delta_token` is written only by
    `finish_delta_run`, in the savepoint that removes the staged rows (ADR-0022 R4). A run
    killed at any point is either still in progress with its staged set intact, or
    finished.
15. **Mount removal never removes unconfirmed local changes** and never changes another
    drive's rows.

## Errors

The error kinds are part of the API. Version 2 adds:

| Error | When |
|---|---|
| `InvalidMount { mount_id, rule: MountRule }` | A mount, or a change to one, would break a `mounts` constraint. `MountRule`: `LinkWithoutRun`, `BothLinks`, `BlockedMessageWithoutReason`, `PausedUntilWithoutPause`, `ReadOnlyReasonWithoutReadOnly`, `DriveTypeRequired`, `NewMountHasDeltaState`, `NewMountRunNotInitial`, `IncrementalWithoutToken`, `InitialWithToken`, `EmptyText`. |
| `InvalidAccount { account_id, reason }` | An empty user, display or organisation name. |
| `NoDeltaRun { action, mount_id }` | Staging, walking, listing or finishing without a run. |
| `DeltaRunComplete { mount_id }` | Staging after the run's delta link is staged. |
| `DeltaRunNotComplete { action, mount_id }` | Walking, the anti-join or finishing before the delta link is staged. |
| `InvalidStagedEntry { mount_id, item_id, reason }` | An entry the staging table cannot hold; the whole page is refused. |
| `MountHasDirtyItems { mount_id, count, sample }` | `remove_mount` with `Refuse` while items are dirty; `sample` holds at most `DIRTY_SAMPLE_LIMIT` keys. |
| `DirtyCountChanged { mount_id, confirmed, actual }` | `remove_mount` with `KeptElsewhere` and a count that is no longer right. |
| `DriveSharedByMounts { mount_id, drive_id, other, count }` | `remove_mount` while another mount shows the drive. |
| `DuplicateName { parent, name, count }` | `child_by_name` when several children have the name. |

`NotAFile` now also covers a package, and `FolderHasChildren` a package with children.

## Migrations

- The schema version is SQLite's `user_version`. Version 0 is an empty database.
- Migrations are numbered from 1; each number is greater than every earlier one
  (ADR-0006 R3). There are no down-migrations.
- A migration's SQL is frozen once released. A schema change, including a new state value
  in a `CHECK` list, is a new migration with a test that runs it on a copy of the previous
  schema.
- On open, every migration newer than the database runs in one transaction together with
  the `user_version` update, so a crash leaves the old version untouched or the new version
  complete. A kill test stops the process after migration 2's statements and version bump,
  before the commit, and finds a valid version 1 store with every row unchanged.
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
| 2 | `accounts` gains `kind`, `username`, `display_name`, `org_name`, `needs_sign_in`, `hash_percent_refused` by `ADD COLUMN`. `mounts` is rebuilt with the new facts, run columns and CHECKs. `items` is rebuilt with `local_id`, `items_by_parent` is replaced by `items_by_parent_name`, and `name` is `COLLATE BINARY` (the default, now explicit). New: `local_id_allocator`, `delta_staged` and its two indexes. |

**Why version 2 rebuilds `items`.** `ALTER TABLE ADD COLUMN` cannot add a `NOT NULL` column
without a constant default, nor a `UNIQUE` column; and a constant default gives every
existing row the same value, which a unique index then refuses. A unit test
(`local_id_needs_a_rebuild_not_add_column`) shows all three. The rebuild copies every
version 1 column unchanged.

**What version 2 makes of version 1 rows.** Every version 1 column keeps its value, bit for
bit: in particular `local_dirty`, `local_generation`, `remote_changed`, the recorded remote
identity, every hydrated range and both delta tokens. The new facts are filled only where
version 1's meaning settles them:

- `accounts.kind`: `personal` where `tenant_id` is `NULL` (version 1's definition of a
  personal account), else `work`. The names are `NULL` and both flags 0.
- `mounts.drive_type`: `personal` for a personal account's mount; `NULL` for a work
  account's, where version 1 cannot tell a OneDrive for Business from a library.
- `mounts.blocked_reason`: version 1 held free text. A blocked mount becomes
  `organisation-policy` (version 1's only meaning: "why policy blocks the mount") with the
  old text as `blocked_message`.
- No run is in progress; a mount without a token is `resyncing` and starts an `initial`
  run.
- `items.local_id`: 1, 2, … in (`drive_id`, `item_id`) order, and the allocator one past the
  largest.

The migration test (`tests/migration.rs`) runs on `tests/fixtures/v1/state-v1.db`, written by
the version 1 code itself: `tests/fixtures/v1/generate.sh` checks out commit 4673830 (the
last with schema version 1) in a temporary worktree and runs
`generate_v1_fixture.rs` there against the version 1 API. Regenerate it only if the
fixture must change.

## Not in version 2

To be added by a later migration (WP-018, schema version 3, and later work), not by editing
versions 1 or 2. Q38 in `open-questions.md` lists the operations WP-018 designs.

- F13's pending-operations records (create, update content, move, delete), with base
  eTags, and upload session records. **The rename/move gap until then:** a local rename or
  move (`move_item_local`) is not recorded as pending, and three operations write parent
  and name from the server: `apply_remote_metadata` (on clean and dirty items alike),
  `complete_upload` (from the upload response) and `discard_local_changes`. So a local
  rename made while an upload is in flight, or before the next delta, can be reverted to
  the server's name and parent. No content is lost, but the user's rename is.
- An error retry count.
- A read-only reader connection for the VFS.
- An "adopt server item" operation that merges a server row into a provisional one (see
  "Recovery" above).
- "Remote deleted while locally modified": the server deleted an item that is dirty here.
  The intended operation re-creates it as a new provisional item, keeping the local bytes,
  and uploads it as a new item. Version 2 has no operation for it; the item stays dirty
  and its delete is refused. Do not use `ServerConfirmedLocalDelete` (the user did not
  delete it) or `discard_local_changes` (it throws the bytes away) instead.
- Returning content that turned out unchanged, or equal to the server's, to in-sync while
  keeping the bytes.
- The atomic conflict split (ADR-0021). With `local_id` the split needs no file rename: the
  row holding the bytes keeps its `local_id` and becomes the copy, and a new row (a new
  `local_id`, from `insert_item`) represents the server's version. Until it exists, the
  three-step order in "What the engine must do" stands.
- Restoring an item after a refused or reverted delete.
- Keeping a pinned file readable while its new version downloads (M2).

Version 2 adds nothing that WP-018 must undo: `local_id` is never rewritten, so a split
or an adopt can keep a row's handle; `items` was rebuilt here, so a version 3 rebuild (for
example to make "a pending operation implies not in-sync" a CHECK) copies `local_id` like
any other column and leaves `local_id_allocator` alone.

F02's per-item "materialised" flag is not needed: ADR-0021 has no materialisation phase.

Also not in version 2: per-folder counters for aggregate folder status (F13 open
question 1).
