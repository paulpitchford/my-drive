# Review checklist

For reviewers. You are given the diff, the work package and the ADRs it names. Form your own
view from those; do not rely on the author's description of what the change does.

Report findings in order of severity. For each: file and line, what is wrong, a concrete
scenario in which it goes wrong, and which rule or criterion it breaks. Say plainly if you
found nothing.

## 1. Data safety (blocking)

- Can any sequence of events (crash, restart, network loss, concurrent local and remote
  change, rename during upload) lose, corrupt or silently discard user data?
- Can a placeholder ever be read as zeros or as an empty file?
- Can a file with unsynced local changes be dehydrated, overwritten or deleted?
- Can removing a mount, signing out or a resync cause a remote delete?
- Is state in the store still consistent if the process dies between any two statements?

## 2. Correctness against the spec (blocking)

- Is each acceptance criterion in the WP actually met? Trace it through the code.
- Does the code follow each "Rules for implementers" item in the ADRs the WP names?
- Does behaviour match the flows and journeys the WP names, including their failure paths?
- Are items identified by (`drive_id`, `item_id`) and never by path?

## 3. Tests (blocking)

- Does each criterion and each ADR rule have a test, and would that test fail if the
  behaviour were removed?
- Are failure paths tested, or only the happy path?
- Any test that sleeps, uses real time, needs the network, or depends on order?

## 4. Boundaries (blocking)

- Changes outside the WP's owned paths?
- A crate doing something its row in the overview says it must not?
- New dependencies, `unsafe`, `unwrap`/`expect` outside tests?
- Anything secret logged or written to disk?

## 5. Quality (non-blocking unless severe)

- Names and vocabulary match the overview.
- Errors carry enough context to diagnose from a log.
- Simpler way to do the same thing; duplicated logic that already exists elsewhere.
- Blocking calls on async paths; unbounded queues, buffers or retries.

## Verdict

End with one of:

- **Approve**: no blocking findings.
- **Changes required**: list the blocking findings.
- **Escalate to lead**: the WP or an ADR is wrong, ambiguous or contradicts another document.
