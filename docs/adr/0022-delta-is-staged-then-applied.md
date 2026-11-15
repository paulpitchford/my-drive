# ADR-0022: A delta run is staged durably, then applied when its delta link arrives

- **Status:** Accepted
- **Date:** 2026-11-15
- **Depends on gates:** none

Refines ADR-0011 R2 ("use the last occurrence of an item within a delta feed") by fixing how
the engine achieves it.

## Context

Flow F02 applied each delta page to the store as it arrived; flow F05 accumulated a feed and
applied last occurrences. They cannot both be right. Applying page by page can act on an
occurrence that a later page of the same feed supersedes, for example deleting an item that a
later page restores, which ADR-0011 R2 forbids. Accumulating in memory loses the feed on a
crash and gives a resync (ADR-0011 R3) no durable "fresh set" to reconcile against.

## Decision

Every delta run, whether the first enumeration, an incremental run or a resync, has two
phases.

1. **Stage.** Each page's entries are written to a staging table keyed by mount and item ID,
   in the same transaction as the link to the next page. A later entry for the same item
   replaces the earlier one. Nothing in the item tables changes.
2. **Apply.** When the page carrying the delta link arrives, the staged set is applied to the
   item tables, parents before children and deletes last, and then the staged rows are
   removed and the delta link becomes the mount's live token, together.

A run interrupted in the stage phase resumes from the stored next link, or restarts if that
link is refused. A run interrupted in the apply phase is applied again from the staged set;
applying a staged entry is idempotent. The live token moves only when a run has been fully
applied.

Staged entries that cannot be used (a problem entry from the Graph client) are staged as
problems and resolved during apply by fetching the item; they are never guessed.

## Alternatives considered

**Apply each page as it arrives.** Least code, but violates ADR-0011 R2 across pages.

**Accumulate in memory.** Correct ordering, but a crash loses the feed, memory grows with the
drive, and resync has nothing durable to compare with.

## Consequences

- First enumeration of a large drive writes every item twice (staged, then applied).
- The store gains a staging table and run-state columns on mounts (schema version 2).
- Resync becomes an ordinary run plus an anti-join for items the server no longer reports.
- F02 and F05 are amended to this model.

## Rules for implementers

- **R1.** A page's staged entries and its next link are committed together or not at all.
- **R2.** Within a run, a later entry for an item replaces the staged earlier one; the apply
  phase sees one entry per item.
- **R3.** No item row changes during the stage phase.
- **R4.** The mount's live delta token changes only in the same transaction that removes the
  staged set after a complete apply.
- **R5.** Applying a staged entry twice gives the same result as applying it once.
- **R6.** A staged problem entry is resolved by fetching the item or recorded as an error on
  it; it is never applied as a best guess.

## Revisit when

- Enumeration cost for very large drives makes double-writing a problem in practice.
