# WP-014: M1 shared-file preparation

- **Status:** done
- **Milestone:** M1
- **Wave:** 5
- **Depends on:** —
- **Gates:** —
- **Owned paths:** root `Cargo.toml`, `Cargo.lock`, every crate's `Cargo.toml`;
  `crates/mydrive-engine/src/**`; `crates/mydrive-daemon/src/**`;
  `crates/mydrive-core/src/lib.rs` and the new stub files `clock.rs` and `conflict.rs`;
  `crates/mydrive-graph/src/lib.rs`,
  `crates/mydrive-graph/src/client/mod.rs`, `client/error.rs` (where `Operation` lives) and
  new files beside them;
  `crates/mydrive-graph-sim/src/**` and `crates/mydrive-graph-sim/README.md`,
  `crates/mydrive-graph-sim/docs/**`; `crates/mydrive-ipc/src/lib.rs` and the new stub
  files `server.rs` and `client.rs`
- **ADRs:** ADR-0003, ADR-0004, ADR-0018, ADR-0023
- **Flows / journeys:** —

## Goal

Rearrange the shared files once, with no behaviour change, so that every later M1 package
owns whole files and no two packages in a wave need to edit the same manifest, module list,
router or operation list. Wave 1 to 3 showed these are where parallel work collides.

## In scope

- **Manifests.** Give each crate the workspace dependencies M1 will need, so later packages
  do not edit `Cargo.toml`: engine (core, store, graph, tokio, tracing, thiserror; dev:
  graph-sim, tempfile, proptest); daemon (core, engine, ipc, tokio, tracing, anyhow, serde,
  and `toml` to read `config.toml`); cli (ipc, clap, serde_json, anyhow; not core, because
  ADR-0004 R3 limits the CLI to `mydrive-ipc`; if the CLI needs to name a core type, the
  package that builds the IPC client re-exports it from `mydrive-ipc`); ipc (tokio for the server); simulator (`sha2`). `toml` is added to
  `[workspace.dependencies]` with the lead's approval (MIT OR Apache-2.0). The overview's
  crate diagram gains the edge daemon to core for this package. Add a library target to `mydrive-daemon` beside its binary so tests can
  run the daemon in-process.
- **Engine module tree.** Declare, as documented empty modules, everything later packages
  will fill: `api`, `db`, `backing`, `clock`, `error`, `events`, `config`, `scheduler`,
  `accounts`, `mount`, `delta` (`convert`, `plan`, `run`, `resync`), `transfer` (`download`,
  `upload`), `local`, `conflict` (`decide`, `apply`), `recovery`, `library`, `policy`. Each is
  a file or directory with a module doc comment saying which package fills it.
- **Core module list.** Declare `clock` and `conflict` in `mydrive-core/src/lib.rs` with
  empty documented files (WP-015 fills them).
- **IPC module list.** Declare `server` and `client` (WP-041 fills them).
- **Graph client.** Split `client/mod.rs` so later packages add files, not lines to one
  file: declare `write`, `session`, `identity` and `discover` sub-modules as documented
  stubs; move nothing that changes a public path. Add the `Operation` variants M1's calls
  will need (upload, create folder, update item, delete item, create and use an upload
  session, `/me`, the user's drive, organisation, resolve a share, drive details), each with
  its `Display` arm, following the existing sanitising rule.
- **Simulator.** Restructure so each feature lives in its own file: the HTTP router is
  composed from per-feature routers; the control API is split into `impl GraphSim` blocks in
  feature files; the README becomes an index linking one document per feature under
  `docs/`. Declare the files later packages will own: `write.rs`, `session.rs`, `delta.rs`,
  `throttle.rs`, `identity.rs`, `authority.rs`, `discovery.rs`, as documented stubs.

## Out of scope

- Any behaviour, any new endpoint, any new error kind, any new public function beyond the
  `Operation` variants. If the rearrangement cannot be done without changing behaviour or a
  public path, stop and report.
- A websocket dependency (change notifications are deferred).

## Acceptance criteria

- AC1: `just check` passes, including `arch-check`.
- AC2: Every test that exists on `master` before this package still exists, unchanged in
  its body, and passes. The diff of test files is empty apart from `use` paths if a move
  required one, and each such change is listed in the report.
- AC3: The public API of every crate is unchanged except for additions: the list of public
  items from `cargo doc` (or an equivalent listing produced by a script in the report) before
  and after differs only by added items.
- AC4: The simulator's README still documents every switch with its default, and the
  defaults test still passes against it wherever the switch table now lives.
- AC5: No new dependency enters `Cargo.lock` other than those already in
  `[workspace.dependencies]`; `cargo deny` passes.
- AC6: Every stub module has a doc comment naming the package that will fill it, and the
  workspace has no `todo!`, `unimplemented!` or dead-code allowance added to make it compile.

## Required tests

- None new. The existing suites are the test: run the simulator and Graph client suites 5
  times.

## Notes

This package is a pure move. Its reviewer and verifier check exactly that. The list of
modules comes from `docs/backlog/drafts/M1-plan.md` section 4; where a name here and there
differ, this file wins.

## Open questions

None.
