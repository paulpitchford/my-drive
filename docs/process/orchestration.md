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

## Lessons from wave 1

Wave 1 (five WPs, 2026-11-14) merged in one sitting. What to keep and what to change:

- **The lead's WP text is the weakest link.** Three of five WPs had errors in them: a wrong
  count, a test dependency the crate did not have, and guidance ("take the stricter reading")
  that review showed was harmful. Implementers reported each one instead of working around
  it, which is the process working. Before a wave, re-read each WP's criteria against the
  crate's actual manifest and the ADRs.
- **Verifiers earn their place by breaking the code.** Mutation checks found two tests that
  could not fail (a test looping over the constant it was testing; a documented behaviour
  with no test) and a CI step that could not pass. Keep "break it and see the test fail" as
  the core of verification.
- **Reviewers find design hazards, not just defects.** Both useful fixes (own pin overriding
  a stale effective pin; ASCII-only lowercasing) were non-blocking findings. Read
  non-blocking findings; fix the cheap ones before merge.
- **Re-verify after a ruling changes behaviour.** A fix round that only adds a guard can be
  read by the lead; one that changes rules goes back to a verifier.
- **Anything observable only in CI needs a CI run before review ends.** Push the branch and
  open the pull request as soon as the implementer reports, not after review.
- **Practicalities.** Reviewers and verifiers cannot check out a branch that is checked out
  in another worktree: tell verifiers to `git checkout --detach <branch>` and reviewers to
  use `git diff master...<branch>`. Tell agents to run `mise trust` if asked, to delete
  `proptest-regressions` files left by deliberate breaks, and to confirm no FUSE mount is
  left behind. Remove agent worktrees and their branches after the wave.
- **Cost.** Five WPs used 5 implementers, 5 reviewers and 6 verifiers, plus 5 resumed
  implementer rounds. Plan on about three agents and one fix round per WP.

## Scale

Start small. The first wave is the test of this process as much as of the code: run it with
a handful of agents, fix what the process got wrong, then widen. Number of parallel agents is
limited by how many non-overlapping WPs are ready, not by how many agents are available.
