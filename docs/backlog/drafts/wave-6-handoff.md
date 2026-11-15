# Wave 6 handoff (2026-11-15)

Work stopped mid-wave because the build machine had to be rebooted. Everything is pushed.
This note is what a new session needs to resume; delete it when wave 6 is merged.

## Read first

`CLAUDE.md`, `docs/process/orchestration.md`, `docs/backlog/README.md`, and the open
questions Q41 to Q43 in `docs/architecture/open-questions.md`.

## State of each package

| Package | Branch and tip | PR | State | Next step |
|---|---|---|---|---|
| WP-015 clock, conflict names | `wp/015-core-clock-and-conflict-names` `7a82b51` | #35 | Reviewed (pass with changes) and verified at `bcb414a`; fixes for every finding in `ee6110e` and `7a82b51`, `just check` exit 0 | Short re-review of the two fix commits, then merge |
| WP-016 protocol methods | `wp/016-ipc-protocol-m1-methods` `f606c12` | #41 | Built, `just check` exit 0. Not reviewed, not verified (both were interrupted) | Adversarial review (several rounds; the protocol freezes at release) and verification |
| WP-017 store schema v2 | `wp/017-store-schema-v2` `a9d3d33` | none | Interrupted work in progress, one `wip` commit. Not known to compile | A new implementer continues from the branch, reading the WP and the lead's points below |
| WP-019 simulator writes | `wp/019-sim-write-side` `872faeb` | #38 | Reviewed (pass with changes) at `8584eb0`; all seven findings fixed in `872faeb`, `just check` exit 0. Verification was interrupted | Verify `872faeb` from scratch; re-review the fix commit |
| WP-023 simulator identity | `wp/023-sim-identity-and-discovery` `d09fff9` | #37 | Reviewed and verified at `8e098f2`; nine review findings fixed in `d09fff9`, `just check` exit 0 | Two items left (below), then re-review and merge |
| WP-011 gates G1, G5 | `wp/011-gate-experiments-g1-g5` `7c44e43` | none | Barely started, one `wip` commit | A new implementer starts again from the WP |
| WP-045 gate G6 | not started | none | Specified | Start after WP-019 merges (its tests need the simulator's rewrite modes) |

## Left to do on WP-023

- Issued secrets that arrive percent-encoded (in a query value, a path or an `Authorization`
  header) are logged undecoded: mask after decoding.
- Add a test that a challenge sent without `code_challenge_method` is treated as `plain`
  (a mutant changing that default survived).
- Document that a verifier sent anywhere but the form body is logged (the simulator cannot
  recognise an arbitrary verifier), and that the README's index row still says two origins.

## Merge order and known interactions

- WP-019 and WP-023 both touch the simulator's `README.md`, `tests/self_tests.rs` and
  `lib.rs`. A trial merge was textually clean. Merge one, merge master into the other, run
  `just check`, then merge it.
- WP-023 keeps `Shared::new` with three arguments so WP-019's unit test compiles.
- WP-023 was based on an older master; merge master into it before the final check.

## Lead's points for WP-017 (from the implementer's brief)

`local_id` never reused across deletion of the highest row, a crash, `VACUUM` and the
migration; the version 1 fixture produced by the version 1 code path; the migration in one
transaction; existing durable facts preserved bit for bit; invariants as table constraints
with typed errors; the staged walk defined for orphans, cycles and the root, with bounded
memory; mount removal refusal bounded and with a typed "kept elsewhere" argument;
`children_after` in byte order; the existing model-based and crash tests extended.

## Rulings made this wave that are not yet in an ADR

- Q41: `anyhow` only in the daemon's `main.rs`.
- Q42: post-upload rewrites; interim ruling, to become an ADR amending ADR-0012 after G6.
- Pause and resume are `mount.pause` and `mount.resume` (ADR-0009 has no `sync.*`);
  `docs/backlog/drafts/M1-plan.md` still says `sync.pause`.

## Running the work

- The owner's dedicated build machine has 20 cores and 64 GB: a full wave (five implementers,
  or their reviewers and verifiers) can run in parallel there. On the original 8-core, 8 GB
  machine seven parallel Rust builds overloaded it; run at most two there. Give each agent
  its own `CARGO_TARGET_DIR`.
- The app registration is managed with a separate Azure CLI profile that exists only on the
  original machine. On another machine, sign in again with a profile kept apart from any
  work login. Nothing in the build needs it.
- The real runs of WP-011 and WP-045 need the owner to sign in. Agents never do.

## Waiting on the owner

Confirm D5 (what M1 lets a user do); confirm or reverse the Zlib licence allowance and the
Linux-only audit targets; review the decisions in `open-questions.md`; publisher
verification before a public release (Q22).
