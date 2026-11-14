# WP-008: Graph client foundations

- **Status:** done (#23)
- **Milestone:** M0
- **Wave:** 3
- **Depends on:** WP-001, WP-007, WP-009
- **Gates:** —
- **Owned paths:** `crates/mydrive-graph/src/client/**`, `crates/mydrive-graph/src/model/**`,
  `crates/mydrive-graph/src/lib.rs` (module declarations and re-exports only),
  `crates/mydrive-graph/Cargo.toml`, `crates/mydrive-graph/tests/client_*.rs`; and in the
  simulator, only what the fault extension below needs: `crates/mydrive-graph-sim/src/fault.rs`,
  the lines of `server.rs`/`json.rs` that build a fault's response, its self-test and README
  row. `Cargo.lock` as a result of this crate's dependencies. Not `src/auth/**` (WP-013
  works there in parallel).
- **ADRs:** ADR-0011, ADR-0010
- **Flows / journeys:** F02, F03, F05, F10

## Goal

A typed client for the read side of Microsoft Graph that follows every rule in ADR-0011, so
the engine never handles raw HTTP and cannot forget a rule.

## In scope

- A client with an injectable base URL and an injectable token provider: the existing
  `auth::TokenProvider` trait (`access_token()` and `force_refresh(rejected)`).
- A small additive extension to the simulator's scripted faults, needed to test error
  mapping: a status fault may carry a chosen Graph error `code` string and an optional
  `Location` header (the documented `410 Gone` resync responses carry both). Default
  behaviour of existing faults is unchanged.
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

As merged (three review rounds), the client also: sends `Accept-Encoding: identity` and
refuses encoded downloads; takes the item's expected size and checks every download against
it; returns a result the caller must match (ranged or whole file); distinguishes `Truncated`
(head verified, bytes may be kept) from `SizeMismatch` (discard and refetch metadata);
returns delta pages as one ordered list of items and problem entries; and never shows raw
server strings in errors. Its module docs list what the engine must still do.

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
- AC5: A 401 causes exactly one `force_refresh` of the rejected token through the provider
  and one retry; a second 401 is returned as unauthorised. A provider error (needs sign-in,
  transient, closed) is returned as its own typed error, never retried in a loop.
- AC6: A truncated body is reported as an error, never as a short successful read.
- AC7: Tokens and pre-authenticated URLs never appear in `Debug` output or log events.
- AC8: Both documented `410` resync variants (`resyncChangesApplyDifferences` and
  `resyncChangesUploadDifferences`) map to distinct error values carrying the restart link
  from the `Location` header when present; an unknown 410 code maps to a third, conservative
  value.
- AC9: Item IDs are kept exactly as received (no case folding); drive IDs go through
  `DriveId`. A response whose JSON has unknown fields or lacks optional ones still parses.
- AC10: Every simulator switch that changes a response shape the client reads is exercised:
  both download modes, `Range` honoured and ignored, both folder-delete modes, both repeat
  modes, hashes present and absent, `parentReference.path` present and absent.

## Required tests

- All against `mydrive-graph-sim`, using its fault queue for AC4, AC5, AC6.
- A test per ADR-0011 rule implemented here, doc-commented with the rule.

## Notes

The crate already contains sign-in (`src/auth/`, WP-009). Add the client beside it; do not
change `src/auth/`. The simulator's README lists every switch and what is not simulated yet
(upload, throttling model, sharing links are M1): do not build client features for those.

## Open questions

None.
