# J04 Edit and save

**Goal:** the user edits a file in any editor and saves; the save is durable on local disk at
once and reaches OneDrive shortly after, as a new version of the same file.

| | |
|---|---|
| Persona | Sam (Priya in V7) |
| Milestone | M2 |
| Related flows | F04 local write and upload, F07 conflict, F10 throttling and scheduler, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Documents/notes.md`, 3,412 bytes, local (hydration `Full`), in sync.
- `~/OneDrive/Documents/budget.xlsx`, 48,213 bytes, online-only.
- `~/Downloads/backup-photos.zip`, 1,288,490,188 bytes (1.2 GiB), outside the mount.
- Quiet period at its default of 2 seconds.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `nvim ~/OneDrive/Documents/notes.md`, edits, `:w` | `"notes.md" written` | Writes land in the backing file. The engine commits sync `PendingUpload` before the reply to `flush`/`release`. If Neovim saves by writing a temporary file and renaming it over `notes.md`, the rename is treated as a content update of `notes.md` (ADR-0012) |
| 2 | Runs `mydrive status ~/OneDrive/Documents/notes.md` straight away | `syncing` | Quiet timer running: 2 s from the last close |
| 3 | Waits a few seconds and runs it again | `local` | After 2 s with no write or open-for-write, the engine snapshots the file and uploads it with `If-Match: <stored eTag>`. On success it stores `local_hash`, `server_hash`, eTag and size; sync `InSync` |
| 4 | `mydrive activity` | An entry: `Documents/notes.md` uploaded | |
| 5 | Opens notes.md's version history on onedrive.com | The previous version is listed; the current version has the edit | The upload replaced the content of the existing item; `item_id` unchanged |
| 6 | Opens `budget.xlsx` in LibreOffice Calc, changes a cell, saves with Ctrl+S (keep format) | LibreOffice saves without complaint | Reading hydrated the file (J03). LibreOffice's write to a non-`Full` file would first hydrate it fully; here it is already `Full`. Upload follows 2 s after LibreOffice closes its write handle |
| 7 | `cp ~/Downloads/backup-photos.zip ~/OneDrive/Archive/` | `cp` finishes at local disk speed | New item, hydration `Full`, sync `PendingUpload`. 2 s after close the engine opens an upload session and sends fragments that are multiples of 320 KiB (default 10 MiB) |
| 8 | `mydrive activity` while the upload runs | The upload with bytes sent so far; `mydrive status` on the file shows `syncing` | Sync `Uploading` |
| 9 | Waits | Status `local`; the zip is visible on onedrive.com | |

## Variations

- **V1 Repeated saves.** An editor autosaves every second. Each write or open-for-write
  restarts the quiet period; one upload follows the last save.
- **V2 Save during an upload.** Sam saves again while the previous upload is in flight. The
  in-flight upload finishes; another cycle uploads the newer content. The server ends with the
  last save.
- **V3 Overwrite without reading.** `echo "draft" > ~/OneDrive/Documents/todo.txt` on an
  online-only file opens it with `O_TRUNC`; nothing is downloaded.
- **V4 Append to an online-only file.** `echo line >> log.txt` first hydrates the whole file,
  then appends. Offline, the write fails with `EIO` (J09).
- **V5 Offline save.** The save succeeds locally; the upload waits for the network (J09).
- **V6 Storage full.** OneDrive answers 507. The file stays as saved locally, shows `error`,
  and `mydrive activity` says the OneDrive storage is full. It uploads once space is freed and
  the upload is retried.
- **V7 SharePoint rewrites the file (Priya).** Priya saves `~/Contoso/Finance -
  Documents/forecast-2027.xlsx`. After upload, SharePoint adds its metadata, so the server
  reports a different size and hash. Nothing else happens: no download, no re-upload, no
  conflict. The file shows `local`.
- **V8 Changed on the server meanwhile.** The upload gets 412: J10.
- **V9 Invalid name.** Covered in J05.

## Acceptance criteria

- **J04-AC1** Given a local file, when a process writes new content and closes it, then an
  immediate read through the mount returns the new content, with the simulator unreachable or
  reachable alike.
- **J04-AC2** Given a write and close at time T, when the simulator's request log is
  inspected, then no upload request for that item started before T + 2 s.
- **J04-AC3** Given a process that opens, writes and closes a file five times at 1-second
  intervals, when the last close is followed by 2 s without writes, then the simulator
  receives exactly one content upload for that item, and its body is the final content.
- **J04-AC4** Given a closed write is uploaded, when `mydrive status <path> --json` runs after
  the upload, then sync is `in-sync` and display status `local`, and the simulator's content
  for the item is byte-identical to the local file.
- **J04-AC5** Given a write was closed less than 2 s ago, when `mydrive status <path> --json`
  runs, then sync is `pending-upload` and display status `syncing`.
- **J04-AC6** Given an existing item, when it is saved by writing in place or by writing a
  temporary file and renaming it over the original, then after upload the simulator item has
  the same `item_id` as before, its previous content is in its version history, and no item
  with the temporary file's name exists on the server.
- **J04-AC7** Given every content upload of an existing item, when the simulator's request log
  is inspected, then each carries `If-Match` with the eTag the client last stored.
- **J04-AC8** Given a new 1.2 GiB file is closed in the mount, when it uploads, then the
  client uses an upload session whose fragments are each a multiple of 320 KiB and under
  60 MiB, and the simulator's content equals the local file.
- **J04-AC9** Given a library mount where the simulator rewrites uploaded Office files, when a
  `.xlsx` is saved and uploaded, then over the following 5 minutes of simulated time the
  simulator receives no further upload or download for that item, the local bytes are
  unchanged, and its sync stays `in-sync`.
- **J04-AC10** Given the simulator answers uploads with 507, when a saved file fails to
  upload, then the local content is unchanged, its display status is `error`, and
  `mydrive activity --json` reports a reason identifying storage as full.

## Must never happen

- A save reported as successful by the editor is lost, whatever the network or server does.
- A partially written file (writer still open, or quiet period not over) is uploaded.
- An upload overwrites a newer server version (always `If-Match`).
- A save via rename shows up as a delete plus a new file, losing version history and sharing.
- A server-side rewrite after upload triggers a download that replaces the user's local bytes.

## Open questions

1. A program that keeps a file open for hours (a database, a VM image) never closes it. Is
   there any upload before close, for example after `fsync`?
2. Are editor temporaries and lock files (`.~lock.budget.xlsx#`, `.notes.md.swp`, `~$*`)
   uploaded, skipped by a built-in ignore list, or left to name validation?
3. Is there an upper bound on the delay between the end of the quiet period and the start of
   the upload when the scheduler is idle?
4. How are uploads ordered when many files are pending (size, age, mount)?
5. On a 423 Locked response (a file open in Office for the web), how long does the client
   retry and what does the user see?
6. What exit status and message does an editor get when a write to an online-only file fails
   because hydration could not complete?
