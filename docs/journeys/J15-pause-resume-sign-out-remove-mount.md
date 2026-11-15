# J15 Pause, resume, sign out and remove a mount

**Goal:** the user can stop syncing for a while, stop syncing one library for good, or sign
out entirely, and none of these ever deletes anything on OneDrive or throws away local
copies.

| | |
|---|---|
| Persona | Priya |
| Milestone | M2 |
| Related flows | F08 offline and recovery, F10 throttling and scheduler, F12 delete safety, F01 sign-in, F02 add mount and initial sync |

## Preconditions

- Priya is signed in as `priya@contoso.com` with four mounts (J11). All `active`.
- `~/Contoso/Project Atlas - Documents` holds 1,900 items, of which 140 files (2.1 GiB) are
  local; the project has ended.
- She is on a phone hotspot for the next two hours.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | `mydrive pause 2h` | Syncing paused until a time two hours from now | Every mount enters `paused`, with the end time recorded |
| 2 | Saves `~/OneDrive - Contoso/Q4 Plan/roadmap.docx` | Saved; status `syncing` | Local write, `PendingUpload`; upload queued |
| 3 | Opens an online-only file | It opens | Hydration on read still works while paused (F13) |
| 4 | `mydrive status` | Paused, with the resume time; one change waiting | |
| 5 | Back on office Wi-Fi early: `mydrive resume` | Syncing resumed | Mounts `active`; the queued upload starts |
| 6 | `mydrive mount list` | The four mounts with their IDs | |
| 7 | `mydrive mount remove <mount_id of Project Atlas>` | Confirmation that syncing stopped; nothing was deleted online; where the local copies are | Worker stops; FUSE filesystem unmounted; each of the 140 fully local files is written into the now-plain mount-point directory at its path, then the backing directory and the mount record are removed (ADR-0021, F12). No delete request is sent |
| 8 | Opens the Project Atlas library on SharePoint | Everything is still there | |
| 9 | Later, leaving the job: `mydrive logout priya@contoso.com` | Signed out; local files stay | The account's Secret Service entry is removed. No delete request is sent |
| 10 | `mydrive status` | No Contoso account | |

## Variations

- **V1 Pause expires.** Without `mydrive resume`, syncing resumes by itself when the two hours
  are up, and the queued upload goes.
- **V2 Pause without a duration.** `mydrive pause` pauses until `mydrive resume`.
- **V3 Remove a mount with changes not yet uploaded.** See open questions; nothing may be
  dropped silently.
- **V4 Delete the folder instead of removing the mount.** `rm -rf ~/Contoso/Project\ Atlas\ -\
  Documents/*` is a delete of every item in a mount, not a "stop syncing". It is more than
  200 items and more than 50% of the mount, so it is held for confirmation (J05).
- **V5 Add it again later.** `mydrive mount add` for the same library creates a new mount.
- **V6 Sign-in expires instead.** The account shows `needs-sign-in`; its mounts stop syncing;
  local content stays readable (overview, Account and mount states).

## Acceptance criteria

- **J15-AC1** Given `mydrive pause 2h`, when the simulator's request log is inspected over the
  following 2 hours of simulated time, then it contains no upload and no delete, and a local
  save during that time succeeds with sync `pending-upload`.
- **J15-AC2** Given every mount is paused, when an online-only file is read, then the read
  returns the simulator's content.
- **J15-AC3** Given `mydrive pause 2h`, when `mydrive status --json` runs during the pause, then
  every mount is `paused` and the output gives the time the pause ends.
- **J15-AC4** Given `mydrive pause 2h` and a pending upload, when 2 hours of simulated time
  pass with no further command, then the mounts are `active` again and the upload completes.
- **J15-AC5** Given a pause and a pending upload, when `mydrive resume` runs, then the mounts
  are `active` and the upload starts no earlier than 2 s after the file's last close.
- **J15-AC6** Given a mount with local files, when `mydrive mount remove <mount>` runs, then
  the simulator receives no delete request for any item of that drive, and every item still
  exists there.
- **J15-AC7** Given J15-AC6, when it completes, then the content of every file that was local
  is still on local disk, byte-identical, at the location the command reports in its `--json`
  output.
- **J15-AC8** Given J15-AC6, when it completes, then the mount no longer appears in
  `mydrive mount list --json`, its mount point is no longer a FUSE mount, and the other mounts
  are unchanged and `active`.
- **J15-AC9** Given an account with mounts, when `mydrive logout <account>` runs, then the
  Secret Service holds no entry for it, `mydrive status --json` no longer lists it as
  `signed-in`, the simulator received no delete request, and no local content was removed.
- **J15-AC10** Given an account that has signed out, when any file of its former mounts is
  read, then the read returns real local content or fails with `EIO` within 30 seconds; it
  never returns zeros.

## Must never happen

- Removing a mount, unmounting, or signing out deletes anything on OneDrive.
- Removing a mount or signing out deletes local copies without asking.
- A change waiting to upload is silently dropped by a remove or a sign-out.
- A pause discards queued changes or makes them upload in the wrong order afterwards.

## Open questions

1. After `mydrive mount remove`, where are the local copies? Answered by ADR-0021: each fully
   local file is written into the mount-point directory at its path, since the backing directory
   is not browsable; placeholders and partly downloaded files are not written.
2. If a mount or account has pending uploads, does remove or logout refuse, upload first, or
   ask?
3. After `mydrive logout`, do the account's mounts stay mounted (readable local content,
   failing placeholders) or go away?
4. Is `<mount>` in `mydrive mount remove <mount>` the `mount_id`, the mount point path, or
   either?
5. Does a pause survive a daemon restart or reboot?
6. Can one mount be paused on its own? The overview's `mydrive pause` takes only a duration.
7. Re-adding a removed library: are the kept local copies reused, or downloaded again?
