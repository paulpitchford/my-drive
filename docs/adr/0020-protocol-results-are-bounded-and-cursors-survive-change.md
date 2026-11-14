# ADR-0020: Every protocol message is bounded, and cursors stay correct while a directory changes

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

Extends ADR-0019. Replaces its rule R1 (with R1 and R2 here) and widens its rule R2 from
`item.status` to every message. Changes one of its value-set decisions: the three state axes
become open on the wire. Everything else in ADR-0019 stands.

## Context

A second review of the protocol, after ADR-0019 was implemented, found three things that
would be frozen at release.

1. **Action results were unbounded.** "Free up space" on a folder of 7,000 files that are
   still uploading produces 7,000 `skipped` entries, about 1.2 MB: over the line limit. A
   cursor cannot fix this, because repeating an action has side effects. Dropping entries
   silently would repeat the defect ADR-0019 exists to prevent.
2. **The cursor was an index.** The reviewer listed 10,000 children, deleted the first after
   page one, and page two skipped an unchanged file. That file never appears in a change
   notification, because it did not change, so the client's view of the folder keeps a hole.
   A file added before the cursor appears twice. A cursor from one listing was accepted for
   another.
3. **`refresh` in `status.changed` did not say whether it covers subdirectories.** A client
   showing `/P/X/sub` when `/P/X` is renamed could keep stale state.

It also questioned closing the three state axes: adapters draw only from the display status
and modifiers, so a new sync step would break every strict client for no benefit.

## Decision

**Keyset cursors.** A directory listing is ordered by name, compared as bytes. The cursor
identifies the last name returned and the listing it belongs to; the next page starts after
that name. Consequently:

- An entry that exists, unchanged, for the whole cursor loop is delivered exactly once.
- An entry added, removed or renamed during the loop may or may not appear. A subscribed
  client learns of it from `status.changed`; a client that does not subscribe sees it on its
  next listing.
- A cursor never becomes stale because the directory changed.
- A cursor is bound to its request: used with a different directory or a different batch of
  paths, it is refused with `invalid-params`.

A cursor is an opaque, non-empty string. For a batch of paths, order is request order and the
cursor identifies the position in that batch.

**Every message is bounded.** No line in either direction exceeds 1 MiB. In addition to
pagination of listings:

- `hello` returns a `limits` object: the line limit and the maximum number of paths in one
  action request (100 in version 1). An action request with more paths is refused with
  `invalid-params`; the client splits the batch.
- In an action result, `skipped` is a sample, not a complete list: the daemon includes as
  many skipped descendants as fit, possibly none, and always includes `skipped_count`, the
  exact total. A client that needs the full picture lists the folder with `item.status`.
- Human-readable messages are capped at 1 KiB.
- A `status.changed` notification too large for one line is split across several, or
  replaced by a `refresh` entry.

**`refresh` covers the subtree.** For each path in `refresh`, the client discards what it has
cached for that path and everything below it, and re-requests what it is showing.

**The state axes are open on the wire.** Hydration, pin and sync may gain values within a
major version; a client keeps an unknown axis value and does not fail the response. The
display status stays closed: clients need an icon and a word for each one, and adding one is a
major-version change. Inside the daemon the axes remain the closed types of ADR-0008; this is
a property of the wire format only.

**Errors a program may act on carry a reason.** `blocked-by-policy` and `read-only` errors
carry a reason code from the same open sets used elsewhere. An action entry that has `error`
has no `skipped`.

## Alternatives considered

**Snapshot cursors** (the daemon remembers each in-progress listing). Correct, but makes the
daemon hold state per client and needs expiry rules. Keyset cursors need no state.

**A "listing changed, start again" error.** Simple, but a busy folder could never be listed,
and every client needs restart logic.

**Paginating action results.** Repeating the call would repeat the action.

**Raising the line limit for results.** Moves the ceiling without removing it.

## Consequences

- The daemon must be able to list a directory's children in name order starting after a given
  name. The store's index on (parent, name) serves this.
- Clients cannot assume `skipped` is complete; they use `skipped_count`.
- Client types for the axes need an "other" variant, like every other open set.
- ADR-0019 R1 is replaced, R2 is widened, and its closed-set list shrinks to the display
  status.

## Rules for implementers

- **R1.** While a directory is listed through a cursor loop, every entry that exists unchanged
  throughout is delivered exactly once, whatever is added, removed or renamed between pages.
- **R2.** A listing of 50,000 entries is retrievable in full in name order, with no page empty
  while more remains and no entry missing or repeated.
- **R3.** A cursor used with a different directory or a different batch of paths is refused
  with `invalid-params`.
- **R4.** No message, in either direction, of any kind, exceeds 1 MiB as one line: listings,
  action results, notifications and errors.
- **R5.** `hello` returns `limits`; an action request with more paths than the limit is
  refused with `invalid-params` and has no effect.
- **R6.** An action result always carries `skipped_count` when anything was skipped, and
  `skipped` holds at most that many entries, truncated to fit the line.
- **R7.** A client type for an axis keeps an unknown value without failing; a client type for
  the display status rejects one.
- **R8.** The catalogue states that `refresh` applies to each listed path and everything
  below it.

## Revisit when

- Adapters need the complete list of skipped items in one call.
- Name order by bytes proves a problem for clients that want locale order (they can sort).
