# WP-012: Architecture conformance checks

- **Status:** done (#9)
- **Milestone:** M0
- **Wave:** 2
- **Depends on:** WP-010 (both change `.github/workflows/ci.yml`)
- **Gates:** —
- **Owned paths:** `scripts/**`, `justfile`, `.github/workflows/ci.yml`
- **ADRs:** ADR-0004, ADR-0017, ADR-0003
- **Flows / journeys:** —

## Goal

Make the structural rules the ADRs state fail the build when broken, so that with many agents
writing code in parallel the architecture cannot drift without anyone noticing.

## In scope

- A check, run by `just check` and CI, that reads `cargo metadata` and fails if any workspace
  crate directly depends on another workspace crate along an edge not drawn in the crate
  diagram in `docs/architecture/overview.md`. The allowed edges are parsed from that diagram,
  not duplicated in the script. `mydrive-graph-sim` may be a dev-dependency of any crate and
  a normal dependency of none; its own edge to `mydrive-core` is drawn in the diagram.
- A check that every crate root except `mydrive-vfs` contains `#![forbid(unsafe_code)]`.
- A report listing every "Rules for implementers" item in `docs/adr/` (as `ADR-NNNN Rn`) with
  the tests that name it, and those with none. Report only: it does not fail the build yet.

## Out of scope

- Making the rule-coverage report a failing check (lead decides when, during M1).

## Acceptance criteria

- AC1: Adding a dependency from `mydrive-store` to `mydrive-graph` makes the check fail and
  name the edge.
- AC2: Removing `#![forbid(unsafe_code)]` from a crate root makes the check fail.
- AC3: Changing the diagram in the overview changes what the check allows, with no script
  change.
- AC4: The coverage report lists every rule in every ADR and finds a rule named in a test doc
  comment.
- AC5: The scripts have tests of their own, run by `just check`.

## Required tests

- As the criteria; use temporary fixture workspaces, not edits to the real tree.

## Notes

Python standard library only, like `scripts/check_docs.py`.

## Open questions

None.
