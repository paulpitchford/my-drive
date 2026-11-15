# ADR-0023: The daemon reaches the store and Microsoft Graph only through the engine facade

- **Status:** Accepted
- **Date:** 2026-11-15
- **Depends on gates:** none

Makes explicit something the crate diagram (ADR-0004) already forces.

## Context

The crate diagram lets `mydrive-daemon` depend on the engine, the IPC crate, the VFS and core,
and not on the store or the Graph client. Several documents nevertheless described the daemon
reading the store to answer status requests and driving sign-in directly. The store is a
single connection with one writer, and sign-in, token storage and every Graph call need the
per-tenant scheduler. If the daemon reached past the engine there would be two owners of each.

## Decision

`mydrive-engine` exposes one facade type, `Engine`. It is the only way the daemon (and, in M2,
the VFS through core traits) causes or observes anything in the store, the backing
directories, the token store or Microsoft Graph.

- The facade's parameter and result types are built from `mydrive-core` types and plain data.
  No store or Graph type appears in it, apart from any deliberately re-exported and listed.
- Sign-in attempts, account and mount management, listing for the protocol's paged status
  calls, hydration, import and export, pause and resume, and the change-event stream are all
  facade methods.
- The engine owns the store connection on one thread; everything else sends it work.
- The facade's signatures are fixed early in M1, with bodies that report "not available yet",
  so the daemon and the engine modules can be built in parallel against a stable contract.
  Changing a signature afterwards is the lead's decision.

## Alternatives considered

**Let the daemon read the store directly for status.** Faster to write, but breaks the
single-owner rule and the crate diagram.

**A separate service crate between daemon and engine.** Adds an edge to the diagram for no
gain; the facade is that layer.

## Consequences

- The daemon is thin: it maps protocol methods to facade calls and facade events to
  notifications.
- End-to-end tests live in the daemon crate and drive the socket with the IPC client.
- A facade that is wrong is expensive, because every engine package builds against it; it is
  reviewed as a foundation.

## Rules for implementers

- **R1.** `mydrive-daemon` and `mydrive-cli` name no type from `mydrive-store` or
  `mydrive-graph`.
- **R2.** Only the engine opens the state store, and it does so on one owning thread.
- **R3.** The facade's public signatures contain no store or Graph type other than those
  listed as re-exported in the engine's crate documentation.
- **R4.** A change to a facade signature after it is frozen is made only by the lead.

## Revisit when

- The VFS in M2 needs a read path the facade cannot serve fast enough.
