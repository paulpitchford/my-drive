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

## Lessons from wave 2

Wave 2 (five WPs, 2026-11-14) took far more rounds than wave 1: the protocol four, the state
store five, sign-in four, the simulator three, the conformance checks two. That was the right
outcome for what they are, and it changes how to plan.

- **Foundation packages need adversarial review, and more than one round of it.** Every
  review of the store and the protocol found a real problem the previous round had not, until
  one did not. Plan two or three review rounds for anything other code will trust: a wire
  contract, a store, a test double, credential handling.
- **Ask the reviewer the question you most fear the answer to.** The most useful briefs were
  "try to fool this check", "list every public operation and what it can do to a dirty item",
  "is this converging or should responsibility move elsewhere", and "review as an attacker".
- **Verification passing is not review passing.** Verifiers repeatedly confirmed that code did
  what it was asked while reviewers showed that what it was asked was not enough. Both are
  needed; neither substitutes for the other.
- **Model-based and mutation testing by the verifier is the strongest evidence available.**
  Random operation sequences against stated invariants (800,000 steps on the store), random
  kills, and planted defects found things example tests never would, including a secrets test
  that missed a planted leak 99 times in 100.
- **Put invariants where they cannot be bypassed.** The store stopped losing unsynced content
  only when "has unsynced content" became an explicit column with CHECK constraints, instead
  of something each operation inferred. Prefer a constraint to a convention.
- **A ruling can be wrong; say so and rule again.** ADR-0019 fixed pagination and still left
  an index cursor that skipped files; ADR-0020 corrected it. Three rulings in wave 2 reversed
  the lead's own earlier text.
- **Stale verifications waste time.** Several verifiers reported on a tip that had already
  moved. Do not push to a branch while a verifier is running on it unless the change is
  tests-only; tell the verifier the commit to check and have it say which commit it checked.
- **Lockfile conflicts are routine.** Every wave-2 branch conflicted in `Cargo.lock` once
  another merged. Resolve by taking master's lockfile and letting `cargo check` add the
  branch's entries, then run `just check`.
- **Licence and audit configuration is the lead's to change**, and such changes go to the
  owner afterwards: two were needed (Zlib allowed; audit limited to Linux targets).
- **Leave non-blocking gaps in a follow-up WP** rather than another round, once a final
  verification passes (WP-013).
- **Cost.** Five WPs used roughly 45 agent runs: 5 implementers resumed 17 times, 11 reviews
  and 17 verifications. Budget eight to ten agent runs for a foundation package.

## Lessons from wave 3

Wave 3 (WP-008 and the three parts of WP-013, 2026-11-14 to 15) confirmed the split the
wave-2 lessons predicted: the three tests-only parts each took one verification and merged;
the Graph client took three review rounds.

- **Tests-only follow-up packages are cheap and worth doing.** One of them stopped on a real
  defect (a forged cursor accepted) instead of working around it; another proved an existing
  test could not fail. Split them by crate so they run in parallel.
- **A reviewer with a raw socket finds what a simulator cannot.** Both blocking download
  findings (compressed bodies passed off as content; a cut-off body with no length reported
  as complete) needed a hand-written TCP server. For anything that parses a network protocol,
  tell the reviewer and verifier to build one.
- **Error kinds are an API contract when the caller's reaction differs.** "Cut off" and
  "wrong version" need opposite reactions from the engine; giving them one label would have
  caused silent corruption. Where two failures need different handling, make the distinction
  a documented guarantee and test each side.
- **Leniency must never guess.** Tolerant parsing stopped one bad item stalling sync, and then
  returned a deleted item as not deleted. The rule that fixed it: an item that cannot be read
  unambiguously becomes a flagged entry, never a best guess.
- **Resume the verifier that found the problem.** The final verification of the client reused
  the agent that had built the probes; it re-ran all of them in one pass.
- **Read the last small diff yourself.** After a final verification leaves two or three narrow
  items, a tests-plus-few-lines commit can be read by the lead and merged on green CI.
- **Scratch builds belong on disk, not in `/tmp`.** A verifier's parallel build loop filled the
  tmpfs and produced false failures. Tell agents to use `/var/tmp` for build copies.
- **Agents leave wait-loops behind.** Two finished agents left shells polling forever (one
  with `pgrep -f`, which matches itself). Tell agents to `wait` on PIDs and check for strays
  after a wave.
- **Cost.** About 20 agent runs: 4 implementers resumed 6 times, 2 reviews, 6 verifications.

## Scale

Start small. The first wave is the test of this process as much as of the code: run it with
a handful of agents, fix what the process got wrong, then widen. Number of parallel agents is
limited by how many non-overlapping WPs are ready, not by how many agents are available.
