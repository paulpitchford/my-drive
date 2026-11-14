# WP-007: Graph simulator core

- **Status:** ready
- **Milestone:** M0
- **Wave:** 2
- **Depends on:** WP-001, WP-004
- **Gates:** —
- **Owned paths:** `crates/mydrive-graph-sim/**`
- **ADRs:** ADR-0017, ADR-0011
- **Flows / journeys:** F02, F03, F05

## Goal

Build the fake Microsoft Graph server that every engine and client test will run against.
It must reproduce the behaviours that break sync clients, not an idealised API, because tests
are only as good as the simulator's faithfulness.

## In scope

- An in-process HTTP server (`axum` on `tokio`) started from a test on an ephemeral
  localhost port, returning a handle with its base URL and a control API.
- A model of drives and items: IDs, names, parents, sizes, timestamps in whole seconds,
  eTags, cTags (present for personal drives, absent for business drives), quickXorHash from
  `mydrive-core`, and file content.
- Endpoints, shaped as Graph v1.0 returns them: get drive; get item by ID; list children
  with paging; `delta` at a drive root with `@odata.nextLink` paging and a final
  `@odata.deltaLink`; deleted items reported with the `deleted` facet; an item changed twice
  appearing twice in one feed; no `path` in `parentReference` for delta.
- Content download in two modes selectable per drive: a 302 redirect to a pre-authenticated
  URL, or direct content requiring the bearer token. `Range` support selectable: honoured
  (206) or ignored (200 with the whole body).
- Bearer-token checking: a missing or unknown token gets 401.
- Control API for tests: create, modify, rename, move and delete items as if another device
  did it; set modes; and a scripted fault queue (next matching request returns a given
  status with optional `Retry-After`, is delayed, or has its body truncated).
- A request log tests can assert on (method, path, headers of interest).

## Out of scope

- Upload, upload sessions, If-Match conflicts, 410 resync, throttling model, SharePoint
  post-upload rewriting, sharing links, socket.io notifications: simulator WPs in M1.
- Any use outside tests.

## Acceptance criteria

- AC1: A test can start the simulator, build a tree, and page through a full delta to a
  delta link; calling the delta link again returns only what changed since.
- AC2: Delta responses for a business drive contain no cTag, and no response contains a path
  in `parentReference`.
- AC3: A rename of a folder appears in delta as that folder only, not its descendants.
- AC4: With Range honoured, a ranged request returns 206 and exactly those bytes; with Range
  ignored, the same request returns 200 and the whole body.
- AC5: In redirect mode the pre-authenticated URL works without a token; in direct mode the
  content request without a token gets 401.
- AC6: A scripted fault applies to exactly the next matching request and then clears.
- AC7: Two simulators can run in the same test process without interfering.
- AC8: The crate's README lists every behaviour simulated and its source in the research
  notes, and every known Graph behaviour deliberately not yet simulated.

## Required tests

- Self-tests for each criterion using a plain HTTP client.

## Notes

JSON shapes should be taken from the Microsoft Graph documentation cited in
`research_notes/OneDrive Linux on demand clients/graph_api_and_windows_parity.md`. Where the
research marks a behaviour as unverified, simulate both possibilities behind a switch rather
than picking one.

## Shared files

This WP may change `Cargo.lock`, and only as a result of dependencies it adds to its own
crate from `[workspace.dependencies]`. Other wave-2 WPs do the same in parallel; the lead
resolves `Cargo.lock` conflicts at merge. Do not edit the root `Cargo.toml`.

## Open questions

None.
