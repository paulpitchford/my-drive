# J03 Open an online-only file

**Goal:** the user opens an online-only file in any application and it just works: the
content downloads on read, large files stream by range, and if the content cannot be fetched
the application gets an error within 30 seconds and the user is told why.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 (notifications in V1 to V3 arrive with M3) |
| Related flows | F03 hydrate on read, F08 offline and recovery, F10 throttling and scheduler, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Documents/budget.xlsx`, 48,213 bytes, online-only.
- `~/OneDrive/Videos/holiday-2026.mp4`, 2,465,792,000 bytes (2.3 GiB), online-only.
- Gate G1 passed (`Range` works on authenticated `/content`), unless a step says otherwise.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `xdg-open ~/OneDrive/Documents/budget.xlsx` | LibreOffice Calc opens the spreadsheet after a short pause | `open` returns at once without downloading. LibreOffice's first `read` reaches the VFS; the engine fetches the content through the tenant's request scheduler, writes it to the backing file, records the extents, verifies the quickXorHash, sets hydration `Full`, then answers the read |
| 2 | `du -h ~/OneDrive/Documents/budget.xlsx` and `ls -l` | `du` shows about `48K`; `ls -l` still shows `48213` | `st_blocks` now covers the file |
| 3 | `mydrive status ~/OneDrive/Documents/budget.xlsx` | `local` | Hydration `Full`, pin `Inherit`, sync `InSync` |
| 4 | `mpv ~/OneDrive/Videos/holiday-2026.mp4` | Playback starts within a few seconds | Each read fetches the requested range plus read-ahead; hydration `Partial` |
| 5 | Seeks to 40:00 | Playback resumes there after a short pause | The range around the new position is fetched; earlier ranges stay local |
| 6 | `du -h ~/OneDrive/Videos/holiday-2026.mp4` | A few hundred MiB, much less than 2.3G | Only fetched extents are allocated |
| 7 | `mydrive status ~/OneDrive/Videos/holiday-2026.mp4` | `online-only` | `Partial` displays as `online-only` (F13) |
| 8 | `mydrive hydrate ~/OneDrive/Videos/holiday-2026.mp4` | The command returns; `mydrive activity` shows the download in progress, then done; status becomes `local` | Sync `PendingDownload` until the remaining ranges are fetched and the whole-file hash verified; pin stays `Inherit` |
| 9 | Plays the video again | Plays from local data | No Graph request: the extents are recorded as local (ADR-0005 R5) |

## Variations

- **V1 Offline.** Sam opens `~/OneDrive/Documents/taxes-2025.pdf`, online-only, with no
  network. The PDF viewer reports "Input/output error" within 30 seconds. `mydrive status
  <path>` says the content could not be fetched because the network is unreachable. With M3,
  a desktop notification says the same (J09).
- **V2 Signed out.** The account is in `needs-sign-in`. Same as V1, with the reason "sign-in
  required" and a pointer to `mydrive login`.
- **V3 Blocked by policy.** The mount is `blocked`. Same as V1, with the reason "blocked by
  your organisation's policy" (J13).
- **V4 G1 failed.** `Range` is not honoured. The first read of `holiday-2026.mp4` fetches the
  whole file before answering; mpv waits until then. `du` afterwards shows the full size.
- **V5 Content check fails.** The fetched bytes do not match the server's quickXorHash. The
  read fails with `EIO`, the bad extents are dropped, the item shows `error`, and
  `mydrive activity` records a hash mismatch.
- **V6 Download refused for one file.** The server answers 403 for that file only. The read
  fails with `EIO`; the item shows `error` with the reason; other files are unaffected.
- **V7 Two programs read the same file at once.** Both get the same correct bytes; each range
  is fetched once.
- **V8 A crawler reads the file.** A thumbnailer or indexer is refused (J18).
- **V9 Throttled.** Graph answers 429 with `Retry-After`. The scheduler waits as told (F10);
  what the reading application sees if the wait exceeds 30 seconds is an open question.

## Acceptance criteria

- **J03-AC1** Given an online-only file, when a process reads it from start to end, then the
  bytes equal the simulator's content, and afterwards `mydrive status <path> --json` reports
  hydration `full`, sync `in-sync`, display status `local`, and `st_blocks * 512 >= st_size`.
- **J03-AC2** Given G1 passed and a 2,465,792,000-byte online-only file, when a process reads
  its first 1 MiB only, then the bytes are correct, hydration is `partial`, display status is
  `online-only`, `st_blocks * 512` is less than `st_size`, and every content request the
  simulator received was a ranged request.
- **J03-AC3** Given a range that was hydrated earlier, when it is read again, then the
  simulator receives no request.
- **J03-AC4** Given the simulator is unreachable, when a process reads an online-only file,
  then the read fails with `EIO` no later than 30 seconds after the call, and no data bytes
  are returned.
- **J03-AC5** Given the read in J03-AC4 has failed, when `mydrive status <path> --json` and
  `mydrive activity --json` run, then both report the failure with a reason identifying the
  network as unreachable.
- **J03-AC6** Given the account's refresh token has been revoked in the simulator, when a
  process reads an online-only file, then the read fails with `EIO` within 30 seconds and
  `mydrive status --json` shows the account as needing sign-in.
- **J03-AC7** Given the simulator returns content whose quickXorHash differs from the item's
  metadata, when a process reads the file to the end, then a read fails with `EIO`, the item's
  hydration is not `full`, its display status is `error`, and no later read returns the
  mismatched bytes.
- **J03-AC8** Given two processes read the same online-only file concurrently, when both
  finish, then both received the simulator's content and no byte range was requested from the
  simulator more than once.
- **J03-AC9** Given an online-only file with pin `Inherit`, when `mydrive hydrate <path>`
  completes, then `--json` status reports hydration `full`, pin still `inherit`, and display status is `local`.
- **J03-AC10** Given an online-only file, when it is read in full, then its `st_mtime` and
  server eTag are unchanged and the simulator received no upload for it.

## Must never happen

- A read returns zeros, or any bytes that did not come from the server, in place of missing
  content.
- A read of content that cannot be fetched waits longer than 30 seconds without progress.
- A file is marked `Full` without its whole-file hash matching the server.
- Reading a file changes its modification time or causes an upload.
- An application gets a bare `EIO` with no reason available anywhere for the user.

## Open questions

1. When Graph throttles with a `Retry-After` longer than 30 seconds, does the read fail with
   `EIO` at 30 seconds (no progress) or wait for the scheduler?
2. Is there an upper limit for a download that is slow but still making progress?
3. With G1 failed, a first read of a 2.3 GiB file waits for the whole download. Is that
   acceptable, and is the 30-second rule measured from the last progress in that case too?
4. Read-ahead size, and whether it adapts to sequential reads.
5. What happens when the local disk fills during hydration (`EIO` or `ENOSPC` to the reader)?
6. Does a read fail at once when the daemon already knows it is offline, or always wait up
   to the 30-second bound?
7. How often may the "could not open" notification repeat when an application retries the
   same file?
8. Does the item stay `error` after V1/V2/V3, or does only the activity log record the failed
   read? F13 keeps sync `InSync` during interactive reads.
