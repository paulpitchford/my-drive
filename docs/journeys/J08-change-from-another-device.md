# J08 Change from another device

**Goal:** changes made on the phone, the web or another computer show up in the mount on
their own, without downloading more than the user's pins require and without touching local
changes that have not been uploaded.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 |
| Related flows | F05 remote change, F07 conflict, F10 throttling and scheduler, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online; change notification
  (`subscriptions/socketIo`) is connected.
- `~/OneDrive/Documents/budget.xlsx`, 48,213 bytes, local (`Full`), unpinned, in sync, not
  open in any program.
- `~/OneDrive/Documents/Taxes/` is pinned and fully downloaded.
- `~/OneDrive/Pictures/2026/` holds online-only photos.
- `~/OneDrive/Documents/old-draft.docx`, in sync, online-only.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Edits `budget.xlsx` in Excel on his phone; it grows to 51,002 bytes | — | OneDrive stores a new version |
| 2 | On the laptop, runs `ls -l ~/OneDrive/Documents/budget.xlsx` shortly after | `51002` and the new modification time | The socket notification wakes the mount's worker, which calls delta. The item's eTag, size and hash changed and nothing is pending locally, so its local content is released: hydration `Placeholder` (F13) |
| 3 | `mydrive status ~/OneDrive/Documents/budget.xlsx` | `online-only` | |
| 4 | Opens `budget.xlsx` | The phone's edits are there | Hydration on read fetches the new version (J03) |
| 5 | Takes a photo that the phone uploads to `Pictures/2026/IMG_2045.jpg` | `IMG_2045.jpg` appears in the folder, `online-only` | New placeholder from delta; nothing downloaded |
| 6 | Renames `Pictures/2026` to `Pictures/2026 Holidays` on onedrive.com | The folder has the new name on the laptop; files inside keep their status | Delta reports the folder with a new name; items are tracked by ID, so children move with it and nothing is downloaded again |
| 7 | Deletes `Documents/old-draft.docx` on onedrive.com | It disappears from `~/OneDrive/Documents` | Delta reports the delete; nothing is pending locally, so the local item and its backing file are removed. The server keeps it in the recycle bin |
| 8 | Edits `Taxes/2026/w2.pdf` on the web | `mydrive status` on it shows `syncing`, then `pinned` | Pinned item changed: the new version is downloaded to a staging file, verified, and swapped in whole |
| 9 | Restores `old-draft.docx` from the recycle bin on the web | It reappears in `~/OneDrive/Documents` as `online-only` | Delta reports it again |

## Variations

- **V1 Local changes not yet uploaded.** Sam edited `budget.xlsx` on the laptop too and the
  upload has not happened. The remote change does not overwrite his edits: J10.
- **V2 Remote delete of a file with local changes.** The local edit is kept: J10.
- **V3 File open during the change.** `budget.xlsx` is open in LibreOffice when the phone's
  edit arrives. Its local content is not released while the file has open handles (F13).
  What LibreOffice sees in the meantime is an open question.
- **V4 Laptop asleep or offline.** Changes made meanwhile arrive in one delta catch-up when it
  is back (J09).
- **V5 Notifications unavailable.** The socket is down; the mount falls back to slow polling.
  Changes still arrive, later.
- **V6 Resync (`410 Gone`).** The server tells the client to resync. The mount shows
  `resyncing`; the user sees no change in their files except that new remote changes may take
  longer. Where the client cannot tell which side is newer, it keeps both copies.
- **V7 Large remote delete.** Someone deletes a folder of 600 items on the web. See open
  questions.

## Acceptance criteria

- **J08-AC1** Given an online-only, unpinned file, when the simulator changes its content,
  then after the next delta its `st_size` and `st_mtime` match the new version, its hydration
  is still `placeholder`, and no content was downloaded.
- **J08-AC2** Given a local, unpinned, in-sync file with no open handles, when the simulator
  changes its content, then after the next delta its hydration is `placeholder`, and a read
  opened afterwards returns only the new version's bytes.
- **J08-AC3** Given a pinned, fully downloaded file, when the simulator changes its content,
  then the file reaches display status `pinned` with the new content without any local read,
  and no read during the update returns a mixture of old and new bytes.
- **J08-AC4** Given the simulator adds a file in a folder that is not pinned, when the next
  delta is applied, then the file appears with the correct name, size and time, hydration
  `placeholder`, and no content is downloaded.
- **J08-AC5** Given a folder containing local files, when the simulator renames it, then the
  local path changes to the new name, every child keeps its hydration and pin, and no content
  is downloaded or uploaded.
- **J08-AC6** Given an in-sync file with nothing pending locally, when the simulator deletes
  it, then it disappears from the local listing and the client sends no upload or create for
  it.
- **J08-AC7** Given a file deleted on the server and listed locally no more, when the
  simulator restores it, then it reappears in the listing as `online-only`.
- **J08-AC8** Given the simulator answers delta with `410 Gone`, when the mount resyncs, then
  the local tree matches the server's afterwards, every local change pending before the
  resync still exists locally or on the server, and the client sent no delete request.
- **J08-AC9** Given a local, unpinned file held open by a process, when the simulator changes
  its content, then the file's hydration does not change until the last handle is closed.

## Must never happen

- A remote change overwrites or discards local changes that are not on the server.
- A remote rename or move is applied as a delete plus a new download, or loses a pin.
- A resync deletes anything on the server, or deletes local content that is not on the
  server.
- A file's bytes from two server versions are served in one read sequence.

## Open questions

1. Is there a target time from a change on the server to its appearance in the mount when
   notifications work, and what is the polling interval when they do not?
2. What does a process holding the old file open see after the remote change: the old bytes
   until it closes (as written above), or an error?
3. Should a remote delete of many items (for example 600) be held for confirmation like a
   local one, or applied? The server keeps them in the recycle bin, but applying it removes
   local copies, including pinned ones the user relies on offline.
4. After a remote content change, an unpinned local file reverts to online-only (F13). Should
   files the user opened recently be re-downloaded instead, as Windows tends to do?
