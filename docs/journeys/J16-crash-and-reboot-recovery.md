# J16 Crash and reboot recovery

**Goal:** when the daemon crashes or the machine loses power, the user loses no saved work,
never sees placeholders read as zeros, and everything carries on by itself after the restart.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 |
| Related flows | F04 local write and upload, F08 offline and recovery, F03 hydrate on read, F12 delete safety, F13 state machines |

## Preconditions

- `~/OneDrive` is mounted and active; the machine is online.
- `~/OneDrive/Documents/thesis.docx` was saved and closed one second ago (`PendingUpload`).
- `~/OneDrive/Archive/backup-photos.zip`, 1.2 GiB, is 40% uploaded (`Uploading`).
- `~/OneDrive/Videos/holiday-2026.mp4` is `Partial`, being read by mpv.
- A deletion of 340 items is held for confirmation (J05 step 7).
- The `mydrived` user unit restarts the daemon on failure.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | (The daemon crashes) | mpv stops with a read error. `ls ~/OneDrive` reports "Transport endpoint is not connected" | The FUSE connection is gone; the kernel fails requests on the stale mount |
| 2 | Waits a few seconds | `ls ~/OneDrive` works again | systemd restarts `mydrived`. The daemon unmounts the stale mount, runs the recovery scan of each backing directory against the store, then mounts again |
| 3 | `mydrive status ~/OneDrive/Documents/thesis.docx` | `syncing`, then `local` | The `PendingUpload` record was committed before the close returned; the upload runs after restart |
| 4 | `mydrive activity` | The zip upload continuing | Upload session resumed from `nextExpectedRanges`, or restarted if the session expired |
| 5 | Plays the video again | It plays | Recorded extents are served locally; missing ranges are fetched |
| 6 | `mydrive activity` | The 340-item deletion is still waiting for confirmation | Held deletes are kept across restart |
| 7 | Later, the laptop loses power during another upload | — | — |
| 8 | Boots and logs in | `~/OneDrive` is an empty directory for a moment, then the files are there | The user unit starts with the session; same recovery as step 2 |
| 9 | `mydrive status` | Uploads that were pending before the power cut are running or done | Every `PendingUpload` and `Uploading` item is resumed (ADR-0012) |

## Variations

- **V1 The daemon will not start.** For example the state store is damaged. `mydrive status`
  says the daemon is not running; `systemctl --user status mydrived` and the journal give the
  error; `~/OneDrive` stays an empty directory or a stale mount. Nothing is deleted.
- **V2 A program writes into `~/OneDrive` while the daemon is down.** See open questions.
- **V3 The file changed on the server while the daemon was down.** On restart the pending
  upload gets 412; conflict rules apply (J10).
- **V4 Crash during free up space or a conflict split.** On restart the item is in a
  consistent state: either before or after the operation, never with content missing that
  was not on the server.

## Acceptance criteria

- **J16-AC1** Given a file whose write was closed and fsynced, when the daemon is killed with
  `SIGKILL` before uploading it and is restarted, then the local content is unchanged and the
  simulator's content for it becomes byte-identical without user action.
- **J16-AC2** Given the daemon was killed with `SIGKILL` and not yet restarted, when a process
  reads a file under the mount point, then the call fails with an error (for example
  `ENOTCONN`) and never returns zeros or an empty file.
- **J16-AC3** Given the daemon was killed with `SIGKILL`, when systemd restarts it, then the
  mount point serves files again without the user running `fusermount3` or any other command.
- **J16-AC4** Given an upload session interrupted by a crash at 40%, when the daemon restarts
  and the upload completes, then the simulator has exactly one item with that name in that
  folder and its content equals the local file.
- **J16-AC5** Given a `Partial` file whose hydration was interrupted by a crash, when it is
  read in full after restart, then every byte equals the simulator's content.
- **J16-AC6** Given the daemon is stopped cleanly, when the mount point is listed, then it is
  an empty directory.
- **J16-AC7** Given a held deletion of 340 items, when the daemon is killed and restarted, then
  the simulator still has received no delete for those items and `mydrive activity --json`
  still reports the deletion as held.
- **J16-AC8** Given crash points injected throughout upload, hydration, free up space and the
  conflict split (ADR-0017 crash tests), when the daemon is restarted after each, then no
  content that was not on the server is lost and no placeholder reads as zeros.
- **J16-AC9** Given a pin recorded before a crash, when the daemon restarts, then the pin is
  still in effect and pending pin downloads continue.

## Must never happen

- A crash causes a truncated or partly written file to be uploaded as the new version.
- After restart, local content missing or unreadable is taken as a delete and sent to the
  server.
- A held mass deletion is carried out by the restart.
- A stale mount returns zeros.
- A crash during upload leaves a duplicate item on the server (`thesis (1).docx`).

## Open questions

1. Is data from `write()` calls that were not followed by `close` or `fsync` guaranteed to
   survive a daemon crash (as opposed to a power cut)?
2. Is there a target time from crash to the mount serving again?
3. If a program writes into the empty mount point while the daemon is down, those files are
   hidden once the mount comes back. Should the daemon refuse to mount over a non-empty
   directory, move the files in, or report them?
4. Does the daemon start at login only, or at boot with lingering? (J14 asks the same for
   headless machines.)
5. Programs that held file descriptors on the crashed mount keep getting errors until they
   reopen. Should the user be told which programs to restart?
6. What does recovery do when the state store itself is damaged?
