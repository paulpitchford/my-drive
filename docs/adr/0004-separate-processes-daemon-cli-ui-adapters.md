# ADR-0004: Separate processes for daemon, CLI, UI and adapters

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

Linux has no platform component for registering a sync root or drawing per-file status, so
each file manager needs its own adapter, and the set of file managers keeps changing
(Omarchy is replacing Nautilus with Files; Flea was a contender)
([report, "One daemon and thin adapters"](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#one-daemon-and-thin-adapters-solve-the-many-file-managers-problem);
README, Omarchy section).

The consensus architecture across Dropbox, ownCloud/Nextcloud, Insync and wusel is that a
daemon owns state and exposes a local socket, and thin per-file-manager plugins query it and
render ([Nextcloud socketapi.cpp](https://raw.githubusercontent.com/nextcloud/desktop/master/src/gui/socketapi/socketapi.cpp),
[wusel status protocol](https://wusel.itbh.at/wusel/latest/explanation/status-protocol.html),
[file_manager_integration.md, section 3](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).
Because Nautilus extensions cannot hook file activation, hydration must happen below the
file manager, and adapters become purely cosmetic and command-issuing
([wusel desktop integration](https://wusel.itbh.at/wusel/0.4.0/explanation/desktop-integration.html)).
The research concluded the UI must be a separate optional process from the daemon so the
toolkit is swappable
([file_manager_integration.md, section 4](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).

On Omarchy the graphical session runs under a populated systemd user session (via uwsm), so a
systemd user unit can start the daemon (local inspection, recorded in
[file_manager_integration.md, section 5](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).

## Decision

my-drive runs as separate processes, as listed in the overview's Processes table:

| Process | Binary | Role |
|---|---|---|
| Daemon | `mydrived` | Sync engine, FUSE mounts, state store, IPC server |
| CLI | `mydrive` | Full control surface |
| UI | `mydrive-ui` (later) | Onboarding, settings, activity; Qt6/QML |
| Adapters | per host | Draw status and menu entries; send commands; no state, no sync logic |

- The daemon runs as a systemd user service, one instance per user. All of that user's
  accounts and mounts live inside the one daemon (ADR-0007).
- The daemon is the only process that opens the state store (ADR-0006) or the backing
  directories. The CLI, UI, tray and adapters are clients of the daemon's socket (ADR-0009)
  and touch neither.
- **CLI parity.** Anything the UI or an adapter can do, the CLI can do. Every action is a
  socket method; the CLI, UI and adapters all call the same methods.
- The Rust code is one Cargo workspace with the crates listed in the overview's Crates table.
  Crate dependencies point downwards only, exactly as in the overview's crate diagram:
  `mydrive-cli` depends on `mydrive-ipc`; `mydrive-daemon` on `mydrive-ipc`, `mydrive-vfs`
  and `mydrive-engine`; `mydrive-engine` on `mydrive-store`, `mydrive-graph` and
  `mydrive-core`; `mydrive-vfs`, `mydrive-ipc`, `mydrive-store` and `mydrive-graph` on
  `mydrive-core`; `mydrive-core` on no workspace crate. `mydrive-graph-sim` is a dev-only
  dependency.
- Each crate respects the "Must not" column of the overview's Crates table.

## Alternatives considered

**A single GUI process that owns sync (Nextcloud desktop style).** One binary is simpler to
package and the UI can read state directly. Rejected: the toolkit becomes part of the sync
engine, the client cannot run headless, and a UI crash stops syncing or interrupts a write.

**A library embedded in each file manager.** Each file-manager plugin links a sync library and
talks to Graph itself. Rejected: several processes would compete for one state store and one
throttling budget, hydration would only work inside file managers, and terminal and editor
access would not work at all, contradicting principle 4.

## Consequences

- The UI toolkit can be replaced, or omitted, without touching the engine.
- The daemon runs on headless machines and in sessions with no tray; the CLI is enough.
- A crashing UI or adapter cannot corrupt sync state, because it holds none.
- Adapters are small and written in the host's language (ADR-0003, ADR-0014).
- Every feature needs a socket method before any client can use it, and the CLI must expose
  it. This adds work to each feature but gives scripts and tests one entry point.
- systemd is required for the supported way of running the daemon. Distributions without a
  systemd user session need another launcher, which is out of scope for now.
- **Open concern:** the overview's diagram labels all adapters as separate processes. They
  are separate from the daemon, but a nautilus-python extension and a Dolphin plugin run
  inside their file manager's process
  ([file_manager_integration.md, section 1](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).
  Such adapters must never block the host's UI thread on a socket call; they answer from a
  cache fed by the change stream (ADR-0009).

## Rules for implementers

- **R1.** Only `mydrived` opens `$XDG_STATE_HOME/mydrive/state.db` or any path under
  `$XDG_DATA_HOME/mydrive/mounts/`. No other binary, script or adapter in the repository
  references these paths.
- **R2.** Every operation offered by the UI or any adapter is a socket method, and the CLI has
  a command that calls it. A test enumerates the IPC method catalogue and fails if a
  user-facing method has no CLI command.
- **R3.** CI checks the direct dependencies between workspace crates (from `cargo metadata`)
  against the edges in the overview's crate diagram and fails on any edge not in it. In
  particular `mydrive-cli` depends directly on `mydrive-ipc` only, and no crate depends on
  `mydrive-daemon` or `mydrive-cli`.
- **R4.** `mydrive-graph-sim` appears only in `[dev-dependencies]`.
- **R5.** The daemon ships as a systemd user unit; one daemon per user. Starting a second
  `mydrived` for the same user fails with a clear error and does not touch the store.
- **R6.** Adapters hold no persistent state of their own beyond a cache of daemon answers,
  and contain no sync logic.

## Revisit when

- A target distribution or session type without a systemd user manager becomes a priority.
- A host file manager offers a native sync-provider interface that requires in-process state.
