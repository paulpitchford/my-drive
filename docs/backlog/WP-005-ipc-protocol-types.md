# WP-005: IPC protocol types and framing

- **Status:** done (#10)
- **Milestone:** M0
- **Wave:** 2
- **Depends on:** WP-001, WP-002
- **Gates:** —
- **Owned paths:** `crates/mydrive-ipc/**`, `docs/protocol/**`
- **ADRs:** ADR-0009, ADR-0019, ADR-0020, ADR-0004, ADR-0008
- **Flows / journeys:** F11, J17

## Goal

Define the wire protocol between the daemon and every client as Rust types plus a written
catalogue, so the daemon, CLI and adapters can be built in parallel against one contract.

## In scope

- JSON-RPC 2.0 envelope types: request, success response, error response, notification.
- Pagination, size limits and open value sets as decided in ADR-0019 and ADR-0020 after
  review (cursors, `limits` in `hello`, every message bounded).
- A newline-delimited framing codec over any byte stream: one JSON document per line, a
  maximum line length of 1 MiB, typed errors for oversize lines, invalid UTF-8 and invalid
  JSON. The codec must not lose or merge messages when reads split or join lines.
- `hello`: parameters (client name, supported protocol versions) and result (chosen version,
  capability list). Protocol version constant `1`.
- An error-code table as an enum with stable integer codes: not-ready, unknown-path,
  not-in-a-mount, refused (with reason), needs-sign-in, blocked-by-policy, read-only,
  offline, unsupported-version, invalid-params, internal.
- Parameter and result types for the version 1 catalogue: `daemon.status`, `account.list`,
  `mount.list`, `item.status` (a batch of paths, or one directory and its children),
  `item.pin`, `item.unpin`, `item.hydrate`, `item.dehydrate`, `status.subscribe`, and the
  `status.changed` notification.
- `docs/protocol/README.md`: the catalogue, with a JSON example of every request, response,
  error and notification, and the versioning rules.

## Out of scope

- The socket server and client (M1). This WP is types and codec only.
- `account.*` and `mount.*` mutation methods (sign-in, add mount): M1, once flows are built.

## Acceptance criteria

- AC1: Every example in `docs/protocol/README.md` is parsed by a test into the matching type
  and serialises back to equivalent JSON.
- AC2: The codec delivers messages intact whatever the read boundaries are.
- AC3: A line over the limit yields a typed error and the stream can continue with the next
  line.
- AC4: Item state in the protocol uses the three axes and the derived display status and
  modifiers from `mydrive-core`, with the same string forms.
- AC5: Unknown fields in incoming messages are ignored; unknown methods produce the JSON-RPC
  "method not found" error. Both are tested.
- AC6: Paths in the protocol are absolute paths; a relative path is rejected with
  invalid-params.

## Required tests

- Property test for AC2: arbitrary messages, arbitrary split points.
- A test per ADR-0009 rule.

## Notes

Adapters will be written in Python, shell and Lua. Keep the JSON plain: no tagged-enum
shapes that are awkward outside Rust, snake_case field names, strings for enums.

## Shared files

This WP may change `Cargo.lock`, and only as a result of dependencies it adds to its own
crate from `[workspace.dependencies]`. Other wave-2 WPs do the same in parallel; the lead
resolves `Cargo.lock` conflicts at merge. Do not edit the root `Cargo.toml`.

## Open questions

None.
