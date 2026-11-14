# J07 Free up space

**Goal:** the user releases the local copy of files that are safely on OneDrive, getting the
disk space back while the files stay visible and openable on demand; anything not yet
uploaded is never freed.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 (the right-click action in V1 is M3) |
| Related flows | F06 pin and dehydrate, F04 local write and upload, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Videos/2025/` holds 18 files, 41.2 GiB, all local (`Full`, `InSync`), pin
  `Inherit` with no pinned ancestor.
- `~/OneDrive/Documents/Taxes/` is pinned and fully downloaded (J06).
- `~/OneDrive/Documents/notes.md` was saved one second ago and has not uploaded yet
  (`PendingUpload`).
- `df -h ~` shows 6.0G available.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `du -sh ~/OneDrive/Videos/2025` | `42G` | |
| 2 | `mydrive free ~/OneDrive/Videos/2025` | A summary: 18 files freed, 41.2 GiB | For each file: checks sync `InSync`; sets pin `Unpinned` if its effective pin is not already unpinned; punches holes in the backing file; deletes its extent records; hydration `Placeholder`. No request goes to Graph |
| 3 | `du -sh ~/OneDrive/Videos/2025`, `ls -l`, `df -h ~` | `du` about 0; `ls -l` shows the same sizes and dates as before; about 47G available | |
| 4 | `mydrive status ~/OneDrive/Videos/2025/ski-trip.mp4` | `online-only` | |
| 5 | Plays `ski-trip.mp4` again | It plays, downloading as it goes | Hydration on read (J03) |
| 6 | `mydrive free ~/OneDrive/Documents/Taxes/2025/return.pdf` | The file is freed; its status becomes `online-only` | Its effective pin was `Pinned` (inherited). As an explicit user request, `free` sets the file's own pin to `Unpinned`, then dehydrates it (overview, Rulings). The folder `Taxes` stays pinned |
| 7 | `mydrive free ~/OneDrive/Documents/notes.md` | Refused: the file has changes that are not uploaded yet. Exit status non-zero | Sync is `PendingUpload`; nothing changes, pin included |
| 8 | `mydrive free ~/OneDrive/Documents` | Eligible files freed; a list of skipped files with the reason for each (here `notes.md`, not uploaded yet) | Each item checked separately |

## Variations

- **V1 From Files (M3).** Right-click → "Free Up Space" runs the same action; a refusal shows
  as a desktop notification (J17).
- **V2 File in conflict or error.** Refused in the same way as step 7: only `InSync` items
  can be freed.
- **V3 Already online-only.** Nothing to do; reported as such, not as an error.
- **V4 Partly downloaded file.** A `Partial` file is freed to `Placeholder`.
- **V5 Offline.** Freeing works offline for in-sync files, since it needs no network. Those
  files cannot be opened again until the network returns.
- **V6 Read-only library (Priya).** Freeing works the same in a read-only library; it changes
  nothing on the server.
- **V7 A write arrives during the free.** An editor writes to the file at the same moment. The
  write is never lost: either the free is refused or the file ends up with the write's
  content and `PendingUpload`.

## Acceptance criteria

- **J07-AC1** Given a folder of 18 local, in-sync, unpinned files, when `mydrive free <folder>`
  completes, then every file reports hydration `placeholder` and display status
  `online-only`, the sum of `st_blocks` over the files is 0, and each file's `st_size` and
  `st_mtime` are unchanged.
- **J07-AC2** Given J07-AC1, when the simulator's request log and item metadata are inspected,
  then the free caused no request at all and every item's eTag is unchanged.
- **J07-AC3** Given a file whose effective pin is `Pinned` through its parent folder, when
  `mydrive free <file>` runs, then the file's own pin becomes `unpinned`, its hydration
  becomes `placeholder`, and the parent folder's pin is still `pinned`.
- **J07-AC4** Given a file with sync `PendingUpload` or `Uploading`, when `mydrive free
  <file> --json` runs, then it exits non-zero, reports the file as refused with a reason
  identifying changes not yet uploaded, and the file's hydration, pin and `st_blocks` are
  unchanged; the upload then completes normally.
- **J07-AC5** Given a file with sync `Conflict` or `Error`, when `mydrive free <file>` runs,
  then it is refused and the file's hydration is unchanged.
- **J07-AC6** Given a folder holding both eligible files and one `PendingUpload` file, when
  `mydrive free <folder> --json` runs, then the eligible files are `placeholder`, the pending
  file is untouched, and the JSON output lists the pending file as skipped with its reason.
- **J07-AC7** Given a file freed by `mydrive free`, when a process reads it, then it is
  hydrated again and the bytes equal the simulator's content.
- **J07-AC8** Given a process repeatedly writing to a file while `mydrive free` is run on it
  many times, when both stop and the quiet period passes, then the server's content and the
  local content both equal the last write.

## Must never happen

- A file with changes not yet on the server is freed.
- Freeing deletes, renames or changes anything on the server.
- Freeing changes a file's size or modification time, so that the engine sees a change and
  uploads an empty or sparse file.
- The engine frees a pinned file on its own; only an explicit user request unpins it first.

## Open questions

1. Should `mydrive free` (or "Free Up Space") refuse or warn for a file currently open by a
   process, such as a video playing in mpv?
2. Should freeing while offline warn that the files cannot be reopened until reconnected?
3. What exit status does `mydrive free` return when some items were freed and others skipped?
4. Is there automatic freeing of unused, unpinned files (as Windows Storage Sense does), or
   only explicit frees? The overview mentions neither.
5. Does `mydrive free` on a folder set the folder's own pin to `Unpinned`, or only its
   files'? This affects new files arriving later under a pinned ancestor.
