# User journeys

A journey is one thing a user sets out to do with my-drive, told from the user's side: what
they do, what they see, and what the system does underneath. Each journey covers the happy
path and its main unhappy branches. Together the journeys say what "done" means; their
acceptance criteria become the journey acceptance tests (ADR-0017, layer 5).

Journeys follow the vocabulary, state names, paths and CLI commands of
`docs/architecture/overview.md`. If a journey disagrees with the overview, the overview wins
and the journey is fixed. ADRs in `docs/adr/` and flows in `docs/flows/` give the mechanism;
journeys give the outcome.

## Structure of a journey file

1. Title and one-line goal.
2. Persona, milestone, related flows.
3. Preconditions.
4. Happy path: a numbered table with "User does", "User sees" and "System does". Anything
   the user cannot observe goes in "System does".
5. Variations: alternative and unhappy branches, each with what the user sees.
6. Acceptance criteria.
7. Must never happen: data-safety and trust violations specific to the journey.
8. Open questions: what the overview and ADRs leave undecided that affects the journey.
   Questions already in `docs/architecture/open-questions.md` are referenced by number, not
   repeated.

## Acceptance criteria

- **IDs.** `Jnn-ACm`, for example `J03-AC4`: journey number, then a sequence number within the
  journey. IDs are stable. A criterion that is dropped keeps its ID retired; it is never
  reused or renumbered. New criteria take the next free number.
- **Form.** Given / when / then. Each criterion is testable on its own.
- **One test each.** Every ID has exactly one journey test, which names the ID (ADR-0017).
- **Observable from outside the daemon.** A criterion is checked only through:
  - the filesystem under the mount point (POSIX calls, `stat`, xattrs `user.mydrive.*`);
  - the CLI, with `--json`, and its exit status;
  - the IPC socket (`$XDG_RUNTIME_DIR/mydrive/daemon.sock`);
  - the Graph simulator (`mydrive-graph-sim`), for what reached or left "the server";
  - the environment the daemon talks to: the Secret Service, the session D-Bus (desktop
    notifications), `~/.cache/thumbnails`, `config.toml`, the journal.

  Tests never open `state.db` or the backing directories to decide a result.
- **Wire forms.** State values in `--json`, IPC and xattrs are lowercase kebab-case
  (`placeholder`, `in-sync`, `pending-upload`, `online-only`; overview, Wire forms). Prose and
  "System does" cells use the overview's axis names (`Placeholder`, `InSync`).
- **Bounds.** Where the design gives a number it is in the criterion: reads that cannot be
  fetched fail with `EIO` within 30 s; uploads start no earlier than 2 s after the last close;
  the mass-delete guard holds more than 200 items or more than 50% of a mount; the crawler
  breaker trips above 50 files or 1 GiB per process in 60 s. Durations such as "5 minutes of
  simulated time" are test windows, not design limits.
- **"Told why".** When an operation fails for a reason the user cannot see in the
  application (offline, `needs-sign-in`, `blocked`), the reason is available from
  `mydrive status` and `mydrive activity` from M2, and as a desktop notification from M3.

## Index

| ID | Title | Persona | Milestone | Related flows | ACs |
|---|---|---|---|---|---|
| [J01](J01-first-run-sign-in.md) | First run and sign-in | Sam | M2 (UI variant M4) | F01, F02, F13 | 11 |
| [J02](J02-browse-without-downloading.md) | Browse without downloading | Sam | M2 | F02, F03, F11, F13 | 7 |
| [J03](J03-open-an-online-only-file.md) | Open an online-only file | Sam | M2 | F03, F08, F10, F13 | 10 |
| [J04](J04-edit-and-save.md) | Edit and save | Sam, Priya | M2 | F04, F07, F10, F13 | 10 |
| [J05](J05-create-rename-move-delete.md) | Create, rename, move and delete | Sam, Priya | M2 | F04, F12, F05, F13 | 10 |
| [J06](J06-always-keep-on-this-device.md) | Always keep on this device | Sam | M2 | F06, F05, F10, F13 | 9 |
| [J07](J07-free-up-space.md) | Free up space | Sam | M2 | F06, F04, F13 | 8 |
| [J08](J08-change-from-another-device.md) | Change from another device | Sam | M2 | F05, F07, F10, F13 | 9 |
| [J09](J09-offline-and-reconnect.md) | Offline and reconnect | Priya | M2 | F08, F04, F03, F10 | 9 |
| [J10](J10-conflict.md) | Conflict | Sam | M2 | F07, F04, F05, F13 | 9 |
| [J11](J11-add-a-sharepoint-library.md) | Add a SharePoint library | Priya | M4 (CLI by link or drive ID from M2) | F09, F02, F01, F10 | 10 |
| [J12](J12-read-only-library.md) | Read-only library | Priya | M2 | F09, F04, F11, F13 | 8 |
| [J13](J13-library-blocked-by-policy.md) | Library blocked by organisation policy | Priya | M2 | F09, F08, F10, F11, F13 | 8 |
| [J14](J14-headless-and-cli-only.md) | Headless and CLI only | Sam | M2 | F01, F02, F06, F11 | 10 |
| [J15](J15-pause-resume-sign-out-remove-mount.md) | Pause, resume, sign out and remove a mount | Priya | M2 | F08, F10, F12, F01, F02 | 10 |
| [J16](J16-crash-and-reboot-recovery.md) | Crash and reboot recovery | Sam | M2 | F04, F08, F03, F12, F13 | 9 |
| [J17](J17-file-manager-on-omarchy.md) | File manager on Omarchy | Sam | M3 | F11, F06, F03, F13 | 8 |
| [J18](J18-crawler-touches-the-mount.md) | A crawler touches the mount | Sam | M2 (Files thumbnails M3) | F03, F10, F11, F13 | 9 |

Total: 164 acceptance criteria.

The milestone is the one at which the journey's acceptance criteria must pass. Steps that
need a later milestone (notifications and tray in M3, the settings window in M4, Nautilus
emblems in M5) say so in place; their criteria are written so that the M2 part can be tested
without them.

Out of scope for every journey: Personal Vault, Office desktop co-authoring and differential
upload (not possible through public Graph), and shortcuts ("Add shortcut to My files", M5).

## Personas

### Sam

- Uses Omarchy on a laptop with hostname `sam-laptop`, and has a headless home server
  `sam-nas`.
- Personal Microsoft account `sam@outlook.com`; about 14,000 items, 38 GB in OneDrive.
- Works mostly in the terminal (Neovim, `ls`, `grep`, `mpv`) and in Omarchy's file manager,
  Files (`folio`). Uses LibreOffice for spreadsheets, and edits files from his phone too.
- Mount: `~/OneDrive`.
- Cares about: not filling his disk, files working from the shell, no surprises.

### Priya

- Uses a work laptop with hostname `priya-x1`, running a mainstream GNOME distribution
  (Nautilus is her file manager, so emblems arrive for her with M5).
- Work account `priya@contoso.com` in tenant Contoso, with OneDrive for Business and three
  SharePoint libraries from her employer:
  - `~/Contoso/Finance - Documents`: writable, about 8,400 items.
  - `~/Contoso/Project Atlas - Documents`: a Teams team's library; later blocked by policy
    (J13).
  - `~/Contoso/Policies - Documents`: read-only (required check-out).
- Her own drive: `~/OneDrive - Contoso`.
- Travels, works offline on flights, and does not control her organisation's policies.
- Cares about: never losing a change, knowing when IT policy is the problem, other
  libraries carrying on when one fails.
