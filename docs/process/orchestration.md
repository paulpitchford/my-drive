# Orchestration playbook

For the lead: how to run a wave of work packages with agents. See
`development-workflow.md` for the process this implements.

## Before a wave

1. Pick the wave from `docs/backlog/README.md`. Check every WP in it is `ready`: its
   dependencies are `done`, its open questions are empty, its gates (if any) have passed.
2. Check owned paths do not overlap between WPs in the wave.
3. Prepare shared files so no WP needs to touch them: module stubs declared, dependencies
   present in `[workspace.dependencies]`.
4. Make sure `master` is green: `just check`.
5. Get the owner's go-ahead for the wave if it starts a new milestone.

## Briefing an implementer

Agent type `wp-implementer`, model Opus 5.5, isolated worktree. The brief is short because
the documents carry the detail:

```
Implement work package docs/backlog/WP-NNN-<slug>.md.
Branch: wp/NNN-<slug>.
Follow CLAUDE.md. Report as your agent definition describes.
```

Add anything the documents do not say: an answer to a question from a previous round, or
review findings to fix. Do not restate the WP.

## Briefing a reviewer and a verifier

Run both in parallel once the implementer reports done. Give them the branch name and the
WP path. **Do not pass on the implementer's summary**: they must reach their own view.

```
Review branch wp/NNN-<slug> against docs/backlog/WP-NNN-<slug>.md.
```

```
Verify branch wp/NNN-<slug> against docs/backlog/WP-NNN-<slug>.md.
```

## Handling results

| Result | Action |
|---|---|
| Reviewer approves, verifier passes, CI green | Merge; set WP `done` |
| Blocking findings | Send the findings verbatim to a fresh or resumed implementer |
| Reviewer and verifier disagree | Read the code yourself; decide; record why in the PR |
| Escalation: WP or ADR is wrong or unclear | Fix the document first, in its own PR, then resume |
| Three rounds without convergence | Take it over, or split or re-scope the WP |
| Implementer touched paths it does not own | Reject; re-brief with the boundary restated |

Treat an agent's "done, all tests pass" as a claim, not a fact. The verifier and CI are the
evidence.

## After a wave

1. `just check` on `master` with everything merged.
2. Update WP statuses and the wave table in `docs/backlog/README.md`.
3. Record anything learned that changes later WPs, and update them.
4. Report to the owner: what merged, what did not and why, what was learned, what is next.

## Scale

Start small. The first wave is the test of this process as much as of the code: run it with
a handful of agents, fix what the process got wrong, then widen. Number of parallel agents is
limited by how many non-overlapping WPs are ready, not by how many agents are available.
