# Flows

How my-drive behaves, step by step, for each mechanism. Journeys (`docs/journeys/`) say what the
user does and sees; flows say what the components do. Both must agree with
`docs/architecture/overview.md`, which wins any disagreement, and with the ADRs in `docs/adr/`.

| ID | Title | Purpose | Journeys |
|---|---|---|---|
| [F01](F01-sign-in.md) | Sign-in | Browser and loopback sign-in with PKCE, headless paste-URL variant, token refresh, sign-out; sign-in returns the account and never creates a mount | J01, J14, J15 |
| [F02](F02-add-mount-initial-sync.md) | Add a mount and initial sync | First delta run, staged then applied to the store; no files for folders or placeholders; delta token; FUSE mount (M2) | J01, J02, J11, J16 |
| [F03](F03-hydrate-on-read.md) | Hydrate on read | Ranged fetch with read-ahead, range or whole-file responses, range tracking, `EIO` instead of zeros, crawler deny list and breaker | J02, J03, J09, J17, J18 |
| [F04](F04-local-write-and-upload.md) | Local write and upload | Durable dirty mark, quiet period, session upload for existing files, two hashes, save-via-rename, create and rename, `item.import` in M1 | J04, J05, J09, J10, J12 |
| [F05](F05-remote-change.md) | A change arrives from the server | Wake-up (M1 polls), staged delta run, metadata and content changes, remote deletes, `410 Gone` resync | J08, J09, J10, J16 |
| [F06](F06-pin-and-dehydrate.md) | Pin, unpin and free up space | Pinning files and folders with inheritance, unpin, dehydrate, refusals | J06, J07, J09, J17 |
| [F07](F07-conflict.md) | Conflict | Both-sides-changed detection on `412` and on delta; the server version keeps the name, the local one becomes a copy, in one store transaction | J04, J09, J10 |
| [F08](F08-offline-and-recovery.md) | Offline, reconnect, crash recovery | Fail-fast reads and kept writes offline, drain order on reconnect, recovery scan after a crash | J09, J15, J16 |
| [F09](F09-add-sharepoint-library.md) | Add a SharePoint library | Link resolution, preview without redeeming, confirm, mount; `blocked` and read-only outcomes | J11, J12, J13 |
| [F10](F10-throttling-and-scheduler.md) | Request scheduler and throttling | One budget per tenant across mounts, priority classes, `429` with `Retry-After` | J03, J08, J09, J11, J18 |
| [F11](F11-status-for-file-managers.md) | Status for file managers | `hello`, batch `item.status`, `status.subscribe` change stream; Files plugin via the CLI | J02, J06, J07, J17 |
| [F12](F12-delete-safety.md) | Delete safety | Local delete to recycle bin with `If-Match` and mass-delete guard (M2); mount removal never deletes and writes local files into the mount point | J05, J15, J16 |
| [F13](F13-state-machines.md) | State machines | Hydration and sync transitions with their events; account and mount states; display status | J02, J03, J04, J06, J07, J08, J10, J12, J13, J17 |

## Structure of each flow

Every flow has: **Purpose**; **Preconditions**; one or more **Mermaid diagrams**; **Steps**
(numbered notes keyed to the diagram's `autonumber` step numbers, explaining what the diagram
cannot show); **Failure paths** (what the user sees, and the resulting state); **Invariants**
(what must never happen); **Open questions** (existing IDs from
`docs/architecture/open-questions.md` first, then new ones); **Related** (journeys, ADRs with
rule numbers, validation gates, other flows).

## Diagram conventions

- `sequenceDiagram` for interactions between components, `flowchart TD` for decision logic,
  `stateDiagram-v2` for state. All diagrams use `autonumber` where steps are referenced.
- Participant names are short and the same in every file:

| Name | Is |
|---|---|
| `User` | The person (an `actor`) |
| `App` | Any application making POSIX calls on the mount: editor, shell, file manager |
| `VFS` | VFS layer (FUSE) |
| `Engine` | Sync engine (one worker per mount); `EngA`, `EngB` when two mounts appear |
| `Store` | State store (SQLite) |
| `Backing` | Backing directory |
| `Sched` | Request scheduler (one per tenant) |
| `GraphC` | Graph client |
| `Graph` | Microsoft Graph |
| `Auth` | Auth and token store |
| `Keyring` | Secret Service |
| `Entra` | Microsoft identity platform (`login.microsoftonline.com`), only in F01 |
| `Browser` | The user's system browser, only in F01 |
| `IPC` | IPC server. It reaches the store, the token store and Graph only through `Engine` (ADR-0023) |
| `CLI` | `mydrive` |
| `Adapter` | A file-manager adapter (socket client); `Host` is its file manager |
| `Files`, `Plugin` | Omarchy's Files and the my-drive Files plugin script, only in F11 |

- In message text, `D` is a `drive_id`, `I` an `item_id`, `P` a parent `item_id`; upper-case
  words (`PATH`, `NAME`, `HOST`) are placeholders. Graph paths are shown without `{}`.
- State values are written with their Rust names in prose and diagrams (`PendingUpload`). On the
  wire (IPC, xattrs, CLI `--json`, store columns) they are lowercase kebab-case (`pending-upload`),
  per the overview's "Wire forms". Account and mount states are already lowercase (`signed-in`,
  `needs-sign-in`, `active`, `paused`, `blocked`, `resyncing`).
- "Rule" citations such as ADR-0012 R3 refer to the "Rules for implementers" list in that ADR.
- To keep diagrams rendering on GitHub, labels avoid semicolons, `#`, and nested quotes; flowchart
  labels with punctuation are quoted.
- Where behaviour depends on an unverified fact, the step names the gate (G1 to G5) or the
  open-questions entry.

## Defaults introduced by the flows

These numbers appear in flows but not in the overview or ADRs. They are proposals, to be confirmed
or replaced by the owning ADR:

| Value | Default | Flow |
|---|---|---|
| Sign-in attempt timeout | 10 minutes | F01 |
| Read chunk alignment; whole-file fetch for small files; read-ahead window | 1 MiB; 4 MiB or less; 1 to 16 MiB | F03 |
| Wake-up coalescing | 1 s | F05 |
| Poll (M1's wake-up source, and the fallback once push notifications exist); safety-net poll while push notifications are up | 5 minutes; 30 minutes | F05 |
| Offline probe backoff | 5 s up to 5 minutes | F08 |
| Blocked-mount re-probe | hourly | F09 |
| Requests in flight per tenant; of which background | 8; 2 | F10 |
| Delete batch window for the guard | 2 s without a new delete | F12 |
