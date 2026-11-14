# ADR-0001: Record architecture decisions as ADRs

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

my-drive is a side project that will be built largely by agents working from written work
packages and reviewed independently (ADR-0018). Nothing is built yet. Decisions taken now
(mechanism, state model, licence, language) will be read by implementers and reviewers who
were not present when they were made, and who may be working months later.

Three things are needed:

- A durable record of why each decision was made, including the alternatives rejected and
  the research behind it, so a later reader does not re-open a settled question by accident
  or keep a decision after its premise has failed.
- A stable, citable form for the rules that code must obey, so tests and reviews can point
  at a specific rule.
- One index of what has been decided.

`docs/architecture/overview.md` is the keystone document: it fixes vocabulary, structure,
paths and the state model, and its Decisions table lists every ADR. ADRs give the detail
behind each row. The research behind the early ADRs is in
[the report](../../reports/OneDrive%20Linux%20on%20demand%20clients.md) and the
[research notes](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/).

## Decision

We record each architecture decision as an Architecture Decision Record (ADR), one Markdown
file per decision, in `docs/adr/`.

- **Template.** Every ADR follows `docs/adr/0000-template.md` exactly: the header lines
  (Status, Date, Depends on gates) and the sections Context, Decision, Alternatives
  considered, Consequences, Rules for implementers (omitted only if there are none) and
  Revisit when.
- **Numbering and file names.** ADRs are numbered sequentially with four digits, starting at
  0001. Numbers are never reused. The file name is the number followed by a kebab-case form
  of the title, for example `0005-fuse-over-backing-directory.md`.
- **Status values.** `Proposed`, `Accepted`, or `Superseded by ADR-NNNN`.
- **Immutability.** Once an ADR is Accepted, its text does not change, except to fix typos and
  to update the Status line. A changed decision is written as a new ADR that states which ADR
  it supersedes; the old ADR's Status becomes `Superseded by ADR-NNNN`.
- **Authority.** Only the project lead creates, accepts or supersedes ADRs. Changes are made
  by pull request. Agents and other contributors may draft an ADR for the lead, but a draft
  has no force until the lead merges it as Accepted.
- **Index.** The Decisions table in `docs/architecture/overview.md` is the index of ADRs. A
  pull request that adds or supersedes an ADR updates that table in the same pull request. If
  an ADR has to disagree with the overview, the overview is changed in the same pull request
  and the ADR says why, as the overview itself requires.
- **Rules for implementers.** Rules are numbered R1, R2, ... within each ADR. Each rule is
  concrete enough to be checked by a test or a reviewer. Tests and review comments cite a
  rule as `ADR-NNNN RN`, for example `ADR-0005 R3`. Rule numbers inside an Accepted ADR never
  change.
- **Evidence.** Factual claims in Context link to the research report, a research note, or
  the original source. Claims that were not verified are marked as unverified. Where a
  decision rests on a fact the research could not confirm, the ADR names the validation gate
  (G1, G2, ...) from the overview in its header.

## Alternatives considered

**No formal records; decisions live in the overview only.** The overview is short by design
and states what, not why. Without the reasoning and rejected alternatives, a later reader
cannot tell whether a premise has changed.

**Decisions in issues or pull request threads.** Searchable only through the hosting service,
mutable, and mixed with discussion. They give no stable rule numbers for tests to cite.

**Mutable design documents edited in place.** Simpler to keep current, but the history of
why a decision changed is lost, and tests citing a rule can silently end up pointing at
different text.

## Consequences

- Every significant decision costs a short document before code. This is accepted overhead.
- Superseded ADRs stay in the repository, so the directory grows; the overview's Decisions
  table is the current view.
- Tests and reviews can cite rules precisely, and a rule cannot change under a test that cites
  it.
- Only the lead can change a decision, which makes the lead a bottleneck for architectural
  change. This is intended for a project of this size.

## Rules for implementers

- **R1.** Every file in `docs/adr/` other than `0000-template.md` has a name matching
  `NNNN-kebab-case-title.md`, contains the template's header lines and sections in the
  template's order, and has a number not used by any other file.
- **R2.** Every ADR file appears as a row in the overview's Decisions table, and every row in
  that table has a matching ADR file.
- **R3.** A change to an Accepted ADR other than a typo fix or its Status line is rejected in
  review; the change must be a new ADR that supersedes it.
- **R4.** A test that exists to enforce an ADR rule names it in the form `ADR-NNNN RN` in the
  test name or in a comment next to the test.
- **R5.** A pull request that supersedes an ADR also sets the old ADR's Status to
  `Superseded by ADR-NNNN` and updates the overview's Decisions table.

## Revisit when

- The project gains maintainers other than the lead, so that a single approver no longer
  fits.
- The overview and the ADR set drift apart repeatedly despite R2, which would suggest the
  index should be generated rather than maintained by hand.
