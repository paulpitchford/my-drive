# J01 First run and sign-in

**Goal:** a new user signs in once and ends up with their OneDrive mounted at `~/OneDrive`,
every file visible with its real name and size, and nothing downloaded.

| | |
|---|---|
| Persona | Sam (Priya in variations) |
| Milestone | M2 (CLI path). The first-run window in variation V8 is M4. |
| Related flows | F01 sign-in, F02 add mount and initial sync, F13 state machines |

## Preconditions

- The `mydrive` package is installed (ADR-0016) and the user has run
  `systemctl --user enable --now` for the `mydrived` unit.
- gnome-keyring is running and its login collection is unlocked, so a Secret Service is
  available on D-Bus.
- The machine is online. Chromium is the default browser (`xdg-open` opens it).
- Sam's personal OneDrive (`sam@outlook.com`) holds about 14,000 items, 38 GB in total.
- `~/OneDrive` does not exist.
- No account is signed in.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | Runs `mydrive status` | Daemon running, no accounts, no mounts | CLI connects to `$XDG_RUNTIME_DIR/mydrive/daemon.sock`, completes `hello`, asks for status |
| 2 | Runs `mydrive login` | A line saying the browser is opening, with the sign-in URL printed as a fallback. Chromium opens the Microsoft sign-in page | Daemon creates a PKCE verifier and challenge, binds a loopback listener on `http://localhost:<random port>`, builds the authorization URL with scopes `Files.ReadWrite.All offline_access User.Read`, calls `xdg-open` |
| 3 | Signs in as `sam@outlook.com` and accepts the consent screen | The browser tab shows a short page saying sign-in is complete and the tab can be closed | Loopback listener accepts exactly one redirect whose `state` matches, then closes. Daemon exchanges the code for tokens and writes them to the Secret Service, one entry for the account |
| 4 | Looks back at the terminal | `Signed in as sam@outlook.com (personal)`; the command exits 0 | Daemon fetches the user's drive, normalises the `drive_id` to lowercase, creates a mount with mount point `~/OneDrive` and backing directory `$XDG_DATA_HOME/mydrive/mounts/<mount_id>/`, and starts the initial delta enumeration (F02) |
| 5 | Runs `mydrive mount list` | One mount, `~/OneDrive`, shown as `resyncing`, with a count of items listed so far | Engine pages through delta, writing metadata into the state store. No content is requested |
| 6 | Runs `ls ~/OneDrive` while step 5 is still running | An empty directory | The mount point is not mounted until enumeration completes (F13) |
| 7 | Waits, then runs `mydrive mount list` again | `~/OneDrive` is active | Enumeration done. Placeholders materialised as sparse files in the backing directory. FUSE filesystem mounted at `~/OneDrive`. A `subscriptions/socketIo` endpoint is obtained for change notification |
| 8 | Runs `ls ~/OneDrive` | `Desktop  Documents  Music  Pictures  Videos` and the rest of Sam's top-level folders | Answered from the state store |
| 9 | Runs `du -sh ~/OneDrive` and `du -sh --apparent-size ~/OneDrive` | About 0 and 38G | Placeholders report `st_blocks = 0` and `st_size` = cloud size |

## Variations

- **V1 Browser does not open.** No default browser, or `xdg-open` fails. The CLI says so and
  keeps waiting; Sam copies the printed URL into a browser by hand. The rest is unchanged.
- **V2 Sam cancels or declines consent.** The browser shows Microsoft's error. `mydrive login`
  exits non-zero with "sign-in cancelled". No account, no mount, no keyring entry.
- **V3 No Secret Service.** gnome-keyring is not running. `mydrive login` fails with an error
  that names `mydrive doctor`; `mydrive doctor` says no Secret Service provider was found and
  how to provide one. Nothing is written anywhere (ADR-0010).
- **V4 Keyring locked.** The keyring's own unlock prompt appears. If Sam dismisses it, sign-in
  fails as in V3.
- **V5 Work account (Priya).** Priya runs `mydrive login` and signs in as `priya@contoso.com`.
  Her drive mounts at `~/OneDrive - Contoso`. Her libraries are added later (J11).
- **V6 The organisation refuses the app.** The tenant has not allowed the app (for example
  AADSTS65005). The browser shows Microsoft's error page; `mydrive login` exits non-zero and
  says the organisation has not allowed this app and an administrator must approve it.
