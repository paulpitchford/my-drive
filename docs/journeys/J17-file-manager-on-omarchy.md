# J17 File manager on Omarchy

**Goal:** in Omarchy's file manager, Files (`folio`), the user sees each mount's status in the
sidebar, a file's status in the details pane, and can pin, free, copy a link or view online
from the right-click menu, without anything downloading just because a folder was opened.

| | |
|---|---|
| Persona | Sam |
| Milestone | M3 |
| Related flows | F11 status for file managers, F06 pin and dehydrate, F03 hydrate on read, F13 state machines |

## Preconditions

- Omarchy with Files (`folio`) as the default file manager (the Omarchy release after 4.5.0
  that drops Nautilus).
- `mydrive` and `mydrive-folio` (the Files plugin) are installed (ADR-0016).
- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Pictures/2026/` holds 1,240 online-only JPEGs; `~/OneDrive/Documents/Taxes/` is
  pinned.

### What Files can and cannot show

Files plugin interface v1 offers right-click actions limited to paths under given folders, a
details-pane row for one selected file, and a status word and icon on a sidebar row (polled,
at most every 5 seconds). **It has no per-file icon on tiles or list rows.** So on Omarchy a
file's status is visible only in the details pane and through `mydrive status`; there is no
cloud or tick drawn on the file itself. Per-file icons need an upstream change to Files
(gate G4).

| Display status | Files details row (M3) | Nautilus/Nemo/Caja emblem, other distros (M5) | Windows icon |
|---|---|---|---|
| `online-only` | `online-only` | Cloud outline | Blue cloud |
| `local` | `local` | Tick outline | Green tick outline |
| `pinned` | `pinned` | Solid tick | Solid green tick |
| `syncing` | `syncing` | Sync arrows | Sync arrows |
| `error` | `error` and the reason | Red cross | Red cross |
| `blocked` | `blocked` and the reason | Emblem not yet chosen | — |
| modifier `read-only` | `read-only` added | Lock | Lock |

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Opens Files | A sidebar row "OneDrive" with a status word and icon | The plugin's `places`/`poll` command asks the daemon for the mount's state. Files polls it at most every 5 s |
| 2 | Opens `~/OneDrive/Pictures/2026` | Tiles with thumbnails for the photos, but no per-file status icon | Files lists and stats the folder (no download). Thumbnails come from the freedesktop cache, which the daemon seeds from Graph's thumbnails (ADR-0013). Any thumbnailer Files runs on a placeholder is refused (J18) |
| 3 | Selects `IMG_2001.jpg` and opens the details pane | A row showing `online-only` | The plugin's `details` command runs `mydrive status --json` (or asks over IPC) for that one path |
| 4 | Right-clicks `Pictures/2026` → "Always Keep on This Device" | The sidebar row shows syncing; after the downloads the details row of a photo shows `pinned` | The action runs `mydrive pin` on the folder (J06) |
| 5 | Right-clicks `Videos/2025` → "Free Up Space" | The details row of a video shows `online-only` | The action runs `mydrive free` (J07) |
| 6 | Right-clicks `Documents/notes.md`, just saved → "Free Up Space" | A desktop notification: not freed, changes not uploaded yet | `mydrive free` refused it; the plugin shows the refusal through `org.freedesktop.Notifications` |
| 7 | Right-clicks `Documents/budget.xlsx` → "Copy Link" | The clipboard holds the file's OneDrive web address | Copies the item's existing `webUrl`; no sharing link is created and no permission changes |
| 8 | Right-clicks it → "View Online" | The browser opens the file on onedrive.com | Opens the `webUrl` |
| 9 | Double-clicks `budget.xlsx` | LibreOffice opens it after a short download | Hydration on read (J03); nothing plugin-specific |
| 10 | Right-clicks a file in `~/Downloads` | No my-drive entries | Actions are limited to paths under the mount points |

## Variations

- **V1 Space preview on a placeholder.** See open questions.
- **V2 Daemon not running.** The sidebar row shows that my-drive is not running; the details
  row says the same rather than showing an old status. Actions report the error.
- **V3 Library mounts.** Each mount has its own sidebar row; a blocked
  library's row shows `blocked`; a read-only library's details rows add `read-only`.
- **V4 Nautilus, Nemo or Caja on another distro (M5).** The Python adapter draws an emblem on
  every file per the table above and offers the same menu actions.
- **V5 Tray and notifications.** The tray icon shows overall state with a flat menu; errors,
  conflicts and held deletions raise notifications.
- **V6 Mount added or removed.** The plugin's actions and sidebar row follow the new set of
  mounts (open-questions Q16).

## Acceptance criteria

These run the plugin's commands directly, with the same arguments Files would pass, and read
their JSON output.

- **J17-AC1** Given the plugin manifest, when it is read, then it declares `actions`,
  `details` and `poll`/`places` hooks, and every action is restricted to paths under the
  current mount points.
- **J17-AC2** Given files with each display status and with the `read-only` modifier, when
  the plugin's `details` command runs for each, then the status it prints equals the display
  status and modifiers that `mydrive status <path> --json` reports.
- **J17-AC3** Given an online-only folder, when the "Always Keep on This Device" action runs on
  it, then the folder's pin becomes `pinned`, exactly as after `mydrive pin`.
- **J17-AC4** Given a file with a pending upload, when the "Free Up Space" action runs on it,
  then the file is unchanged and one desktop notification stating the reason is sent on the
  session bus.
- **J17-AC5** Given an item, when the "Copy Link" action runs, then the clipboard receives the
  item's `webUrl` and the simulator receives no request to create a link or change
  permissions.
- **J17-AC6** Given the plugin's `poll` and `details` commands run against a mount, when the
  simulator's request log is inspected, then they caused no Graph request and no content
  download.
- **J17-AC7** Given a folder of 1,240 online-only JPEGs whose thumbnails the simulator serves,
  when Files (or a script calling the installed thumbnailers the way Files does) displays the
  folder, then the simulator received zero content downloads and a seeded thumbnail exists in
  `~/.cache/thumbnails` for each photo shown.
- **J17-AC8** Given the daemon is stopped, when the plugin's `details` and `poll` commands
  run, then each returns within the plugin's bounded timeout and reports that my-drive is not
  running.

## Must never happen

- The journey, the docs or the plugin promise or imply a per-file icon in Files that Files
  cannot draw.
- Opening a folder in Files downloads its files.
- A plugin command reads file content, or causes hydration.
- A stale status is shown as current when the daemon is unreachable.
- "Copy Link" creates a sharing link or changes who can see the file.

## Open questions

1. Is Space preview of a placeholder a user action that should download (as opening does), or
   is the previewer refused like a thumbnailer? It depends on which process Files uses.
2. Does Files generate thumbnails in its own process? If so, its reads cannot be told apart
   from a user opening a file, and the deny list cannot cover them.
3. What words and icons does the sidebar row show for each mount state (`active`, `paused`,
   `blocked`, `resyncing`) and for account `needs-sign-in`?
4. What does the details pane show beyond the status: size on disk, pin, last sync, reason?
5. Should Files show a folder's aggregate status? Folders currently derive `local` (F13 open
   question 1).
6. Per-file hooks and a thumbnail hook in Files are tracked as open-questions Q17 (gate G4).
