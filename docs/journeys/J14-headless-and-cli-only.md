# J14 Headless and CLI only

**Goal:** on a machine with no desktop and no browser, reached over SSH, the user signs in by
pasting a redirect URL and does everything else from `mydrive`, with `--json` output that
scripts can rely on.

| | |
|---|---|
| Persona | Sam, on his headless home server `sam-nas` |
| Milestone | M2 |
| Related flows | F01 sign-in, F02 add mount and initial sync, F06 pin and dehydrate, F11 status for file managers |

## Preconditions

- `sam-nas` runs Arch Linux with no graphical session; Sam reaches it with
  `ssh sam@sam-nas`.
- `mydrive` is installed and the user unit enabled with `systemctl --user enable --now`.
- A Secret Service provider is running and unlocked in Sam's user session (how, on a headless
  machine, is an open question).
- `fuse3` is installed. The machine is online.
- Sam has a browser on his laptop.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mydrive doctor` | FUSE, keyring and network checks pass | Checks `fusermount3`, the Secret Service on D-Bus, and Graph reachability |
| 2 | `mydrive login --headless` | A sign-in URL and an instruction: open it in a browser on any device, sign in, then paste the full address the browser ends up on. A prompt waits for input | Daemon creates PKCE values and `state`, builds an authorization URL with a `http://localhost:<port>` redirect. It does not try to open a browser |
| 3 | Opens the URL on the laptop and signs in | The laptop's browser ends on `http://localhost:<port>/?code=...&state=...` and says the page cannot be reached. This is expected | Nothing on the laptop is listening |
| 4 | Copies that address and pastes it at the prompt | `Signed in as sam@outlook.com (personal)` | Daemon checks `state`, extracts the code, exchanges it, stores tokens in the Secret Service. Mount `~/OneDrive` is created and enumerated (J01) |
| 5 | `mydrive mount list --json \| jq '.'` | Valid JSON describing the mount | |
| 6 | `mydrive status ~/OneDrive/Documents/budget.xlsx --json` | JSON with hydration `placeholder`, pin `inherit`, sync `in-sync`, display status `online-only` | Wire forms from the overview |
| 7 | `mydrive pin ~/OneDrive/Backups/configs` | Pinned; files download | J06 |
| 8 | `mydrive pause 1h`, later `mydrive resume` | Confirmation each time | J15 |
| 9 | `mydrive activity --json` | JSON list of recent transfers and errors | |

## Variations

- **V1 Wrong URL pasted.** Sam pastes a URL with a different `state`, or junk. The CLI
  rejects it; nothing is signed in. He runs `mydrive login --headless` again.
- **V2 He takes too long.** The authorization code expires before he pastes it; the exchange
  fails with a message to start again.
- **V3 No Secret Service.** `mydrive login --headless` fails and names `mydrive doctor`, which
  says no Secret Service is available. No token is written anywhere (ADR-0010).
- **V4 Daemon not running.** Every command exits non-zero with "daemon not running" and how to
  start it; `~/OneDrive` is an empty directory.
- **V5 Device-code sign-in.** Offered only if gate G5 shows it works for personal accounts.
- **V6 Work account.** Same steps; the conditional-access rules of the tenant apply (J13).

## Acceptance criteria

- **J14-AC1** Given `mydrive login --headless` runs with a fake `xdg-open` on `PATH`, when it
  prints its URL, then the fake `xdg-open` is never invoked and the printed URL is an
  authorization URL with a `http://localhost:<port>` redirect and an `S256` code challenge.
- **J14-AC2** Given the printed URL, when the simulator's authorize step is completed and the
  resulting redirect URL is written to the command's standard input, then the command exits 0
  and `mydrive status --json` lists the account as `signed-in`.
- **J14-AC3** Given `mydrive login --headless` is waiting, when a redirect URL with a
  non-matching `state` is pasted, then sign-in does not complete, the command exits non-zero,
  and no account or Secret Service entry exists.
- **J14-AC4** Given each command in the overview's CLI table, when it is run with `--json`,
  then its standard output parses as JSON.
- **J14-AC5** Given each command in the overview's CLI table, when it succeeds it exits 0, and
  when it fails (for example an unknown path, or the daemon stopped) it exits non-zero.
- **J14-AC6** Given the daemon is stopped, when `mydrive status --json` runs, then it exits
  non-zero, reports that the daemon is not reachable, and `~/OneDrive` is an empty directory.
- **J14-AC7** Given state values in any `--json` output, when they are compared with the
  overview's wire forms, then each is the lowercase kebab-case form (for example
  `pending-upload`, `online-only`).
- **J14-AC8** Given `mydrive doctor --json` runs, then it reports each of the FUSE, keyring,
  network and policy checks with a pass or fail result.
- **J14-AC9** Given a headless sign-in, when terminal output and the journal are searched,
  then neither contains the access token, refresh token or PKCE verifier.

## Must never happen

- A token written to a file because no keyring was available.
- An access or refresh token printed to the terminal or the journal.
- A `--json` command mixing human-readable text into standard output.
- The CLI waiting forever for input it cannot get (no terminal, or stdin closed).

## Open questions

1. How does a headless user run and unlock a Secret Service without a graphical login?
   (D3 in open-questions records the decision not to fall back to a file.)
2. Should the daemon keep running after Sam logs out of SSH? That needs `loginctl
   enable-linger`, which the package does not set up.
3. How long does `--headless` wait for the paste before giving up?
4. Exit codes: one generic non-zero, or distinct codes for "daemon not running", "refused",
   "partly done"?
5. Error output in `--json` mode: a JSON error object on standard output, or text on
   standard error?
6. Is the `--json` schema versioned and stable before 1.0, given the IPC protocol may change
   (ADR-0016)?
