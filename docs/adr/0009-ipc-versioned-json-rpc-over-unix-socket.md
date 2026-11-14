# ADR-0009: IPC is versioned JSON-RPC 2.0 over a Unix socket with a change stream

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

The daemon owns all state; the CLI, UI, tray and adapters are its clients (ADR-0004).
Adapters are written in Python (Nautilus/Nemo/Caja), C++ (Dolphin), Lua (yazi) and any
language for the Omarchy Files plugin, whose commands print JSON and whose `poll` hook runs at
most every 5 seconds (README, Omarchy section; [omacom/folio](https://github.com/omacom/folio)).

- Sockets in the established clients: Nextcloud serves a line-based text protocol at
  `$XDG_RUNTIME_DIR/<App>/socket`
  ([socketapi.cpp](https://raw.githubusercontent.com/nextcloud/desktop/master/src/gui/socketapi/socketapi.cpp));
  Dropbox's sockets live under `~/.dropbox/`, and its daemon fails to create them when the home
  directory is on a filesystem that cannot hold sockets (via search summary)
  ([file_manager_integration.md, sections 3 and 5](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).
- Nextcloud's status vocabulary (`OK`, `SYNC`, `NEW`, `IGNORE`, `ERROR`, ...) cannot express
  online-only versus local versus pinned. Its Nautilus script compares protocol versions as
  strings and blocks the UI for up to 100 ms waiting for menus
  ([syncstate.py](https://raw.githubusercontent.com/nextcloud/desktop/master/shell_integration/nautilus/syncstate.py)).
- wusel measured about 230 µs per single-file status query, about 46 ms for a cold 200-file
  folder, and uses a batch `enumerate` plus a change signal so adapters can cache
  ([wusel status protocol](https://wusel.itbh.at/wusel/latest/explanation/status-protocol.html)).
  It rejected FIFOs for lacking per-client connections and peer credentials.
- konedrive uses D-Bus (`org.konedrive.*`) between its daemon, GUI and CLI
  ([konedrive README](https://github.com/kartas39/konedrive)).
- The research recommended a richer versioned protocol with batch enumeration and a change
  stream, with the Nextcloud dialect as a compatibility endpoint at most
  ([report](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#one-daemon-and-thin-adapters-solve-the-many-file-managers-problem)).

## Decision

- **Transport.** A Unix stream socket at `$XDG_RUNTIME_DIR/mydrive/daemon.sock`. The daemon
  creates `$XDG_RUNTIME_DIR/mydrive/` with mode `0700` and the socket with mode `0600`.
- **Framing.** Newline-delimited JSON-RPC 2.0: each message is one JSON object on one line,
  terminated by `\n`.
- **Handshake.** The first request on every connection is `hello`, carrying the client's name
  and the list of protocol major versions it supports. The daemon replies with the chosen
  version (an integer major) and a list of capability strings. Any other request before a
  successful `hello` gets a JSON-RPC error response.
- **Compatibility.** Within one major version, changes are additive only: new methods, new
  optional parameters, new result fields, new notification types, new capabilities. Removing
  or changing the meaning of anything requires a new major version.
- **Methods.** Namespaced: `daemon.*`, `account.*`, `mount.*`, `item.*` (`item.status`,
  `item.pin`, `item.unpin`, `item.hydrate`, `item.dehydrate`), `activity.*`, and
  `status.subscribe`.
- **Batch status.** `item.status` accepts either a list of paths or one directory path; for a
  directory it returns the status of every child, so an adapter fetches a whole folder in one
  call.
- **Change stream.** `status.subscribe` turns its connection into a change stream: from then
  on the daemon sends JSON-RPC notifications for state changes. Adapters update their caches
  from it and never poll per file. A client that also needs to issue requests uses a second
  connection.
- **Paths.** Paths in the protocol are absolute paths under a mount point, never backing
  directory paths.
- **Catalogue.** The full method catalogue, parameters, results and notification shapes live
  in `docs/protocol/`, written alongside the implementation.
- **Out of scope for now.** A Nextcloud-socket-compatible endpoint.

## Alternatives considered

**D-Bus.** The desktop's standard IPC, with introspection, and used by konedrive. Not chosen:
the lead's assessment is that a session bus may be absent in minimal or headless sessions
(not verified in the research), and calling D-Bus from a shell-script plugin is awkward. The
daemon still uses D-Bus as a client for Secret Service, notifications and the tray
(ADR-0003).

**gRPC.** Typed and versioned, but heavy for a Python or shell adapter, needing generated
code and an HTTP/2 stack.

**Nextcloud's line protocol.** Would let a rebranded `syncstate.py` work at once, but its
status vocabulary cannot express our states, its framing is string-typed, and the licence of
the existing scripts was not verified.

**A REST server on localhost.** Easy from any language, but exposes a TCP port to every local
user and process, which a `0600` Unix socket does not.

## Consequences

- Any language with JSON and Unix sockets can be a client; shell plugins can call the CLI
  with `--json` instead of speaking the socket directly.
- File permissions on the socket are the access control: only the user's own processes can
  connect.
- Adapters keep a per-path cache primed by `item.status` on a directory and kept current by
  `status.subscribe`.
- We own the protocol and its documentation, and must keep each major version working for
  as long as we support it.
- Existing Nextcloud adapters cannot be reused unchanged.
- **Open concern:** JSON strings are Unicode, but Linux file names are arbitrary bytes. A
  local name that is not valid UTF-8 cannot be sent as a protocol path; such names need a
  rule (rejection by name validation is the likely answer, as OneDrive names are Unicode).

## Rules for implementers

- **R1.** The daemon listens only on `$XDG_RUNTIME_DIR/mydrive/daemon.sock`; the directory
  has mode `0700` and the socket `0600`. The daemon opens no TCP or UDP listener.
- **R2.** Every message, in both directions, is a single line of JSON terminated by `\n`,
  valid against JSON-RPC 2.0.
- **R3.** A request other than `hello` on a connection that has not completed `hello` receives
  a JSON-RPC error and has no effect.
- **R4.** `hello` with no supported major version in common receives an error that lists the
  versions the daemon supports.
- **R5.** Clients ignore unknown result fields, unknown notification methods and unknown
  capabilities. A contract test runs each client against a daemon response with extra fields
  added.
- **R6.** A test asserts that each released major version's request and response schemas are
  a subset of the current ones for that major version.
- **R7.** `item.status` on a directory returns one entry per child in a single response; no
  adapter in the repository calls `item.status` once per file of a listed directory.
- **R8.** Every path sent or accepted is absolute and under a mount point; a backing-directory
  path is rejected with an error.
- **R9.** Every method has an entry in `docs/protocol/` in the same pull request that adds it.

## Revisit when

- A target environment lacks `$XDG_RUNTIME_DIR`.
- Demand appears for reusing Nextcloud adapters (Thunar, Dolphin) through a compatibility
  endpoint.
- Change-stream volume becomes a measurable cost for adapters (for example during an initial
  delta of a large library).
