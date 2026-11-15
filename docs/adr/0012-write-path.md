# ADR-0012: Write path: durable local write first, asynchronous upload, two hashes per item

- **Status:** Accepted; R5 extended by the overview's rulings (replacing a file always uses a session)
- **Date:** 2026-11-14
- **Depends on gates:** none

## Context

Write-path corruption leads onedriver's issue tracker: large files destroyed when
manipulated, KeePassXC databases corrupted on save, and creation or renames failing when
performed before an upload completes; v0.14.0 introduced file corruption fixed in v0.14.1
([issues index](https://issues.ecosyste.ms/hosts/GitHub/repositories/jstaf%2Fonedriver/issues?page=2),
[onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc),
[Releases](https://github.com/jstaf/onedriver/releases)). rclone's full cache mode writes to
local sparse files, uploads on close and resumes pending uploads after a crash
([rclone mount](https://rclone.org/commands/rclone_mount/)). Principle 1 of the overview puts
data safety above everything else.

Relevant Graph facts:

- Upload sessions accept `If-Match` with an eTag and return `412 Precondition Failed` on
  mismatch; whole files are always re-sent
  ([createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)).
  `If-Match` on a simple `PUT .../content` is background knowledge, not verified in research.
- SharePoint libraries modify some uploaded files (Office, and reportedly eml/msg/TIFF), so the
  size and hash the server reports differ from what was sent. rclone tells users to ignore
  checksum and size; abraunegg offers switches to skip validation or re-upload
  ([rclone OneDrive docs](https://rclone.org/onedrive/),
  [abraunegg config options](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/application-config-options.md)).
  The cause ("property promotion") rests on a community answer
  ([Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/513739/sharepoint-modifying-internal-file-structure-of-wo)).
- Deleted items go to the recycle bin ([rclone OneDrive docs](https://rclone.org/onedrive/)).
- The Windows client appends the device name to the losing copy of a conflict. This comes from
  the research's feasibility assessment; the Windows conflict-naming rule was not read and is
  unverified.

## Decision

**Local first.** Every write lands in the backing file. Before a partial write to a
`Placeholder` or `Partial` item, the item is hydrated to `Full`. An open with `O_TRUNC` needs no
hydration.

**Durable dirty mark.** Before the first modification through a handle touches the backing
file, the engine records sync = `PendingUpload` in the state store and commits it. The mark is
therefore on disk before any local byte differs from the server's, so a crash at any later
point leaves an item the recovery pass knows it must not discard. `fsync` on a file also
fsyncs its backing file before replying.

**Asynchronous upload.** An upload starts after the last close plus a quiet period (default
2 s, configurable). Any further write or open-for-write restarts the period, so an editor that
saves in several steps produces one upload. Files over 4 MiB use an upload session (fragment
rules in ADR-0011). Every content upload of an existing item carries `If-Match` with the
item's known eTag.

**Conflicts.** A 412 is a conflict. The server version is kept under the original name. The
local version is kept as `<name>-<hostname>.<ext>` (no extension: `<name>-<hostname>`) and
uploaded as a new item. Neither version is ever discarded.

**Two hashes.** Each file stores `local_hash`, the quickXorHash of the bytes we uploaded, and
`server_hash`, the hash the server reports after the upload. The item is `InSync` while the
local content still hashes to `local_hash` and the server still reports `server_hash`. A
server-side rewrite after our upload therefore does not trigger a download, re-upload or
conflict, and validation stays on.

**Save via rename.** When a file is renamed over an existing item in the same mount, the
engine treats it as a content update of the existing item: the target keeps its `item_id`
and version history, and the temporary file is never uploaded as an item of its own.

**Deletes.** A local delete becomes a remote delete, which goes to the OneDrive recycle bin.
A mass-delete guard pauses propagation and asks the user for confirmation when one operation
would delete more than 200 items or 50% of a mount (both configurable).

**Recovery.** On daemon start, every item in `PendingUpload` or `Uploading` is resumed. A file
with local changes not yet on the server is never dehydrated and never overwritten by a remote
change.

## Alternatives considered

**Write-through to Graph.** Every write would depend on the network, and a dropped connection
loses data. Rejected.

**Upload on every close without a quiet period.** Multi-step saves would upload partial
states and create extra versions. Rejected.

**Ignore-checksum switch for SharePoint.** What rclone and abraunegg do; it disables the check
that detects real corruption. Rejected in favour of two hashes.

**Delete plus create for save-via-rename.** Loses version history and sharing, and opens a
window where the item does not exist remotely. Rejected.

## Consequences

- The first write through each handle pays for one SQLite commit before it is applied.
- Partial writes to placeholders wait for full hydration; offline, they fail with an error.
- Users see conflict copies they must reconcile by hand.
- The store schema needs `local_hash` and `server_hash` columns per file.
- Open concern: the 4 MiB session threshold is not sourced in the research; the
  createUploadSession page recommends sessions for files over 10 MiB. Using sessions below
  10 MiB is allowed, so this is a cost question, not a correctness one.
- Open concern: "one operation" for the mass-delete guard needs a precise definition (for
  example deletes accumulated for a mount before they are sent), fixed by the engine work
  package.

## Rules for implementers

- **R1.** A partial write to an item whose hydration is not `Full` blocks until it is `Full`, or
  fails with `EIO`; it never writes into an unhydrated region.
- **R2.** An open with `O_TRUNC` does not trigger a download.
- **R3.** The `PendingUpload` record is committed before the first write, truncate or other
  modification through a handle is applied to the backing file. Killing the daemon at any
  point after a write has been acknowledged never leaves a modified backing file whose item
  is recorded as `InSync`.
- **R4.** No upload starts until the quiet period has elapsed since the last close, and any write
  restarts it.
- **R5.** Uploads of more than 4 MiB use an upload session.
- **R6.** Every content upload of an existing item sends `If-Match` with the stored eTag.
- **R7.** On 412 both versions exist afterwards: the server version under the original name, the
  local version as `<name>-<hostname>.<ext>`.
- **R8.** After a successful upload, `local_hash` and `server_hash` are both stored, and a later
  delta reporting `server_hash` leaves the item `InSync` without a download.
- **R9.** Rename over an existing item preserves the target's `item_id`.
- **R10.** A delete batch over the threshold is held until the user confirms.
- **R11.** After a crash, every `PendingUpload` and `Uploading` item is uploaded on restart, and none
  is dehydrated or overwritten by a remote change before that.

## Revisit when

- Crash tests (ADR-0017) find a window where a local change is lost.
- Graph documents server-side rewriting, or adds a way to disable it.
- Users find the quiet period or the mass-delete thresholds wrong in practice.
- Editors are found that save by delete-and-recreate rather than rename.
