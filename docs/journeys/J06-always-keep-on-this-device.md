# J06 Always keep on this device

**Goal:** the user pins a folder so that everything in it, now and in future, is downloaded
and stays available offline.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 (the right-click action in V1 is M3) |
| Related flows | F06 pin and dehydrate, F05 remote change, F10 throttling and scheduler, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Documents/Taxes/` holds 230 files, 410 MiB in total, all online-only, pin
  `Inherit` throughout.
- At least 2 GiB of free local disk space.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mydrive pin ~/OneDrive/Documents/Taxes` | The command returns at once and confirms the pin | Pin `Pinned` committed on the folder. Every descendant file has effective pin `Pinned` and hydration not `Full`, so each moves to sync `PendingDownload` and is queued with the request scheduler |
| 2 | `mydrive status ~/OneDrive/Documents/Taxes/2025/return.pdf` | `syncing` | Pinned but not yet `Full` shows `syncing` (F13) |
| 3 | `mydrive activity` | Downloads in progress for files under `Documents/Taxes` | Whole-file downloads, hash-verified |
| 4 | Waits, then `du -sh ~/OneDrive/Documents/Taxes` | `410M` | All 230 files `Full`, `InSync` |
| 5 | `mydrive status ~/OneDrive/Documents/Taxes/2025/return.pdf` | `pinned` | Own pin `Inherit`, effective pin `Pinned`, hydration `Full` |
| 6 | Turns Wi-Fi off and opens `return.pdf` | It opens | Local content, no network needed |
| 7 | On his phone, saves `Taxes/2026/w2.pdf` into the folder; turns Wi-Fi back on | Within a short time `w2.pdf` is in `~/OneDrive/Documents/Taxes/2026/` and shows `pinned` without Sam opening it | Delta reports a new item under a pinned folder; it is created as a placeholder, moved to `PendingDownload` and downloaded |
| 8 | On the web, creates a new subfolder `Taxes/2027/` with a file in it | The subfolder and file arrive and show `pinned` | Effective pin is inherited at any depth |
| 9 | `mydrive unpin ~/OneDrive/Documents/Taxes` | The command confirms; `du -sh` still `410M`; status of the files is `local` | Pin back to `Inherit`; nothing is dehydrated. `Unpinned` (here, the root default) means "may be dehydrated", not "dehydrate now" |

## Variations

- **V1 From Files (M3).** Right-click on `Taxes` → "Always Keep on This Device" runs the same
  pin (J17).
- **V2 Pin a single file.** `mydrive pin ~/OneDrive/Videos/holiday-2026.mp4` downloads that
  file and keeps it.
- **V3 Pin while offline.** The pin is recorded and the files show `syncing`; downloads start
  when the network returns.
- **V4 A pinned file changes on another device.** The new version downloads automatically and
  is swapped in whole; the file never drops back to online-only (J08).
- **V5 One file cannot be downloaded.** The server answers 403 for `Taxes/scan-07.tif`. That
  file shows `error` with the reason in `mydrive activity`; the other 229 reach `pinned`.
- **V6 Not enough disk space.** See open questions.
- **V7 Pin then free.** `mydrive free` on a pinned item unpins it and frees it (J07).
- **V8 Crawlers.** Pinned content is never refused to indexers or thumbnailers (ADR-0013).

## Acceptance criteria

- **J06-AC1** Given a folder of 230 online-only files, when `mydrive pin <folder>` runs and the
  downloads finish, then every file's content equals the simulator's, every file reports
  hydration `full` and display status `pinned`, and the folder reports pin `pinned`.
- **J06-AC2** Given the pin in J06-AC1, when a child file's status is read with `--json`, then
  its own pin is `inherit` and its effective pin is `pinned`.
- **J06-AC3** Given a pinned folder whose downloads have not finished, when a not-yet-
  downloaded child's status is read with `--json`, then its sync is `pending-download` and
  display status `syncing`.
- **J06-AC4** Given a pinned folder, when the simulator adds a new file under it, then the
  file is hydrated to `full` without any local process reading it, and shows `pinned`.
- **J06-AC5** Given a pinned folder, when the simulator adds a new subfolder containing a
  file, then that file too reaches `full` and `pinned` without a local read.
- **J06-AC6** Given a pinned, fully downloaded folder, when `mydrive unpin <folder>` runs, then
  the folder's pin is `inherit`, every child's hydration is still `full`, the sum of
  `st_blocks` under the folder is unchanged, and every child shows `local`.
- **J06-AC7** Given the simulator is unreachable, when `mydrive pin <folder>` runs, then it
  succeeds and records the pin, and once the simulator is reachable again every child reaches
  `pinned` with no further command.
- **J06-AC8** Given a pinned folder in which the simulator refuses one file with 403, when the
  downloads finish, then that file shows `error`, `mydrive activity --json` names it with the
  reason, and every other file shows `pinned`.
- **J06-AC9** Given a pinned, fully downloaded file, when the simulator replaces its content,
  then at every moment observed during the update its hydration is `full`, and afterwards a
  read returns the new content.

## Must never happen

- The engine dehydrates an item whose effective pin is `Pinned` on its own initiative.
- A new child of a pinned folder is left online-only.
- `mydrive pin` downloads anything outside the pinned path.
- `mydrive unpin` frees space or deletes anything.

## Open questions

1. What happens when the pinned content does not fit on the local disk: refuse up front,
   download until full and mark the rest `error`, or warn?
2. `mydrive free` on a child of a pinned folder sets that child to `Unpinned` (overview,
   Rulings). When the user later runs `mydrive pin` on the parent again, does the child's
   explicit `Unpinned` survive, or is it reset to `Inherit`?
3. How are pin downloads prioritised against interactive reads (J03) on the same tenant
   budget?
4. When an item moves out of a pinned folder, it keeps its content but its effective pin
   changes. Is that the intended outcome?
5. What does the pinned folder itself display while its children download (F13 open
   question 1, folder display)?
6. Is a pin kept when a mount is removed and added again?
