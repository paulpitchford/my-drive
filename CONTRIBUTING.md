# Contributing

This project is at the foundations stage. Nothing is released and interfaces will change.

- Architecture and vocabulary: [docs/architecture/overview.md](docs/architecture/overview.md)
- Decisions: [docs/adr/](docs/adr/)
- How work is done: [docs/process/development-workflow.md](docs/process/development-workflow.md)
- What "done" means: [docs/process/definition-of-done.md](docs/process/definition-of-done.md)
- What is planned: [docs/backlog/](docs/backlog/)

## Setup

```sh
mise install     # pinned Rust, just and cargo-deny
just check       # format check, clippy, tests, dependency audit, docs check
```

Runtime needs `fuse3` and a Secret Service provider such as gnome-keyring.

## Changes

One work package, one branch, one pull request. Use the pull request template. Changes to
ADRs or the architecture overview go in a pull request of their own.

The project is licensed GPL-3.0-or-later. Code taken from another project must be under a
GPL-3.0-compatible licence, with its origin recorded in the file and in `NOTICE` (ADR-0002).
