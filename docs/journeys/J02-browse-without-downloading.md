# J02 Browse without downloading

**Goal:** the user lists, searches and inspects the whole drive from a terminal or a file
manager, sees real names, sizes and dates, and nothing downloads.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 |
| Related flows | F02 add mount and initial sync, F03 hydrate on read (as the thing that must not happen), F11 status for file managers, F13 state machines |

## Preconditions

- J01 done: `~/OneDrive` is mounted and active, initial sync complete.
- Every file is online-only. Examples:
  - `~/OneDrive/Documents/budget.xlsx`, 48,213 bytes, last modified on the server
    2026-11-02 14:31:07.
  - `~/OneDrive/Documents/` holds 120 files.
  - `~/OneDrive/Pictures/2026/` holds 1,240 JPEG files, 6.8 GiB in total.
- The machine is online.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `ls ~/OneDrive/Documents` | 120 names, at once | VFS answers `readdir` from the state store |
| 2 | `ls -l ~/OneDrive/Documents/budget.xlsx` | `-rw-r--r-- 1 sam sam 48213 Nov  2 14:31 budget.xlsx` | `getattr` from the store: `st_size` is the cloud size, `st_mtime` the server time in whole seconds |
| 3 | `stat ~/OneDrive/Documents/budget.xlsx` | `Size: 48213  Blocks: 0` and the modification time `2026-11-02 14:31:07` | No content is local, so `st_blocks` is 0 |
| 4 | `du -sh ~/OneDrive/Pictures/2026` | `0` (or a few KiB) | `du` sums `st_blocks` |
| 5 | `du -sh --apparent-size ~/OneDrive/Pictures/2026` | `6.8G` | `du` sums `st_size` |
| 6 | `find ~/OneDrive -name '*.pdf'` | The matching paths | Only `readdir` and `lookup`; answered from the store |
| 7 | `mydrive status ~/OneDrive/Documents/budget.xlsx` | `online-only`; with `--json`, hydration `placeholder`, pin `inherit`, sync `in-sync` | IPC `item.status` |
| 8 | `getfattr -d -m user.mydrive ~/OneDrive/Documents/budget.xlsx` | `user.mydrive.hydration="placeholder"`, `user.mydrive.status="online-only"` and the other mirrored values | xattrs served from the store (ADR-0006) |
| 9 | Opens `~/OneDrive/Pictures/2026` in a file manager | Folder contents with names, sizes and dates; thumbnails where the cloud has them (J17, J18) | File manager lists and stats every entry; the daemon serves metadata only. Thumbnail seeding from Graph is covered by J18 |
| 10 | `mydrive activity` | No downloads | Nothing was hydrated |

## Variations

- **V1 Offline.** Sam lists folders with the network down. Listings and `stat` return the
  same results as online, because metadata is in the store (J09).
- **V2 A tool reads content.** `file budget.xlsx`, `head`, `less` or `md5sum` read bytes and
  therefore hydrate (J03). This is expected: reading is what downloads.
- **V3 `grep -r` or a backup tool.** Reading everything hydrates everything; the crawler
  defence in J18 applies.
- **V4 Remote change while browsing.** A file added on another device appears in the next
  listing as a placeholder (J08).
- **V5 Case.** Sam types `ls ~/OneDrive/documents` (lower case) where the folder is
  `Documents`. What the mount answers is an open question.
- **V6 Very large folder.** A folder with 30,000 entries lists from the store; no request goes
  to Graph per entry.

## Acceptance criteria

- **J02-AC1** Given a folder of online-only files, when `ls -l` lists it, then every name,
  `st_size` (exact bytes) and `st_mtime` (to the second) equals the simulator's metadata, and
  the simulator recorded zero content download requests.
- **J02-AC2** Given an online-only file, when `stat` is called, then `st_blocks` is 0 and
  `st_size` equals the cloud size.
- **J02-AC3** Given a folder whose 1,240 files are all online-only, when `du -s` and
  `du -s --apparent-size` run on it, then the sum of `st_blocks` over its files is 0 and the
  apparent size equals the sum of the cloud sizes.
- **J02-AC4** Given a fully enumerated mount, when a test program calls `opendir`, `readdir`,
  `lstat`, `open` (read-only, then `close` without reading), `listxattr` and `getxattr` on
  every entry of the mount, then the simulator recorded zero content download requests and
  every file's hydration is still `placeholder`.
- **J02-AC5** Given an online-only file, when `mydrive status <path> --json` runs, then it
  reports hydration `placeholder`, pin `inherit`, sync `in-sync` and display status
  `online-only`.
- **J02-AC6** Given an online-only file, when its `user.mydrive.hydration` and
  `user.mydrive.status` xattrs are read, then they are `placeholder` and `online-only`, the
  same values `mydrive status --json` reports.
- **J02-AC7** Given the simulator is unreachable, when any folder in the mount is listed and
  stat-ed, then the results equal those returned while online and no call fails.

## Must never happen

- Listing a folder, `stat`, `open` without a read, or reading xattrs starts a download.
- A placeholder reports size 0, or a size other than the cloud size.
- A placeholder reports allocated blocks, so `du` claims space that is not used.
- A folder shows fewer entries than the server has without saying so.

## Open questions

1. Is name lookup case-sensitive or case-insensitive? OneDrive is case-insensitive; Linux
   tools assume case-sensitive names.
2. Folder display status: by the current derivation every listed folder shows `local`, even
   when all its files are online-only (F13 open question 1).
3. Is there a latency target for listing a large folder (for example 30,000 entries)?
4. Which server timestamp becomes `st_mtime` (`lastModifiedDateTime` or
   `fileSystemInfo.lastModifiedDateTime`), and what are `st_atime` and `st_ctime`?
5. What do directories report for `st_blocks` and `st_size`?
