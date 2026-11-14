# my-drive

A OneDrive client for Linux with Windows-style Files On-Demand. Rust workspace, GPL-3.0-or-later.

## Read before changing anything

1. `docs/architecture/overview.md`: components, vocabulary, state model, paths. Authoritative.
2. The ADRs your work touches, in `docs/adr/`. Their "Rules for implementers" are binding.
3. Your work package in `docs/backlog/`, and the flows and journeys it names.
4. `docs/process/development-workflow.md` and `docs/process/definition-of-done.md`.

`README.md` and `reports/` hold the research behind the decisions. Read them for the why.

## Commands

Tools are pinned in `mise.toml`. Run `mise install` once.

| Command | Does |
|---|---|
| `just check` | Everything CI runs: format check, clippy, tests, dependency audit, docs check |
| `just fmt` | Format |
| `just lint` | Clippy with warnings denied |
| `just test` | All tests |
| `just deny` | Licence and advisory audit |
| `just docs-check` | Broken links and dangling ADR/journey/flow/WP references |

If `cargo` is not on your PATH, prefix with `mise exec --`.

## Rules

Data safety comes first. A change that could lose, corrupt or silently discard a user's
file is wrong even if every test passes.

- **Stay inside your work package.** Change only its owned paths. If the job needs a change
  elsewhere, stop and report it.
- **Do not change ADRs or `docs/architecture/overview.md`.** If a decision looks wrong,
  ambiguous or impossible, stop and report. Do not work around it.
- **Use the vocabulary in the overview** exactly: mount, drive, item, placeholder, hydrate,
  dehydrate, pin, and the state names.
- **Respect crate boundaries.** Dependencies point downward only (see the overview's crate
  table for what each crate must not do).
- **No new dependencies** beyond `[workspace.dependencies]` without the lead's approval.
- **No `unsafe`** outside `mydrive-vfs`. No `unwrap`, `expect`, `todo!` or `dbg!` outside
  tests. `thiserror` in libraries, `anyhow` only in binaries.
- **Every ADR rule you implement gets a test that names it**, for example a test doc comment
  `/// ADR-0012 R3`.
- **Tests never touch the real Microsoft service.** Use `mydrive-graph-sim`.
- **Never commit credentials, tokens or account data.** Never put a real account name, email
  or tenant in code, tests, docs or commit messages.
- **Report honestly.** If something is unfinished, untested or uncertain, say so in the PR
  under "Not done / follow-ups".

## Conventions

- Branch: `wp/<id>-<slug>`. One work package, one branch, one pull request.
- Commits: Conventional Commits (`feat(engine): ...`, `fix(vfs): ...`, `docs(adr): ...`).
- Errors carry context: what was being done, to which item, why it failed.
- Log with `tracing`. Never log tokens, file contents or full URLs with embedded auth.
- Public items have doc comments. Comments explain why, not what.

## Needs the human owner

Stop and ask before: anything using a real Microsoft account, creating a release or tag,
opening issues or pull requests on other projects, changing the app registration, or
publishing anything outside this repository.
