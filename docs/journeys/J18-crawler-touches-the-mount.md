# J18 A crawler touches the mount

**Goal:** indexers, thumbnailers and other tools that walk the whole tree do not download the
user's OneDrive; a program that starts downloading en masse is stopped and the user decides.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 (thumbnails in Files, step 1, are M3) |
| Related flows | F03 hydrate on read, F10 throttling and scheduler, F11 status for file managers, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active with about 14,000 files, 38 GB, nearly all online-only;
  `~/OneDrive/Documents/Taxes/` is pinned.
- Thumbnailers such as `ffmpegthumbnailer` and `gdk-pixbuf-thumbnailer` are installed.
- On an Omarchy 4.5.0 machine (still with Nautilus), `localsearch` starts with the session.
- The deny list and circuit breaker are at their defaults (ADR-0013): breaker at more than 50
  files or 1 GiB hydrated by one process within 60 seconds.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Opens `~/OneDrive/Pictures/2026` in the file manager | Thumbnails for the photos | The daemon has written thumbnails fetched from Graph into `~/.cache/thumbnails`. When a thumbnailer process reads a placeholder anyway, its `comm`/`exe` matches the deny list and the read fails with `EIO`; nothing downloads |
| 2 | Logs in; `localsearch` indexes the home directory, `~/OneDrive` included | Nothing changes; no downloads in `mydrive activity` | `localsearch` reads of non-local content fail with `EIO`. Local content (for example the pinned `Taxes` folder) is served to it normally |
| 3 | `du -sh ~/OneDrive`, `ncdu ~/OneDrive`, `find ~/OneDrive -size +100M` | Results at once | Metadata only; no reads, no downloads |
| 4 | `grep -r "invoice" ~/OneDrive/Documents` | grep runs, then prints "Input/output error" for further files. A desktop notification: grep is downloading many files, with "Allow once" and "Always allow" | grep is not on the deny list, so its reads hydrate. Once it has hydrated more than 50 files within 60 s, the breaker pauses hydration for that process; its next reads of non-local content fail with `EIO`; one notification is raised |
| 5 | Meanwhile opens `budget.xlsx` in LibreOffice | It opens normally | The breaker applies to grep only |
| 6 | Chooses "Allow once" | Runs `grep -r` again; it completes | grep (that process, until it exits) may hydrate. The new `grep` run is a new process, so the breaker may trip again; see open questions |
| 7 | Runs `restic backup ~` | The same notification for `restic` | Breaker trips on byte count (1 GiB) or file count |
| 8 | Chooses "Always allow" for `restic` | Later backups run without asking | `restic` added to the allow list in `config.toml` |

## Variations

- **V1 Crawler reads local content.** An indexer reading a hydrated file gets its content;
  nothing is downloaded.
- **V2 Pinned items.** Never refused to anyone (ADR-0013).
- **V3 Thumbnail not available from Graph.** The file manager shows a generic icon; still no
  download.
- **V4 Offline.** Seeded thumbnails already in the cache still show; others do not.
- **V5 Sandboxed or renamed crawler.** It escapes the deny list but is still counted by the
  breaker.
- **V6 Thumbnail after hydration.** A thumbnailer that got `EIO` may have written a failure
  marker, so the file manager may not retry the thumbnail after the file is later downloaded
  (ADR-0013 consequences).
- **V7 `cp -r` from the mount.** A legitimate bulk copy trips the breaker; the notification is
  the way to let it continue.

## Acceptance criteria

- **J18-AC1** Given an online-only file, when a process whose `comm` is `ffmpegthumbnailer`
  reads it, then the read fails with `EIO`, returns no data, the file's hydration stays
  `placeholder`, and the simulator receives no content request.
- **J18-AC2** Given a mount of online-only files, when a process whose `comm` is `localsearch`
  opens and reads every file, then the simulator receives zero content requests.
- **J18-AC3** Given a local file and a pinned online-only file, when a deny-listed process
  reads each, then it receives the local file's content, and the pinned file is hydrated and
  served rather than refused.
- **J18-AC4** Given the breaker defaults, when one process not on any list reads 60 different
  online-only files within 60 seconds, then the first 50 are served with correct content, a
  hydrating read of a later file fails with `EIO` before the 60th file, and exactly one
  desktop notification is raised naming the process.
- **J18-AC5** Given a process paused by the breaker, when another process reads an online-only
  file, then that read hydrates and returns the correct content.
- **J18-AC6** Given a paused process and the user chooses "Allow once", when that process
  continues reading, then its reads hydrate and return correct content until it exits; a new
  process with the same name is counted afresh.
- **J18-AC7** Given the user chooses "Always allow" for a process, when `config.toml` is read,
  then the allow list contains it, and a new process with that name reading 60 files within
  60 seconds is not paused.
- **J18-AC8** Given an online-only image whose thumbnail the simulator serves, when the
  daemon seeds thumbnails, then a thumbnail for it exists at the path and keys the freedesktop
  cache expects, and the simulator received a thumbnail request but no content request.
- **J18-AC9** Given a mount of online-only files, when `du -s`, `find` and `ls -R` run over
  it, then the breaker does not trip and no content is downloaded.

## Must never happen

- A crawler, indexer or thumbnailer downloads the drive and fills the disk.
- A crawler's access changes a file's modification time or causes an upload.
- A refused or paused process receives zeros or fabricated content instead of an error.
- The user's own editor or terminal is refused because a crawler tripped the breaker.
- Pinned content is refused.

## Open questions

1. "Allow once" lasts for the life of one process, but `grep -r` run again is a new process.
   Should the choice cover a short period or the same command in the same terminal?
2. Should the breaker take the process group or session into account, so that a pipeline or
   `xargs` fan-out counts as one?
3. Should a breaker notification also be answerable from the CLI (no command is listed in the
   overview) for headless machines (J14)?
4. Should the mount also ship indexer markers (`.trackerignore`, `.nomedia`) as a second line
   of defence?
5. How does a user opt in to indexing their OneDrive on purpose?
6. Deny-list workability and the thumbnail cache layout are tracked as open-questions Q20 and
   Q19.
