# J09 Offline and reconnect

**Goal:** with no network, the user keeps working on everything that is local, gets a clear
error within 30 seconds for anything that is not, and on reconnect every change made offline
uploads without any action.

| | |
|---|---|
| Persona | Priya |
| Milestone | M2 (tray and notifications in steps 1 and 5 arrive with M3) |
| Related flows | F08 offline and recovery, F04 local write and upload, F03 hydrate on read, F10 throttling and scheduler |

## Preconditions

- Priya is signed in as `priya@contoso.com`; mounts `~/OneDrive - Contoso` and
  `~/Contoso/Finance - Documents` are active.
- `~/OneDrive - Contoso/Q4 Plan/` is pinned and fully downloaded; it holds `roadmap.docx`.
- `~/Contoso/Finance - Documents/forecast-2027.xlsx` is local, unpinned, in sync.
- `~/Contoso/Finance - Documents/Archive/2019-ledger.xlsx` is online-only.
- She boards a flight; the laptop has no network.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Turns Wi-Fi off; runs `mydrive status` | The account is shown as offline. With M3, the tray icon shows the same | Graph requests fail to connect; the account is marked offline. Item states do not change (F13) |
| 2 | `ls -l ~/Contoso/Finance\ -\ Documents/Archive` | The usual listing | Metadata from the store |
| 3 | Opens `Q4 Plan/roadmap.docx`, edits, saves | Opens and saves normally | Local write; sync `PendingUpload`; the upload cannot start |
| 4 | Creates `~/OneDrive - Contoso/Q4 Plan/notes-flight.md` | Saved | New item, `PendingUpload` |
| 5 | Opens `Archive/2019-ledger.xlsx` | Within 30 seconds LibreOffice reports a general input/output error. With M3, a notification says the file is not on this device and the network is unreachable | The read cannot be fetched; it fails with `EIO` once no progress has been made for 30 s. Reason recorded in status and activity |
| 6 | `mydrive activity` | Two changes waiting to upload, and the failed open with its reason | |
| 7 | Lands; Wi-Fi reconnects | After a short time `mydrive status` shows the account online; the two files go from `syncing` to `pinned` | Engine detects connectivity, uploads both pending items through the scheduler, then calls delta on each mount to catch up |
| 8 | `mydrive activity` | Both uploads done; remote changes made by colleagues during the flight are reflected in the mounts | |

## Variations

- **V1 Someone else edited `roadmap.docx` during the flight.** On reconnect the upload gets
  412: both versions are kept (J10).
- **V2 Captive portal.** Hotel Wi-Fi connects but returns its own login page for every
  request. The client treats Graph as unreachable; no portal HTML ever becomes file content.
- **V3 Flaky network.** Requests fail and succeed in turn. Uploads retry; partial session
  uploads resume where possible.
- **V4 Append to an online-only file offline.** `echo >> Archive/notes.txt` needs the whole
  file first, so the write fails with `EIO` (J04).
- **V5 Free up space offline.** Allowed for in-sync files (J07).
- **V6 Reboot while offline.** The pending changes survive and upload on reconnect (J16).
- **V7 Sign-in expires while offline.** The account shows `needs-sign-in` once a refresh is
  rejected after reconnecting; pending changes stay queued until Priya signs in again.

## Acceptance criteria

- **J09-AC1** Given the simulator is unreachable, when any folder in any mount is listed and
  stat-ed, then the entries, sizes and times equal those returned while online.
- **J09-AC2** Given the simulator is unreachable, when a local (`full`) file is read, then its
  content is returned without error.
- **J09-AC3** Given the simulator is unreachable, when an online-only file is read, then the
  read fails with `EIO` no later than 30 seconds after the call, and no data bytes are
  returned.
- **J09-AC4** Given a read failed as in J09-AC3, when `mydrive status --json` runs, then it
  reports that the network is unreachable, and `mydrive activity --json` lists
  the failed read with that reason.
- **J09-AC5** Given the simulator is unreachable, when a file is created or saved in a mount,
  then the save succeeds, the content is readable at once, and the item's sync is
  `pending-upload`.
- **J09-AC6** Given items are `pending-upload` while offline, when the simulator becomes
  reachable again, then every one uploads without user action and its content on the
  simulator equals the local content.
- **J09-AC7** Given the simulator made changes while the client was offline, when the client
  reconnects, then those changes appear in the mount after the catch-up delta.
- **J09-AC8** Given a file saved twice while offline, when the client reconnects and the
  uploads finish, then the simulator's content equals the second save.
- **J09-AC9** Given a proxy that answers every request with an HTML login page and status
  200, when an online-only file is read, then the read fails with `EIO` within 30 seconds and
  the file's hydration stays `placeholder`.

## Must never happen

- A read offline returns zeros, an empty file, or bytes that did not come from Graph.
- A read waits longer than 30 seconds without progress.
- A change saved offline is dropped, or uploaded in an older form than the last save.
- A captive portal's page is stored as file content.
- Going offline changes item states or deletes anything locally.

## Open questions

1. How does the daemon learn the network is back: NetworkManager signals, periodic probes, or
   both? Is there a target time from reconnect to the first upload?
2. Is there a defined account-level word for "offline" in `mydrive status --json`? The
   overview lists only `signed-in` and `needs-sign-in` for accounts.
3. Should metered connections (phone tethering) defer uploads or pin downloads?
4. Should a notification fire once when the client goes offline, or only when a read fails?
5. Is a limit placed on how many changes can wait offline, or for how long?
