# Backlog

Work is divided into **work packages** (WPs). One WP is sized for one agent session and
becomes one branch and one pull request. See `docs/process/development-workflow.md`.

Statuses: `draft` (not fully specified), `ready`, `blocked` (says on what), `in progress`,
`done`.

## Milestone M0: Foundations

Outcome: the pure-logic core, the protocol types, the state store, a Graph simulator and a
Graph client exist and are tested; the riskiest unknowns (gates G1 and G5, FUSE in CI) are
answered. No daemon yet.

| WP | Title | Wave | Depends on | Status |
|---|---|---|---|---|
| [WP-001](WP-001-core-identifiers.md) | Core identifiers | 1 | — | done (#4) |
| [WP-002](WP-002-core-state-model.md) | Core state model | 1 | — | done (#3) |
| [WP-003](WP-003-core-name-validation.md) | Name and path validation | 1 | — | done (#6) |
| [WP-004](WP-004-core-quickxorhash.md) | quickXorHash | 1 | — | done (#5) |
| [WP-010](WP-010-fuse-smoke-test-in-ci.md) | FUSE smoke test in CI | 1 | — | done (#2) |
| [WP-005](WP-005-ipc-protocol-types.md) | IPC protocol types and framing | 2 | WP-001, WP-002 | done (#10) |
| [WP-006](WP-006-store-schema.md) | State store schema v1 | 2 | WP-001, WP-002 | done (#12) |
| [WP-007](WP-007-graph-simulator-core.md) | Graph simulator core | 2 | WP-001, WP-004 | done (#13) |
| [WP-009](WP-009-graph-auth.md) | Sign-in and token storage | 2 | — | done (#16) |
| [WP-012](WP-012-architecture-conformance-checks.md) | Architecture conformance checks | 2 | WP-010 | done (#9) |
| [WP-008](WP-008-graph-client-foundations.md) | Graph client foundations | 3 | WP-001, WP-007, WP-009 | ready |
| [WP-013](WP-013-wave-2-test-follow-ups.md) | Wave 2 test follow-ups | 3 | WP-005, WP-006, WP-009 | ready |
| [WP-011](WP-011-gate-experiments-g1-g5.md) | Gate experiments G1 and G5 | 4 | WP-008, WP-009 | blocked on wave 3 and on the owner (app registration) |

Waves 1 and 2 are merged. What they taught is in `docs/process/orchestration.md`
("Lessons from wave 1", "Lessons from wave 2"). Wave 3 needs the owner's go-ahead.

## Validation gates

From `docs/architecture/overview.md`. A gate is an experiment, not a code deliverable.

| Gate | How it is answered | Needs | Status |
|---|---|---|---|
| — FUSE in hosted CI | WP-010 | — | **answered: yes** (`docs/process/ci-fuse.md`) |
| G1 Range on authenticated `/content` | WP-011 against a real personal account, then a work account | App registration; owner signs in | open |
| G2 Policy errors and shortcut stubs | Manual experiment against a work tenant with unmanaged-device controls | A work tenant; owner | open |
| G3 abraunegg FUSE ADR | Lead reads PR #3600 and its ADR, writes up findings | Nothing | open |
| G4 Per-file status in Files | Issue on omacom/folio | Owner approves the wording before it is posted | open |
| G5 Device-code sign-in for personal accounts | WP-011 | App registration; owner signs in | open |

## Later milestones (outline, not yet specified)

These become WPs when the milestone before them is nearly done, so they can use what was
learned.

**M1 Engine**: request scheduler per tenant; delta application into the store; download
manager with ranges; simple and session upload with If-Match; conflict handling; simulator
support for upload sessions, 410 resync, throttling and SharePoint rewriting; socket.io
notifications; sharing-link and Teams resolution for libraries; multi-mount supervision;
daemon lifecycle and IPC server; CLI commands; blocked and read-only detection.

**M2 On-demand**: FUSE filesystem over the backing directory; hydrate on read; write path
and quiet period; pin and dehydrate; xattr mirror; crash-recovery scan; mass-delete guard;
crawler deny list and circuit breaker; thumbnail seeding; crash-injection test harness.

**M3 Omarchy**: Files plugin; tray; notifications; systemd user unit; AUR packages.

**M4 Onboarding**: first-run UI; library picker; settings; activity view.

**M5 Breadth**: shortcuts; site search; Nautilus/Nemo/Caja, Dolphin and yazi adapters; deb
and rpm; Intune broker sign-in.