- **V7 Daemon restarts after sign-in.** `systemctl --user restart mydrived`. No browser opens;
  the account is still signed in and `~/OneDrive` comes back, using the refresh token from the
  keyring.
- **V8 First-run window (M4).** `mydrive-ui` offers the same sign-in through a "Sign in"
  button that opens the system browser. It then shows the mount being prepared. Same
  outcomes as the CLI path.
- **V9 Refresh token expires later** (for example after 90 days unused). The account enters
  `needs-sign-in`. `mydrive status` says so; reads of online-only files fail as in J03 with
  the reason "sign-in required". Running `mydrive login` again restores it without creating a
  second account.

## Acceptance criteria

- **J01-AC1** Given no account is signed in and the Graph simulator accepts the sign-in, when
  `mydrive login` completes, then it exits 0 and `mydrive status --json` lists exactly one
  account, of type personal, in state `signed-in`, with the signed-in user's name.
- **J01-AC2** Given `mydrive login` is running, when it opens the browser, then it invokes the
  desktop URL handler (`xdg-open`) with an authorization URL whose `redirect_uri` is
  `http://localhost:<port>`, whose `code_challenge_method` is `S256`, and whose scopes are
  exactly `Files.ReadWrite.All offline_access User.Read`. No embedded web view is started.
- **J01-AC3** Given sign-in succeeded, when every file under `$XDG_CONFIG_HOME/mydrive`,
  `$XDG_STATE_HOME/mydrive`, `$XDG_DATA_HOME/mydrive` and the daemon's journal output is
  searched for the access token, refresh token, authorization code and PKCE verifier, then
  none is found, and the Secret Service holds one entry for the account.
- **J01-AC4** Given the loopback listener is waiting, when a redirect arrives with a `state`
  that does not match, then sign-in does not complete and no account is created.
- **J01-AC5** Given no Secret Service is available on the session bus, when `mydrive login`
  runs, then it exits non-zero, its error names `mydrive doctor`, no account is listed, and
  no file containing a token exists on disk.
- **J01-AC6** Given sign-in completed for a drive of 14,000 items in the simulator, when the
  mount reaches the active state, then `find ~/OneDrive | wc -l` counts every item, each
  file's `st_size` equals its cloud size, and the simulator recorded zero content download
  requests.
- **J01-AC7** Given enumeration is still in progress, when `~/OneDrive` is listed, then it is
  an empty directory and not a FUSE mount (`findmnt ~/OneDrive` finds nothing).
- **J01-AC8** Given an account signed in earlier, when `mydrived` is restarted, then the
  account is still signed in, `~/OneDrive` is mounted again, and no browser was opened.
- **J01-AC9** Given a work account in tenant "Contoso", when sign-in completes, then
  `mydrive mount list --json` shows its own drive at `~/OneDrive - Contoso`.

## Must never happen

- A token, authorization code or PKCE verifier written to disk, logs or an IPC response.
- An embedded web view used for sign-in.
- Initial sync downloading file content.
- `~/OneDrive` showing a partial tree as if it were complete while enumeration runs.
- Signing in again creating a duplicate account or a second mount of the same drive.

## Open questions

1. Is the account's own drive mounted automatically on sign-in, as written above, or does the
   user add it with `mydrive mount add`? The overview's CLI lists no separate step.
2. What happens when `~/OneDrive` (or `~/OneDrive - <Org>`) already exists and is not empty,
   for example left by another client? Refuse, choose another path, or ask?
3. Where does `<Org>` come from (tenant display name?), and how is it sanitised and shortened?
4. Two work accounts in the same tenant collide on `~/<Org>` (tracked as open-questions Q14).
5. How is initial-sync progress reported (items listed, estimated total), and can the user
   cancel an enumeration in progress?
6. What does `<account>` look like in `mydrive logout <account>` and other commands: the
   sign-in address, an account ID, or both?
7. Whether Omarchy's SDDM login unlocks gnome-keyring automatically was not checked (ADR-0010).
8. The overview's mount states are `active`, `paused`, `blocked`, `resyncing`; F13 adds
   `Initialising` with the mount point not yet mounted. Which state does `mydrive mount list
   --json` report during the first enumeration?
