# WP-008: Graph client foundations

- **Status:** ready
- **Milestone:** M0
- **Wave:** 3
- **Depends on:** WP-001, WP-007, WP-009
- **Gates:** —
- **Owned paths:** `crates/mydrive-graph/src/client/**`, `crates/mydrive-graph/src/model/**`,
  `crates/mydrive-graph/src/lib.rs`, `crates/mydrive-graph/Cargo.toml`,
  `crates/mydrive-graph/tests/client_*.rs`
- **ADRs:** ADR-0011, ADR-0010
- **Flows / journeys:** F02, F03, F05, F10

## Goal

A typed client for the read side of Microsoft Graph that follows every rule in ADR-0011, so
the engine never handles raw HTTP and cannot forget a rule.

## In scope

- A client with an injectable base URL and an injectable token provider (trait from WP-009).
- Response types for drive, drive item and delta page that tolerate what real responses omit:
  no cTag on business drives, missing hashes, the `deleted` facet, `remoteItem`, the root
  item. Drive IDs are normalised through `DriveId` at this boundary.
- Delta: fetch a page from a root or from a saved link; return items plus either a next link
  or a delta link.
- Download: request a byte range of an item's content and report what actually arrived:
  the requested range (206), or the whole file (200). Follow a 302 to a different host
  without forwarding the `Authorization` header to it; send the bearer token otherwise.
- Error mapping to a typed error: unauthorised, forbidden (with the error code string
  preserved for policy detection), not found, gone-needs-resync (both documented variants),
  throttled (with `Retry-After` as a duration), server error, transport error, malformed
  response.
- The `ISV|<company>|<app>/<version>` User-Agent on every request.

## Out of scope

- Sleeping or retrying on throttling: the client reports it; the scheduler (M1) decides.
- Upload (M1). Sharing links, Teams, sites (M1).

## Acceptance criteria

- AC1: Paging a multi-page delta from the simulator yields every item and a delta link;
  resuming from the link yields only later changes.
- AC2: A ranged download against a simulator that ignores `Range` is reported as "whole
  file", never as the requested range.
- AC3: In redirect mode the `Authorization` header is not sent to the redirected URL
  (asserted from the simulator's request log); in direct mode it is sent.
- AC4: A 429 or 503 with `Retry-After` maps to the throttled error carrying that duration;
  the client itself does not retry.
- AC5: A 401 causes exactly one token refresh through the provider and one retry; a second
  401 is returned as unauthorised.
- AC6: A truncated body is reported as an error, never as a short successful read.
- AC7: Tokens and pre-authenticated URLs never appear in `Debug` output or log events.
- AC8: Both `410` resync variants map to distinct error values.

## Required tests

- All against `mydrive-graph-sim`, using its fault queue for AC4, AC5, AC6.
- A test per ADR-0011 rule implemented here, doc-commented with the rule.

## Notes

WP-009 creates this crate's `lib.rs` and manifest in wave 2; this WP owns them in wave 3.

## Open questions

None.
