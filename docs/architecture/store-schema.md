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
| `foreign_keys` | `ON` | Hydrated ranges cannot outlive their item. |
| `busy_timeout` | 5 s | Tolerates a brief lock from someone inspecting the file with `sqlite3`. |

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
| `delta_token` | TEXT | yes | The mount's one delta token (ADR-0007 R2). `NULL`: none saved yet. Never logged. |
| `read_only` | INTEGER | no | 0 or 1. |
| `blocked_reason` | TEXT | yes | Why policy blocks the mount; `NULL` when not blocked. |

`UNIQUE (drive_id, root_item_id)`: the same remote folder cannot be mounted twice
(ADR-0007 R4). The store refuses it with an error naming the existing mount.

The mount point has no unique constraint: paths are never keys (ADR-0006 R4, ADR-0007 R6).
Refusing a mount point that is in use, nested or overlapping is the engine's check.

### `items`

Primary key (`drive_id`, `item_id`) (ADR-0006 R4). `WITHOUT ROWID`.

| Column | Type | Null | Notes |
|---|---|---|---|
| `drive_id` | TEXT | no | Lowercase. |
| `item_id` | TEXT | no | Opaque, stored exactly as Graph gives it. |
| `parent_item_id` | TEXT | yes | Parent in the same drive. `NULL` for the drive root. No foreign key: see invariants. |
| `name` | TEXT | no | Not empty, not `.` or `..`, no `/` or NUL. May exceed 255 bytes (F02 stores such items with sync `error`). |
| `kind` | TEXT | no | `file` or `folder`. |
| `size` | INTEGER | no | Bytes, `>= 0`. |
| `created_at` | INTEGER | no | Unix seconds, UTC. Whole seconds (ADR-0011 R5). |
| `modified_at` | INTEGER | no | Unix seconds, UTC. |
| `etag` | TEXT | no | |
| `ctag` | TEXT | yes | Business delta omits it. |
| `server_hash` | TEXT | yes | quickXorHash the server reports, base64 as Graph sends it (ADR-0012). |
| `local_hash` | TEXT | yes | quickXorHash of the bytes we uploaded, base64 (ADR-0012). |
| `hydration` | TEXT | no | `placeholder`, `partial`, `full`. |
| `pin` | TEXT | no | The item's own pin: `inherit`, `pinned`, `unpinned`. Never the effective pin. |
| `sync` | TEXT | no | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked`. |
| `last_error` | TEXT | yes | Most recent error for the item. |

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
| `drive_id` | TEXT | no | With `item_id`, references `items`. |
| `item_id` | TEXT | no | |
| `range_start` | INTEGER | no | First byte, `>= 0`. |
| `range_end` | INTEGER | no | One past the last byte; `> range_start`. |

Ranges are half-open, `[range_start, range_end)`.

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
4. **Ranges are stored merged.** For one item no two ranges overlap or touch. Recording a
   range replaces every stored range it overlaps or touches with their union, in one
   savepoint. A zero-length range records nothing. "Is this range present" is true only if
   one stored range covers every byte of it; a zero-length query is trivially present.
5. **Ranges fit the item.** Every range ends at or before the item's `size`; a range past
   the end is refused, and so is a query past the end. Folders have no ranges. An upsert
   that would shrink an item below its ranges, or turn it into a folder, is refused: the
   caller clears the ranges first, in the same transaction.
6. **Deletes.** Deleting an item deletes its ranges. Deleting an item with children is
   refused unless the caller asks for a recursive delete, which removes the whole subtree
   and its ranges and terminates on a parent cycle. Deleting rows never touches a backing
   directory; whether an item with pending local changes may be deleted is the engine's
   decision.
7. **Every operation is atomic.** An operation that runs several statements runs them in its
   own savepoint, so it applies fully or not at all, whether called on its own or inside a
   caller's transaction.
8. **Transactions.** A caller's batch (for example a delta page plus its next token) runs in
   one `BEGIN IMMEDIATE` transaction. It is visible only after commit, and absent if the
   transaction is dropped or the process dies first.

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
- A database whose `user_version` is greater than the build's `SCHEMA_VERSION` is refused
  with `SchemaTooNew` and is not modified: the version is read over a read-only connection
  before anything else, and WAL mode is set only afterwards.
- A database at version 0 that already has tables is not a my-drive store and is refused
  the same way.

| Version | Change |
|---|---|
| 1 | `accounts`, `mounts`, `items`, `hydrated_ranges`; indexes `items_by_parent`, `items_by_sync`. |

## Not in version 1

Recorded so later work packages add them by migration rather than by editing version 1:

- Mount state (`active`, `paused`, `blocked`, `resyncing`) and account state
  (`signed-in`, `needs-sign-in`). Version 1 stores only a mount's blocked reason.
- F02's saved `nextLink` and the pending-versus-live delta token. Version 1 has one
  `delta_token` per mount.
- F02's per-item "materialised" mark.
- F13's pending operation records with base eTags, and an error retry count.
- A provisional local key for items created locally before the server assigns an
  `item_id` (F13 open question 4). Version 1 requires a server `item_id` and `etag`.
- Per-folder counters for aggregate folder status (F13 open question 1).
