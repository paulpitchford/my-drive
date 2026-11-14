# J05 Create, rename, move and delete

**Goal:** ordinary file operations in the mount (`mkdir`, `cp`, `mv`, `rm`) do the same thing
on OneDrive; deleted items go to the OneDrive recycle bin, and a large delete waits for the
user's confirmation.

| | |
|---|---|
| Persona | Sam (Priya in V6) |
| Milestone | M2 |
| Related flows | F04 local write and upload, F12 delete safety, F05 remote change (for restores), F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active, about 14,000 items; the machine is online.
- `~/OneDrive/Documents/old-draft.docx`, local, in sync.
- `~/OneDrive/Projects/old-site/`, 340 items, all online-only.
- `~/OneDrive/Videos/holiday-2026.mp4`, 2.3 GiB, online-only.
- Mass-delete thresholds at their defaults: more than 200 items or 50% of a mount in one
  operation.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mkdir -p ~/OneDrive/Documents/Taxes/2026` | The folders exist at once | A pending create record per folder is committed before the change is applied locally; the folders are created on the server, parent first |
| 2 | `cp ~/Downloads/receipt-0412.pdf ~/OneDrive/Documents/Taxes/2026/` | Copy completes | Upload after close plus 2 s (J04) |
| 3 | `mv ~/OneDrive/Documents/Taxes/2026/receipt-0412.pdf ~/OneDrive/Documents/Taxes/2026/dentist-2026-04.pdf` | Renamed at once | One rename request; the item keeps its `item_id`; no content is uploaded |
| 4 | `mv ~/OneDrive/Documents/Taxes ~/OneDrive/Archive/` | Moved at once | One move request for the folder. Children keep their `item_id`, hydration and pin; nothing downloads or uploads |
| 5 | `rm ~/OneDrive/Documents/old-draft.docx` | The file is gone | Delete request; the item goes to the OneDrive recycle bin |
| 6 | Opens the recycle bin on onedrive.com | `old-draft.docx` is there and can be restored | — |
| 7 | `rm -r ~/OneDrive/Projects/old-site` | `rm` finishes. A notification (M3) and `mydrive activity` say a deletion of 340 items in `Projects/old-site` is waiting for confirmation; `mydrive status` shows the mount `paused` | The delete batch exceeds 200 items, so the mass-delete guard holds it and no delete is sent (ADR-0012) |
| 8 | Confirms the deletion | `mydrive activity` shows the 340 items deleted; the mount is `active` again | Deletes sent; all 340 items go to the recycle bin |
| 9 | `mv ~/OneDrive/Videos/holiday-2026.mp4 ~/Videos/` | `mv` takes as long as downloading 2.3 GiB, then finishes | The kernel returns `EXDEV` for a cross-filesystem rename, so `mv` copies and then unlinks. The copy reads the file, which hydrates it; the unlink sends the item to the recycle bin |

## Variations

- **V1 Sam declines the held deletion.** Nothing is deleted on the server. What the folder
  looks like locally afterwards is an open question.
- **V2 Small mount, large share.** In a mount of 40 items, `rm -r` of a folder holding 21
  items (more than 50%) is held the same way.
- **V3 Name OneDrive does not allow.** `touch '~/OneDrive/Documents/Q3: plan.txt'` succeeds
  locally, but the name contains `:`. It is not uploaded; the item shows `error` and
  `mydrive status` gives the reason. Renaming it to a valid name clears the error and uploads
  it. The same applies to trailing spaces, reserved names such as `CON` or `desktop.ini`,
  names starting `~$`, and paths over 400 characters.
- **V4 Names differing only by case.** Creating `Report.txt` next to `report.txt` is a case
  collision; the new item is not uploaded and shows `error` (ADR-0015).
- **V5 Name that is not UTF-8.** The VFS refuses to create it (overview, Rulings).
- **V6 Move between mounts (Priya).** `mv ~/OneDrive\ -\ Contoso/plan.docx
  ~/Contoso/Finance\ -\ Documents/` is a copy plus a delete, as in step 9. The original goes to
  the recycle bin of Priya's OneDrive; version history does not move with it.
- **V7 Delete of a file never uploaded.** A file created and removed within the quiet period
  never reaches the server; nothing is sent.
- **V8 Restore from the recycle bin.** Sam restores `old-draft.docx` on onedrive.com; it
  reappears in `~/OneDrive/Documents` (J08).
- **V9 Delete offline.** The delete is queued and sent on reconnect; the guard still applies.

## Acceptance criteria

- **J05-AC1** Given `mkdir -p` creates two nested folders in the mount, when the engine has
  synced, then the simulator has both folders under the correct parents and both show
  `local` in `mydrive status --json`.
- **J05-AC2** Given an in-sync file, when it is renamed within its folder, then the simulator
  item keeps its `item_id` and has the new name, and no content upload request was made.
- **J05-AC3** Given a folder of 50 online-only files, when it is moved to another folder in
  the same mount, then the simulator received one move request, every child keeps its
  `item_id`, no content was downloaded or uploaded, and every child's hydration is still
  `placeholder`.
- **J05-AC4** Given an in-sync file, when it is deleted with `rm`, then the simulator shows it
  in the recycle bin and not permanently deleted.
- **J05-AC5** Given a folder of 340 items, when it is removed with `rm -r`, then the simulator
  receives no delete request until the user confirms, and `mydrive activity --json` reports a
  held deletion naming the folder and the count 340.
- **J05-AC6** Given the mass-delete guard at its defaults, when one `rm -r` deletes a folder
  of exactly 200 items, then the deletes are sent without asking; when it deletes a folder of
  201 items, then they are held.
- **J05-AC7** Given a mount of 40 items, when one `rm -r` deletes a folder of 21 items, then
  the deletes are held.
- **J05-AC8** Given a held deletion of 340 items, when the user confirms, then all 340 items
  are in the simulator's recycle bin and none is permanently deleted.
- **J05-AC9** Given a local file named `Q3: plan.txt`, when the engine processes it, then the
  simulator never receives an upload or create for that name, and `mydrive status <path>
  --json` reports sync `error` with a reason naming the invalid character.
- **J05-AC10** Given an online-only file moved out of the mount with `mv`, when `mv`
  finishes, then the destination file is byte-identical to the server's content and the
  server item is in the recycle bin.

## Must never happen

- A delete bypasses the recycle bin.
- A delete over the threshold reaches the server before the user confirms.
- A rename or move inside one mount is carried out as delete plus upload, or downloads or
  uploads content.
- A file with changes not yet uploaded is deleted on the server because a local rename was
  misread as a delete.
- An item whose name the server would reject is sent anyway and fails half-way.

## Open questions

1. How does the user confirm or decline a held deletion: a notification action, a CLI
   command (none is listed in the overview), the UI, or all of these?
2. While a deletion is held, do the items stay absent locally, or reappear? If declined, are
   they restored locally as placeholders?
3. Does a held deletion expire, and does it survive a daemon restart (J16 assumes it does)?
4. While the guard holds a delete, the mount is `paused` (F13), so uploads also wait. Is that
   intended, or should only the deletes wait?
5. Is the 50% boundary "more than 50%" or "50% or more"? This journey tests 21 of 40 only.
6. "One operation" is undefined (open-questions Q6); J05-AC6 assumes one `rm -r` of one folder
   is one operation.
7. Symbolic links, hard links, FIFOs and device nodes in the mount: refused, kept local only,
   or something else?
8. Does the guard also apply to many deletes arriving from the server (J08)?
