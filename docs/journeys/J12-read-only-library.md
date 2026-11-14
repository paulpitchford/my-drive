# J12 Read-only library

**Goal:** in a library the user can read but not change, every file opens normally, every
attempt to change something fails at once with a read-only error, and nothing is accepted
locally only to be stranded later.

| | |
|---|---|
| Persona | Priya |
| Milestone | M2 (the details-pane row and emblem in step 8 arrive with M3 and M5) |
| Related flows | F09 add SharePoint library, F04 local write and upload, F11 status for file managers, F13 state machines |

## Preconditions

- `~/Contoso/Policies - Documents` is mounted (J11). The library requires check-out, so the
  daemon has detected it as read-only.
- `travel-policy-2026.pdf` (1,204,331 bytes) and `expenses-guide.docx` are online-only.
- The machine is online.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mydrive mount list` | `~/Contoso/Policies - Documents` marked read-only | Mount carries the read-only condition |
| 2 | `mydrive status ~/Contoso/Policies\ -\ Documents/travel-policy-2026.pdf` | `online-only`, `read-only` | Display status plus modifier |
| 3 | Opens `travel-policy-2026.pdf` | It opens | Hydration on read, as anywhere else |
| 4 | Opens `expenses-guide.docx` in LibreOffice and tries to save a change | LibreOffice reports the file is read-only (it may open it read-only from the start, offering "Edit copy") | The open for writing fails with `EROFS` |
| 5 | `cp ~/Downloads/draft.docx ~/Contoso/Policies\ -\ Documents/` | `cp: cannot create regular file ...: Read-only file system`, at once | `create` refused with `EROFS`; no request to Graph |
| 6 | `rm`, `mv`, `mkdir`, `touch` inside the mount | The same error each time | `unlink`, `rename`, `mkdir` and content-changing `setattr` refused with `EROFS` |
| 7 | `mydrive pin ~/Contoso/Policies\ -\ Documents/Handbook` | Pins and downloads as usual (J06) | Pin and free change only local state, so they are allowed |
| 8 | Selects a file in a file manager | Files (M3): the details row shows `online-only` and `read-only`. Nautilus on other distros (M5): a lock emblem | Adapters read the modifier from `item.status` |
| 9 | Saves an edited copy to `~/Documents/expenses-guide-notes.docx` instead | Saved | Outside the mount |

## Variations

- **V1 Library becomes writable.** The library owner removes required check-out. After the
  daemon re-detects it, the read-only modifier disappears and writes succeed.
- **V2 Library becomes read-only with changes pending.** Priya saved a file in a library just
  before it turned read-only. Her change is kept locally and shows `error` with the reason;
  it is not lost.
- **V3 Read-only because of permission.** Priya has only Read permission on the library;
  same behaviour.
- **V4 Remote changes.** Colleagues with edit rights change files; the changes arrive as
  usual (J08).
- **V5 Free up space.** Allowed (J07).

## Acceptance criteria

- **J12-AC1** Given a library the simulator reports as requiring check-out, when it is
  mounted, then `mydrive mount list --json` marks the mount read-only and `mydrive status
  <file> --json` reports the `read-only` modifier for its items.
- **J12-AC2** Given a read-only mount, when a process opens an existing file with `O_WRONLY`
  or `O_RDWR`, then the call fails with `EROFS` and the simulator receives no request caused
  by it.
- **J12-AC3** Given a read-only mount, when a process calls `creat`, `mkdir`, `rename`,
  `unlink`, `rmdir`, `truncate` or `utimensat` on a path in it, then each fails with `EROFS`
  and the simulator receives no request caused by them.
- **J12-AC4** Given a read-only mount, when `access(path, W_OK)` is called on a file, then it
  fails.
- **J12-AC5** Given an online-only file in a read-only mount, when a process reads it, then
  the bytes equal the simulator's content.
- **J12-AC6** Given a read-only mount, when `mydrive pin`, `mydrive free` and `mydrive hydrate`
  run on items in it, then each succeeds and the simulator receives no write request.
- **J12-AC7** Given a read-only mount, when the simulator makes the library writable and the
  daemon has re-detected it, then the modifier is gone and a file created in the mount
  uploads.
- **J12-AC8** Given a file with a pending upload in a library, when the simulator makes the
  library read-only before the upload, then the local content is unchanged, the item shows
  `error` with a reason, and its pending change still exists after a daemon restart.

## Must never happen

- A write is accepted locally and then silently never uploaded (the Windows behaviour this
  design replaces).
- Anything is uploaded to, renamed in or deleted from a read-only library.
- A change saved before the library turned read-only is lost.

## Open questions

1. When and how often is read-only status re-detected? (Detection itself is open-questions
   Q12.)
2. Can single items or folders be read-only inside a writable library, and does the VFS
   enforce that per item?
3. Are mode bits shown without `w` in a read-only mount, so that tools like LibreOffice can
   tell before trying to write?
4. In V2, what can Priya do with her stranded change: is there a way to move it out, or is
   copying it elsewhere by hand the answer?
5. Should a file checked out by someone else be shown differently?
