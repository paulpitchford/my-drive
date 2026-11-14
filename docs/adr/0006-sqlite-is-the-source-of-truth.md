# ADR-0006: SQLite is the source of truth; xattrs are a public mirror

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

- Graph delta omits paths, and a folder rename does not return its descendants, so items must
  be tracked by ID, never by path
  ([delta docs](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)).
  Business delta omits `cTag`; `quickXorHash` is the only hash on both account types
  ([hashes](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0)).
- SharePoint rewrites uploaded Office files, so each item needs two hashes: the local hash at
  upload and the server's afterwards ([rclone OneDrive docs](https://rclone.org/onedrive/);
  README, SharePoint section).
- A library is its own drive, so the store must be keyed by (drive ID, item ID) from the first
  commit ([report, SharePoint section](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#sharepoint-libraries-make-every-drive-a-separate-sync-root)).
- xattrs alone are too fragile as state: "many tools and programs seem to strip them on save"
  ([nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)). openvfs keeps
  pin state in xattrs ([openvfs](https://github.com/opencloud-eu/openvfs)).
- Sparse-file holes cannot be distinguished from regions that are legitimately zero, and
  filesystems may allocate or merge extents, so `SEEK_HOLE`/`SEEK_DATA` cannot be trusted as
  the record of what is hydrated; rclone tracks downloaded ranges separately
  ([kernel_ondemand_mechanisms.md, section 6](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/kernel_ondemand_mechanisms.md);
  [rclone mount docs](https://rclone.org/commands/rclone_mount/)).
- The research's suggested layering is SQLite as source of truth, `user.*` xattrs as the
  public read and pin-request interface, and sparse extents as the physical cache
  ([report, FUSE section](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#fuse-stacked-over-a-real-directory-is-the-mechanism-to-build-on)).
- A 2026 implementer, wusel, removed xattr-based status in favour of live socket queries,
  because status stored on files goes stale and a file copied out of the mount has no status
  to carry ([wusel status protocol](https://wusel.itbh.at/wusel/latest/explanation/status-protocol.html)).

## Decision

- **Store.** The state store is one SQLite database at `$XDG_STATE_HOME/mydrive/state.db`,
  in WAL mode. Only the daemon opens it. The sync engine is its single writer; other daemon
  components read through `mydrive-store` or ask the engine.
- **Migrations.** The schema is versioned. Migrations are forward-only, numbered, and applied
  by `mydrive-store` at daemon start. There are no down-migrations.
- **Keys.** Every item table is keyed by (`drive_id`, `item_id`). Paths are derived from the
  parent chain and are never keys.
- **Per-item contents.** For each item the store holds: parent, name, size, timestamps, eTag,
  cTag where the server provides one, server hash, local hash, the three state axes of
  ADR-0008 (hydration, pin, sync) stored as separate columns, and the map of hydrated byte
  ranges. The hydrated-range map is authoritative; it is never inferred from `SEEK_HOLE`.
- **Extended attributes.** The FUSE layer exposes a public mirror of store state as xattrs on
  every item under a mount point. Readable: `user.mydrive.item_id`, `user.mydrive.hydration`,
  `user.mydrive.pin`, `user.mydrive.sync`, `user.mydrive.status`. Writing `user.mydrive.pin`
  is accepted as a pin request, which the engine processes asynchronously. All other
  `user.mydrive.*` attributes are read-only. Values use the wire forms defined in the
  overview ("Wire forms"): lowercase kebab-case, for example `placeholder`, `pinned`,
  `in-sync`, and `online-only` for `status`.
- **xattrs are never the source of truth.** Values are served from the store; nothing reads
  xattrs back to decide state.
- **Disagreement.** If the store and a backing directory disagree, the store wins for
  metadata. Local content that has not reached the server is never discarded. A recovery scan
  at daemon start reconciles each backing directory with the store before the mount is
  served.

## Alternatives considered

**xattrs only (openvfs style).** No database, state visible with `getfattr`. Rejected: tools
strip xattrs on save, there is no transaction across items, and ID-keyed queries such as
"all descendants of this folder" are impractical.

**A custom file format.** Full control, but we would re-implement atomicity, crash recovery
and indexing that SQLite already provides.

**An embedded key-value store (sled, redb).** Pure Rust, but no relational queries or
indexes across columns, and migrations become our own code. The research did not evaluate
these; this is the lead's judgement.

## Consequences

- State survives daemon crashes and can be inspected with `sqlite3`.
- Scripts and file-manager tools can read state and request pins with `getfattr`/`setfattr`
  and no IPC client.
- Every schema change needs a migration and a test that runs it on a copy of the previous
  schema.
- The single-writer rule serialises store writes through the engine; this may become a
  throughput limit under heavy delta processing.
- The recovery scan adds start-up time proportional to the number of local, not-yet-uploaded
  files.
- **Open concern:** `cp --preserve=xattr` or `rsync -X` copying out of the mount would carry
  `user.mydrive.*` values onto files that are no longer managed, the staleness wusel warned
  about.

## Rules for implementers

- **R1.** No process other than `mydrived` opens `state.db`; within the daemon, only the
  engine issues write statements.
- **R2.** The database is opened with `journal_mode=WAL`; a test asserts it.
- **R3.** Each migration has a number greater than all previous ones; applying migrations to
  any earlier released schema yields the current schema. There is no down-migration code.
- **R4.** Every item table has a primary or unique key that is (`drive_id`, `item_id`); no
  table has a path column used as a key.
- **R5.** Hydration, pin and sync are three separate columns.
- **R6.** The hydrated-range map is read from the store; no code calls `SEEK_HOLE` or
  `SEEK_DATA` to decide what is hydrated.
- **R7.** `getxattr` on the five names returns the store's current values; `setxattr` on
  `user.mydrive.pin` with `Pinned`, `Unpinned` or `Inherit` queues a pin request; `setxattr`
  on any other `user.mydrive.*` name fails with `EPERM`.
- **R8.** When the recovery scan finds a backing file whose state disagrees with the store
  and whose local content has not reached the server, it never deletes, truncates or
  overwrites that content; it updates metadata from the store and keeps the bytes.

## Revisit when

- Store write contention measurably delays hydration or FUSE replies.
- A tool we must support relies on xattrs being stored on disk rather than synthesised.
