# J13 Library blocked by organisation policy

**Goal:** when the organisation's policy cuts off one library from this device, the user is
told plainly that it is a policy block, that library stops without losing anything, and
every other mount carries on.

| | |
|---|---|
| Persona | Priya |
| Milestone | M2 (notification in step 4 arrives with M3) |
| Related flows | F09 add SharePoint library, F08 offline and recovery, F10 throttling and scheduler, F11 status for file managers, F13 state machines |

## Preconditions

- Priya has four mounts: `~/OneDrive - Contoso`, `~/Contoso/Finance - Documents`,
  `~/Contoso/Project Atlas - Documents`, `~/Contoso/Policies - Documents`. All are active.
- In `~/Contoso/Project Atlas - Documents`: `specs/atlas-v2.pdf` is local;
  `diagrams/arch.drawio` is online-only.
- Contoso applies a sensitivity label to the Project Atlas site that blocks access from
  unmanaged devices. Priya's laptop is not Intune-compliant.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Keeps working; the policy takes effect (it can take up to 24 hours) | — | The next Graph request for the Project Atlas drive gets a policy denial. The mount state becomes `blocked` with the reason. Its delta and uploads stop. Item sync values are left as they are (F13) |
| 2 | `mydrive status` | Project Atlas: `blocked`, "your organisation's policy blocks access to this library from this device". The other three mounts: `active` | |
| 3 | `mydrive status ~/Contoso/Project\ Atlas\ -\ Documents/diagrams/arch.drawio` | `blocked` | Mount state `blocked` overrides display for every item in it |
| 4 | Opens `arch.drawio` | An input/output error within 30 seconds. With M3, a notification naming the policy block | Content cannot be fetched; `EIO` after 30 s without progress |
| 5 | `ls ~/Contoso/Project\ Atlas\ -\ Documents/diagrams` | The last known listing | Metadata from the store; nothing removed |
| 6 | Edits and saves `~/Contoso/Finance - Documents/forecast-2027.xlsx` | Saves and uploads as usual | The tenant's scheduler keeps serving the other mounts |
| 7 | `mydrive doctor` | A policy block reported for Project Atlas, with the likely cause (device not managed by the organisation) and the advice to contact IT | |
| 8 | IT exempts the site; later the block is lifted | `mydrive status` shows Project Atlas `active` again; items show their normal statuses | The daemon retries the drive, succeeds, runs delta to catch up, then sends any changes that were pending |

## Variations

- **V1 Blocked when added.** Priya tries to add a library that is already blocked (J11). See
  open questions.
- **V2 Whole tenant refuses the app.** A consent refusal (for example AADSTS65005) applies to
  every mount of the account: all Contoso mounts show `blocked`. Her personal account, if
  signed in too, is not affected.
- **V3 One file refused.** The server refuses a single file (403) while the library is
  otherwise reachable. Only that file shows `error`; the mount is not `blocked`.
- **V4 Offline at the same time.** Offline and blocked are reported separately; offline does
  not hide a known block, and a block is never reported as offline.
- **V5 Local copies.** What Priya can do with `atlas-v2.pdf`, which is local, while blocked is
  an open question.

## Acceptance criteria

- **J13-AC1** Given the simulator answers every request for one library's drive with a policy
  denial, when the daemon next contacts that drive, then `mydrive mount list --json` shows
  that mount `blocked` with a reason identifying organisation policy, every item in it reports
  display status `blocked`, and the other mounts are `active`.
- **J13-AC2** Given a blocked mount, when an online-only file in it is read, then the read
  fails with `EIO` within 30 seconds, and `mydrive status <path> --json` gives a reason that
  is distinct from the reasons for offline and for `needs-sign-in`.
- **J13-AC3** Given one blocked mount in a tenant, when a file is saved in another mount of
  the same tenant, then it uploads and reaches `in-sync` as it would with no mount blocked.
- **J13-AC4** Given a blocked mount, when `mydrive doctor --json` runs, then it reports a
  policy block naming that mount.
- **J13-AC5** Given a blocked mount, when the simulator and the local mount are inspected,
  then no item was deleted on the server, no item was removed from the local listing, and
  every local file's `st_blocks` is unchanged.
- **J13-AC6** Given a blocked mount with an item whose upload was pending, when the simulator
  lifts the block and the daemon retries, then the mount becomes `active`, the pending upload
  completes, and no unchanged file is uploaded or downloaded again.
- **J13-AC7** Given a library reachable except for one file refused with 403, when that file
  is read, then that file shows `error` and the mount is not `blocked`.
- **J13-AC8** Given a tenant-wide consent refusal for an account, when the daemon contacts
  Graph, then every mount of that account is `blocked` and mounts of other accounts stay
  `active`.

## Must never happen

- A policy block is treated as remote deletion, so the local tree is emptied or the server
  is told to delete anything.
- One blocked library stalls, slows or blocks other mounts.
- A block is reported as "offline" or as a generic error with no cause.
- A pending local change is discarded when the mount is blocked or unblocked.

## Open questions

1. While a mount is `blocked`, are its local copies (such as `atlas-v2.pdf`) still readable?
   The overview says local content stays readable for `needs-sign-in` but does not say so for
   `blocked`, and the organisation may expect the opposite.
2. Are writes in a blocked mount accepted and queued, or refused at once (for example with
   `EACCES` or `EROFS`)?
3. How often does the daemon retry a blocked drive, and does that retry count against the
   tenant budget in a way that matters?
4. Should a library that is already blocked at add time be refused, or added in the
   `blocked` state?
5. How does the client tell a policy block from a revoked permission or a deleted library?
   Depends on gate G2 (open-questions Q13).
6. "Limited access" policy (view in browser, no download) and "block access" policy: treated
   the same?
7. When is Intune broker sign-in planned (ADR-0010 says later)?
