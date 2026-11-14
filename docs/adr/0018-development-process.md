# ADR-0018: Development process: work packages built by agents, reviewed independently, gated by CI

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

## Context

my-drive is a side project with one human owner, Paul. The code is to be written by AI agents
working from written plans. Two properties of agent work shape the process:

- An agent session has a bounded context, so work has to be cut into pieces that fit one
  session and can be checked on their own.
- An agent reviewing its own work tends to confirm it. A review is worth more when it comes
  from a different agent that sees the change and the rules, not the author's account of it.

The overview says the client must never lose or corrupt user data (principle 1), and the ADRs
carry the rules that protect that. The process has to keep those rules from being quietly
bent by whoever is implementing. Several actions reach outside the project and cannot be
undone by reverting a commit: publishing releases, creating the Microsoft Entra app
registration (ADR-0010), filing issues or pull requests on other projects (for example the
gate G4 request to Files), and anything that uses real Microsoft accounts (live tests,
ADR-0017).

## Decision

**Work packages.** Work is divided into work packages (WPs) in `docs/backlog/`. Each WP is
sized for one agent session and lists its goal, owned paths, dependencies, the ADRs that apply,
acceptance criteria and required tests. One WP is one branch, named `wp/<id>-<slug>`, and one
pull request.

**Roles.** All agents run on Claude Opus 5.5.

- The **lead** plans, writes ADRs and WPs, assigns WPs and merges pull requests.
- An **implementer** agent builds one WP in an isolated git worktree and changes only that WP's
  owned paths.
- A **reviewer** agent, different from the implementer, receives the diff, the WP and the
  relevant ADRs, and not the implementer's own summary.
- A **verifier** agent runs the build and the tests from a clean checkout of the branch.

**Merging.** CI must be green before merge. Nobody pushes directly to `master`.

**Decisions.** ADRs and `docs/architecture/overview.md` are changed only by the lead. An agent
that finds a decision wrong or ambiguous stops and reports it instead of working around it.

**Commits** follow Conventional Commits.

**Human approval.** Paul approves each milestone and anything outward-facing: releases, the
app registration, issues or pull requests on other projects, and anything that uses real
accounts.

**Done.** The definition of done is in `docs/process/definition-of-done.md`.

## Alternatives considered

**One long-running agent.** Fewer hand-offs, but its context fills, earlier decisions drift,
and nobody independent checks the result. Rejected.

**Humans writing code with agent assistance.** The project has one human with limited time;
this would make that time the bottleneck. Rejected.

**No independent review.** Faster, but leaves the implementer's reading of the ADRs unchecked,
which is where data-safety rules would be bent. Rejected.

## Consequences

- The lead's planning becomes the main cost and the main bottleneck: WPs must be specific
  enough that an implementer does not need to make design decisions.
- Owned paths must be cut so that WPs running at the same time do not touch the same files.
- An implementer that hits an ambiguity blocks its WP until the lead answers; this is slower
  but keeps decisions in the ADRs.
- The rule-to-test mapping in ADR-0017 gives reviewers a concrete checklist.
- `docs/backlog/`, `docs/journeys/` and `docs/process/definition-of-done.md` must exist before
  the first WP is assigned.

## Rules for implementers

- **R1.** Every pull request corresponds to exactly one WP and comes from a branch named
  `wp/<id>-<slug>`.
- **R2.** A pull request changes only paths listed as owned by its WP.
- **R3.** No pull request from an implementer, reviewer or verifier changes `docs/adr/` or
  `docs/architecture/overview.md`.
- **R4.** The reviewer of a pull request is not its implementer, and is given the diff, the WP and
  the ADRs listed in the WP.
- **R5.** A verifier report states the commit it built from a clean checkout and the test results.
- **R6.** No merge happens while CI is red, and `master` accepts no direct pushes.
- **R7.** Commit messages follow Conventional Commits.
- **R8.** No agent creates a release, changes the app registration, opens an issue or pull request
  on another project, or uses a real account without Paul's recorded approval.
- **R9.** An agent that finds an ADR wrong or ambiguous records the problem in its report and stops
  work on the affected part.

## Revisit when

- Reviews repeatedly miss defects that later tests or users find.
- WPs routinely overflow one session or collide on owned paths.
- The project gains more human contributors.
