# J10 Conflict

**Goal:** when the same file changes here and elsewhere before the two meet, the user ends up
with both versions side by side, is told about it, and nothing is lost.

| | |
|---|---|
| Persona | Sam |
| Milestone | M2 (notification in step 5 arrives with M3) |
| Related flows | F07 conflict, F04 local write and upload, F05 remote change, F13 state machines |

## Preconditions

- `~/OneDrive/Documents/budget.xlsx`, 48,213 bytes, local, unpinned, in sync at server eTag
  E1.
- The laptop's hostname is `sam-laptop`.
- The laptop is offline (or the mount is paused) so that its next upload is delayed.

## Happy path

| # | User does | User sees | System does |
|---|---|---|---|
| 1 | On the laptop, offline, edits `budget.xlsx` and saves | Saved | Local write; sync `PendingUpload` with base eTag E1 |
| 2 | On his phone, edits the same file in Excel | — | The server now has eTag E2 |
| 3 | Reconnects the laptop | — | Either delta reports the content change for an item with a pending upload, or the upload with `If-Match: E1` gets 412. Sync `Conflict` (F07) |
| 4 | `ls ~/OneDrive/Documents` | `budget.xlsx` and `budget-sam-laptop.xlsx` | Split: the original name takes the server's version (E2); the local bytes become a new item `budget-sam-laptop.xlsx`, which uploads as a new file. The original returns to `InSync` |
| 5 | Looks at `mydrive activity` | A conflict entry: `budget.xlsx` changed here and on another device; this device's version was kept as `budget-sam-laptop.xlsx`. With M3, a notification says the same | Conflict recorded in the activity log |
| 6 | Opens both files | `budget.xlsx` has the phone's edit; `budget-sam-laptop.xlsx` has the laptop's | |
| 7 | Opens onedrive.com | Both files are there | |
| 8 | Merges the laptop's change into `budget.xlsx`, saves, deletes `budget-sam-laptop.xlsx` | Both actions behave as normal edits and deletes (J04, J05) | `budget-sam-laptop.xlsx` goes to the recycle bin |

## Variations

- **V1 Conflict while online.** Sam saves at the same moment as the phone; the server's
  change arrives inside the 2-second quiet period. Same outcome as steps 3 to 5.
- **V2 Edited here, deleted elsewhere.** Sam edits `notes.md` offline; it is deleted on the
  web. On reconnect the local edit is kept, and `mydrive activity` reports what happened.
- **V3 Deleted here, edited elsewhere.** Sam deletes `plan.docx` offline; it is edited on the
  web. On reconnect the edited version is not deleted; `plan.docx` reappears locally with the
  server's content, and `mydrive activity` reports it.
- **V4 No extension.** A conflict on `README` keeps the local version as `README-sam-laptop`
  (ADR-0012).
- **V5 Both created a new file with the same name.** The server rejects the create with
  `409 nameAlreadyExists`; handled as a conflict, with the local file kept under the
  conflict name.
- **V6 SharePoint rewrote the file after upload (Priya).** Not a conflict (J04 V7).
- **V7 Pinned file.** The original stays pinned and gets the server version; see open
  questions for the copy.

## Acceptance criteria

- **J10-AC1** Given a local file with a pending upload based on eTag E1, and the simulator now
  at E2 with different content, when the client reconciles, then reading `budget.xlsx`
  returns the E2 content and reading `budget-sam-laptop.xlsx` returns the local edit's bytes.
- **J10-AC2** Given J10-AC1, when the simulator is inspected, then the original item keeps its
  `item_id` and E2 content, and a new item `budget-sam-laptop.xlsx` in the same folder holds
  the local edit's bytes.
- **J10-AC3** Given J10-AC1, when the simulator's request log is inspected, then no upload
  replaced the E2 content of the original item.
- **J10-AC4** Given a conflict was resolved by the split, when `mydrive activity --json` runs,
  then it lists one conflict entry naming both paths.
- **J10-AC5** Given the machine's hostname is `sam-laptop`, when conflicts are split for
  `budget.xlsx` and for `README`, then the local copies are named `budget-sam-laptop.xlsx` and
  `README-sam-laptop`.
- **J10-AC6** Given a local save and a server change that both happen within the same 2-second
  quiet period, when the client reconciles, then both versions exist as in J10-AC1 and
  J10-AC2.
- **J10-AC7** Given a file edited locally while offline and deleted on the simulator, when the
  client reconnects, then the local edit's bytes are still readable in the mount and
  `mydrive activity --json` reports the event.
- **J10-AC8** Given a file deleted locally while offline and edited on the simulator, when the
  client reconnects, then the simulator's item is neither deleted nor in the recycle bin, and
  the file is present locally with the server's content.
- **J10-AC9** Given a conflict copy `budget-sam-laptop.xlsx` that has uploaded, when the client
  runs for a further 5 minutes of simulated time with no new changes, then no further copy
  (such as `budget-sam-laptop-sam-laptop.xlsx`) is created.

## Must never happen

- Last writer wins: either version silently replaced.
- A conflict copy is created and never uploaded, so the other devices never see it.
- Resolving a conflict deletes either version.
- Conflict copies multiply in a loop.
- A remote delete removes local changes that never reached the server.

## Open questions

1. Multi-dot and hidden names: what is `<ext>` for `archive.tar.gz` or `.bashrc`?
2. In V2 (edited here, deleted elsewhere), is the local edit uploaded again under the
   original name, kept under the conflict name, or kept local only?
3. Does the conflict copy inherit the original's pin, and is it pinned explicitly?
4. How does the user see that unresolved conflict copies exist, beyond the activity log?
   F13 open question 3 notes `Conflict` is visible only briefly.
5. Rename on both sides to different names: which name wins?
6. What happens if the file is still open in the editor when the split happens? Does the
   editor keep writing to the original name, which now holds the server version?
7. Existing conflict names and invalid conflict names are tracked as open-questions Q7.
