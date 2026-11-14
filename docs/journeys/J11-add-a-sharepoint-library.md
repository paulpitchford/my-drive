# J11 Add a SharePoint library

**Goal:** with the sign-in she already has, the user adds the SharePoint libraries she works in,
each as its own mount under `~/Contoso/`, by pasting a link, browsing Teams or entering a
drive ID.

| | |
|---|---|
| Persona | Priya |
| Milestone | M4 (the library picker with Teams browsing). Adding by link or drive ID from the CLI works from M2. |
| Related flows | F09 add SharePoint library, F02 add mount and initial sync, F01 sign-in, F10 throttling and scheduler |

## Preconditions

- Priya is signed in as `priya@contoso.com` (tenant Contoso); `~/OneDrive - Contoso` is
  mounted and active.
- Her laptop is not blocked by tenant policy (J13 covers that).
- A colleague has sent her a sharing link to the Finance site's Documents library:
  `https://contoso.sharepoint.com/:f:/s/Finance/EjK3pQ2...`. The library holds about 8,400
  items.
- She is a member of the Team "Project Atlas".
- IT has given her the drive ID of the Policies library: `b!Xk2v...` (mixed case as copied).
- None of these libraries is mounted yet; `~/Contoso` does not exist.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mydrive mount add 'https://contoso.sharepoint.com/:f:/s/Finance/EjK3pQ2...'` | A preview: site "Finance", library "Documents", about 8,400 items, proposed mount point `~/Contoso/Finance - Documents`; she is asked to confirm | Resolves the link through `/shares/{encoded}` with `Prefer: redeemSharingLinkIfNecessary`. The link is not redeemed. The item count is added to the existing mounts' total and compared with the warning threshold near 300,000 |
| 2 | Confirms | The mount is added; `mydrive mount list` shows it | Link redeemed only now. New `mount_id`, its own delta token, backing directory and worker; tokens are the account's existing ones, so no browser opens. Initial enumeration (F02) |
| 3 | Waits, then `ls ~/Contoso/Finance\ -\ Documents` | The library's folders and files, all `online-only` | Metadata only; no content downloaded |
| 4 | Opens the settings window (`mydrive-ui`, M4) → Add library → Browse Teams | Her teams; she picks "Project Atlas" and its library | The UI asks the daemon over IPC; the daemon uses `filesFolder` and takes the item it returns |
| 5 | Clicks Add | `~/Contoso/Project Atlas - Documents` appears | Same as step 2 |
| 6 | `mydrive mount add b!Xk2v...` | Preview of the Policies library, proposed `~/Contoso/Policies - Documents`; it also shows the library as read-only. She confirms | `drive_id` normalised to lowercase. Read-only detection runs once for the library (J12) |
| 7 | `mydrive mount list` | Four mounts: `~/OneDrive - Contoso`, `~/Contoso/Finance - Documents`, `~/Contoso/Project Atlas - Documents`, `~/Contoso/Policies - Documents` (read-only), each with its own state | One request scheduler for the Contoso tenant serves all four |

## Variations

- **V1 Custom location.** `mydrive mount add '<link>' --at ~/work/finance` mounts there.
- **V2 Already mounted.** Adding a link whose root is the same (`drive_id`, `item_id`) as an
  existing mount is refused, naming the existing mount.
- **V3 Nested or overlapping.** `--at ~/OneDrive\ -\ Contoso/finance` (inside another mount),
  or a folder of a library already mounted, is refused (overview, Rulings).
- **V4 No access.** The link points to something Priya cannot open. The command fails with
  a no-access error; nothing is mounted and no access is granted.
- **V5 Near the item limit.** The library would bring the total across mounts close to
  300,000 items; the preview warns before she confirms.
- **V6 Blocked library.** See J13.
- **V7 Link to one folder inside a library.** See open questions.
- **V8 Site search.** Searching sites by name is not in M4; it is an extra that requests
  `Sites.Read.All` when opened (ADR-0010, ADR-0015).

## Acceptance criteria

- **J11-AC1** Given a sharing link to a library, when `mydrive mount add <link> --json` is run
  up to its confirmation step, then the output contains the site name, library name,
  `drive_id`, item count and proposed mount point; no mount has been created; and the
  simulator has received no request with `Prefer: redeemSharingLink`.
- **J11-AC2** Given the preview in J11-AC1, when the user confirms, then `mydrive mount list
  --json` includes a mount at `~/Contoso/Finance - Documents` with a lowercase `drive_id`,
  the path is a FUSE mount, and no browser was opened.
- **J11-AC3** Given a library of 8,400 items in the simulator, when the new mount becomes
  `active`, then its listing matches the simulator and the simulator recorded zero content
  downloads for it.
- **J11-AC4** Given a drive ID in mixed case, when `mydrive mount add <drive-id>` completes,
  then the mount's `drive_id` is lowercase and the library is mounted at
  `~/Contoso/<Site> - <Library>`.
- **J11-AC5** Given `--at ~/work/finance`, when the mount is added, then its mount point is
  `~/work/finance`.
- **J11-AC6** Given a library already mounted, when it is added again by link or by drive ID,
  then the command exits non-zero, names the existing mount, and `mydrive mount list --json`
  still contains one mount for it.
- **J11-AC7** Given a mount point inside an existing mount, when `mydrive mount add ... --at
  <that path>` runs, then it exits non-zero and no mount is created.
- **J11-AC8** Given a link the simulator refuses with 403, when `mydrive mount add <link>`
  runs, then it exits non-zero with a no-access reason, no mount is created, and no redeem
  request was sent.
- **J11-AC9** Given existing mounts totalling 295,000 items and a library of 8,400, when its
  preview is shown, then the preview includes a warning about the total item count (threshold
  configured near 300,000).
- **J11-AC10** Given a client connected to the IPC socket, when it browses Teams and adds the
  library of one team, then the result is a mount with the same properties as in J11-AC2 and
  its root is the item `filesFolder` returned.

## Must never happen

- A sharing link is redeemed, granting Priya permanent access, before she confirms.
- Adding a library asks her to sign in again, or creates a second account.
- Adding a library interrupts or slows the other mounts' syncing.
- Two mounts reach the same remote folder.
- The new mount point hides existing files in a non-empty directory.

## Open questions

1. How does the CLI confirm: an interactive prompt, a `--yes` flag, or a second command? How
   does `--json` work without a terminal?
2. A link to a single folder inside a library: mount just that folder, or the whole library?
   A link to a single file: refuse, or mount its library?
3. Mount point name for a Teams private channel, whose `filesFolder` is a drive root of its
   own site: `<Site> - <Library>` of that site?
4. How are site and library names sanitised and shortened for the mount point (characters
   such as `/` and `:`, long names)?
5. With two accounts signed in, which account does `mydrive mount add <link>` use?
6. Does a plain browser URL of a library (not a sharing link) resolve?
7. Libraries from another tenant (guest access): supported?
8. Folder naming `<Site> - <Library>` and Teams scopes are tracked as open-questions Q14 and
   Q11.
