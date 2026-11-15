# ADR-0021: Content files in the backing directory are keyed by a stable local ID

- **Status:** Accepted (conflict-split wording corrected 2026-11-15, before any implementation)
- **Date:** 2026-11-15
- **Depends on gates:** none

Amends ADR-0005: the backing directory no longer mirrors names and folders. Everything else
in ADR-0005 stands, including the separate mount point and backing directory.

## Context

ADR-0005 left open how files are laid out inside a mount's backing directory, and the flows
assumed a tree that mirrors the remote names (F02 open question 1). Planning the sync engine
showed what a mirrored tree costs:

- A rename or move must change both the store and the disk, in two steps that a crash can
  separate.
- A conflict split must rename a content file while the store changes, leaving a window in
  which the user's bytes are under a name the store does not expect.
- First enumeration must create a placeholder file for every item: 300,000 files for a large
  library, before the mount is usable.
- Names valid on OneDrive are not always usable as local file names in a mirrored tree
  (length, case collisions on some filesystems).

The store is already the source of truth for names, parents and sizes (ADR-0006), and the
FUSE layer (M2) serves the namespace from it.

## Decision

Every item row has a `local_id`: an integer assigned when the row is created, unique within
the store, never changed for the life of the row (it survives `rekey_item`, `complete_upload`
and adopting a server item), and **never reused** after the row is removed.

A file's content lives at `mounts/<mount_id>/content/<local_id>`. The engine may add fixed
sub-directory fan-out; the path is a pure function of `mount_id` and `local_id`.

- A content file exists only for a file that has, or has had, local content. A placeholder
  that has never been hydrated has no content file. There is no materialisation phase.
- Folders have no on-disk representation in the backing directory.
- Renames and moves change only the store.
- A conflict split is one store transaction and no file is renamed: the row that holds the
  local bytes becomes the conflict copy (it keeps its `local_id` and its content file, takes
  the copy's name and becomes a not-yet-uploaded item), and a new row with a new `local_id`
  is created for the server's version under the original name.
- A content file with no row is an orphan, found by the recovery scan and never interpreted
  as an item. A row whose content file is missing is handled by recovery: for a clean item it
  becomes a placeholder; for a dirty item it is an error state, and it never becomes a remote
  delete.
- Removing a mount writes each fully local file into the mount-point directory at its path
  (ADR-0015 R8), since the backing directory is not browsable.
- In M2 the `local_id` is the natural FUSE inode number.

## Alternatives considered

**A name-mirrored tree.** Browsable without the daemon, but every rename, move and conflict
split becomes a two-step operation across the store and the disk, and enumeration must create
a file per item.

**Keying by the server's item ID.** Items created locally have no server ID yet, and the ID
changes when one is assigned; that reintroduces a rename.

## Consequences

- The backing directory is not something a user can browse or rescue files from by hand.
  `mydrive` must offer a way to export local content, and mount removal must write a named
  tree.
- Flows F02, F05, F07 and F08 are amended: no placeholder files, no on-disk rename.
- The store gains a column and an allocation rule (schema version 2).
- xattrs are served from the store by the VFS and are not written on content files.

## Rules for implementers

- **R1.** A `local_id` is assigned exactly once per row, is unique in the store, and is never
  reused after its row is removed, including across restarts.
- **R2.** `rekey_item`, `complete_upload` with a new server ID, adopting a server item and
  local moves leave `local_id` unchanged.
- **R3.** The content path is computed only from `mount_id` and `local_id`; no code builds a
  backing path from an item name.
- **R4.** No file or directory is created in the backing directory for a folder or for a
  never-hydrated placeholder.
- **R5.** A content file with no matching row is never read as an item's content, and a
  missing content file never causes a remote delete.

## Revisit when

- Users need to recover content from the backing directory without the daemon.
- A filesystem limit makes one flat content directory per mount impractical despite fan-out.
