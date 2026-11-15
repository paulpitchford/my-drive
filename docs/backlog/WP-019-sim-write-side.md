# WP-019: Simulator write side

- **Status:** blocked on WP-014
- **Milestone:** M1
- **Wave:** 6
- **Depends on:** WP-014
- **Gates:** —
- **Owned paths:** `crates/mydrive-graph-sim/src/model.rs`, `src/write.rs`,
  `crates/mydrive-graph-sim/tests/write_*.rs`, `crates/mydrive-graph-sim/docs/write.md`,
  `crates/mydrive-graph/tests/sim_write_*.rs`
- **ADRs:** ADR-0017, ADR-0012, ADR-0011
- **Flows / journeys:** F04, F07, F12

## Goal

The simulator can only be read today. Make it accept writes the way Microsoft Graph does,
including the refusals and server-side changes that hurt sync clients, so the upload and
conflict work can be tested without a real account.

## In scope

- Simple upload: replace content by item ID, and create by parent and name.
- Create folder; update an item (rename, move); delete an item.
- `If-Match` on every write that documents it, answering `412` on a stale eTag.
- `@microsoft.graph.conflictBehavior`; a name that exists (case-insensitively) with `fail`
  answers `409` with the `nameAlreadyExists` code.
- A quota that answers `507` when exceeded; `423` for a locked item, by switch.
- `fileSystemInfo` timestamps supplied on upload are kept, at whole seconds.
- SharePoint post-upload rewriting, by switch on library drives: for chosen extensions the
  stored content, size and hash differ from what was uploaded, either already in the upload
  response or only in a later delta (two modes).
- An internal function, usable by the upload-session package, that commits a completed upload
  to the model.
- Switches for what is unverified, defaulting to the case harder for a client: whether a
  simple replace honours `If-Match` (default: not honoured); whether a rename changes
  `lastModifiedDateTime`.

## Out of scope

- Upload sessions (WP-020). Throttling model, 410 model, identity endpoints.

## Acceptance criteria

- AC1: A replace with a stale `If-Match` on a route that honours it answers `412` and changes
  nothing; with the current eTag it succeeds and the eTag and cTag change.
- AC2: A create with `conflictBehavior=fail` on a name differing only in case answers `409
  nameAlreadyExists`; no item is created.
- AC3: Every write appears in a later delta exactly as the same change made through the
  control API does, in both repeat modes.
- AC4: With rewriting on, the hash and size served afterwards differ from those of the
  uploaded bytes, downloads return the rewritten bytes, and in "later" mode the upload
  response still reports the uploaded bytes' hash.
- AC5: A delete with a stale `If-Match` answers `412`; a deleted item then appears in delta
  with the `deleted` facet, following the folder-delete switch for its descendants.
- AC6: The request log records the `if-match` header of every write.
- AC7: Exceeding the quota answers `507` and stores nothing.
- AC8: The defaults test covers every new switch, and `docs/write.md` states the source of
  each behaviour and marks each unverified one.

## Required tests

- Self-tests per criterion with a plain HTTP client, and one that drives a write and then
  reads the result back through the existing `mydrive-graph` client. That one lives in
  `crates/mydrive-graph/tests/sim_write_*.rs`: the simulator may not depend on
  `mydrive-graph`, even for tests (the architecture check refuses it).

## Notes

Shared files, as WP-014 left them: the write routes returned by `write::routes()` are already
merged into the Graph router; `lib.rs` re-exports everything public in `write.rs` with a
glob; `State` holds a `write::WriteState` for state that is not part of the item model. So
declare new public types (switches, modes) in `write.rs`, not `model.rs`, whose public types
are re-exported by name from `lib.rs`. New fields on the existing `DriveOptions` are fine.
Do not edit `lib.rs` or `server.rs` (WP-023 owns them in this wave). Refusing writes on a
read-only library is WP-023's, done before a request reaches your handlers.

Follow the simulator's rule: the default is what Microsoft documents; where that is unknown,
a switch whose default is harder for a client. The research note
`research_notes/OneDrive Linux on demand clients/graph_api_and_windows_parity.md` records
what is known about uploads and rewriting.

## Open questions

None.
