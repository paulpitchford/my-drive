# ADR-0019: Protocol version 1 paginates large results and declares which value sets are open

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

Amends ADR-0009: its rule R7 ("every child in a single response") is replaced by R1 here.
Everything else in ADR-0009 stands.

## Context

Review of the first protocol implementation (WP-005) measured about 226 bytes per item in an
`item.status` result. ADR-0009 R7 required a directory's whole listing in one response, and
the framing limit is 1 MiB per line, so a directory of about 4,600 or more children could not
be answered at all. A folder of 6,000 photos is ordinary.

The same review found two things that cannot be fixed after version 1 is released, because
ADR-0009 R5 tells clients to ignore unknown fields:

- Pagination added later would be silently ignored by a version 1 client, which would cache a
  partial folder as complete.
- The protocol did not say whether sets such as mount state may gain values within a major
  version. A client that fails on an unknown value would break on a routine daemon upgrade.

It also found that one error code, `not-ready`, covered both a daemon that is still starting
(retry) and a client that skipped `hello` (a bug; retrying never helps).

## Decision

**Pagination.** Any result that lists items may be split. A request may carry `cursor`; a
result carries `next_cursor` when more remains and omits it when complete. A client repeats
the call with the same parameters plus the cursor until `next_cursor` is absent, and treats
the listing as complete only then. The daemon sizes each page to fit the line limit and
returns entries in a stable order (by name within a directory; by request order for a batch
of paths). This applies to `item.status` for a directory and for a batch of paths. The
purpose of ADR-0009 R7 is unchanged: an adapter makes one call sequence per directory, never
one call per file.

**Line limit.** 1 MiB per line in both directions. The daemon never emits a longer line.

**Closed and open value sets.** In a major version:

- *Closed* (a new value requires a new major version): the hydration, pin and sync axes and
  the display status.
- *Open* (values may be added): mount state, account state, account kind, modifiers, error
  codes, refusal reasons, blocked reasons and capabilities.

A client must accept an unknown value of an open set without failing the response. The
documented fallbacks: an unknown mount or account state is treated as "not active" and shown
by its word; an unknown modifier, capability or reason is ignored or shown as text.

**Modifiers** are a list of kebab-case strings (`["read-only", "shortcut"]`, usually empty).

**Reasons are codes.** Wherever the protocol gives a reason a program might act on (a refusal,
a blocked mount, an item in `error`), it is a kebab-case code from an open set, with a
separate human-readable message. Programs never parse messages.

**Sequence errors.** `not-ready` means only that the daemon is still starting. A request sent
before `hello` gets a dedicated `hello-required` error.

**Change stream.** The daemon closes a subscribed connection that falls too far behind rather
than buffering without bound. A client treats any close as: reconnect, re-subscribe,
re-request what it is showing. A `status.changed` notification carries changed entries and,
separately, a list of directories the client should re-request (used when a change is too
large to enumerate, such as a rename of a folder with many descendants).

**Item actions.** The result for each requested path may carry `skipped` (descendants that
were not acted on, each with an error) and `bytes_freed`. Dehydrate is decided per file
before the response is sent; pin and hydrate are accepted when recorded.

## Alternatives considered

**A larger or asymmetric line limit.** Raising the daemon-to-client limit moves the ceiling
without removing it, and makes every client buffer more.

**A streaming response (several lines per request).** Breaks JSON-RPC's one-response-per-
request model and is harder for shell and Python adapters than a cursor loop.

**Leave value sets unspecified.** Guarantees a breaking change the first time a state is
added.

**Modifiers as an object of booleans.** Also additive, but larger per item and inconsistent
with how capabilities and reasons are expressed.

## Consequences

- Every client needs a cursor loop for listings. It is a few lines in any language.
- Rust client types for open sets carry an "other" variant, so they cannot be matched
  exhaustively without a fallback arm.
- The catalogue in `docs/protocol/README.md` must state, for every enumerated field, whether
  its set is closed or open.
- ADR-0009 R7 no longer applies as written.

## Rules for implementers

- **R1.** A directory or batch too large for one line is returned across several calls using
  `cursor` and `next_cursor`; a listing of 50,000 items is retrievable in full, in a stable
  order, with no entry missing or repeated when nothing changes between calls.
- **R2.** No message the daemon emits exceeds 1 MiB as one line.
- **R3.** `next_cursor` is absent on the last page and present on every earlier page.
- **R4.** A client type for an open set deserialises an unknown value without error and
  preserves it; a client type for a closed set rejects an unknown value.
- **R5.** A request before `hello` is answered with `hello-required`; `not-ready` is used only
  while the daemon is starting.
- **R6.** Every reason a program may act on is a kebab-case code with a separate message.
- **R7.** The catalogue marks every enumerated field as closed or open, and a test checks the
  marking against the types.

## Revisit when

- Adapters show that cursor loops are a practical problem.
- A closed set needs a new value before a new major version is otherwise justified.
