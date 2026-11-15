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
| [WP-008](WP-008-graph-client-foundations.md) | Graph client foundations | 3 | WP-001, WP-007, WP-009 | done (#23) |
| [WP-013](WP-013-wave-2-test-follow-ups.md) | Wave 2 test follow-ups | 3 | WP-005, WP-006, WP-009 | done (#20, #21, #22) |
| [WP-011](WP-011-gate-experiments-g1-g5.md) | Gate experiments G1 and G5 | 4 | WP-008, WP-009 | blocked on the owner (app registration) |

Waves 1 to 3 are merged. What they taught is in `docs/process/orchestration.md`. Wave 4
(WP-011) needs the owner to register the Microsoft app. Milestone M1 is next to be specified.

## Validation gates

From `docs/architecture/overview.md`. A gate is an experiment, not a code deliverable.

| Gate | How it is answered | Needs | Status |
|---|---|---|---|
| — FUSE in hosted CI | WP-010 | — | **answered: yes** (`docs/process/ci-fuse.md`) |
| G1 Range on authenticated `/content` | WP-011 against a real personal account, then a work account | App registration; owner signs in | open |
| G2 Policy errors and shortcut stubs | Manual experiment against a work tenant with unmanaged-device controls | A work tenant; owner | open |
| G3 abraunegg FUSE ADR | Lead reads PR #3600 and its ADR, writes up findings | Nothing | **answered: pass with changes** ([docs/gates/G3.md](../gates/G3.md)); ADR-0005 to be amended before M2 |
| G4 Per-file status in Files | Issue on omacom/folio | Owner approves the wording before it is posted | open |
| G5 Device-code sign-in for personal accounts | WP-011 | App registration; owner signs in | open |

## Milestone M1: Engine

Outcome: sign in; mirror the metadata of a personal drive and one library into the store; the
CLI can list, hydrate, export and import single files. No mounted folder yet. Started
2026-11-15 with the owner's go-ahead. The plan and the lead's rulings are in
[drafts/M1-plan.md](drafts/M1-plan.md); packages are written one wave ahead, against the code
as it then stands. Rows marked `planned` have no file yet.

| WP | Title | Wave | Depends on | Status |
|---|---|---|---|---|
| [WP-014](WP-014-m1-shared-file-preparation.md) | M1 shared-file preparation | 5 | — | done |
| [WP-015](WP-015-core-clock-and-conflict-names.md) | Clock and conflict-copy names | 6 | WP-014 | ready |
| [WP-016](WP-016-ipc-protocol-m1-methods.md) | Protocol methods for M1 | 6 | WP-014 | ready |
| [WP-017](WP-017-store-schema-v2.md) | State store schema version 2 | 6 | WP-014 | ready |
| [WP-019](WP-019-sim-write-side.md) | Simulator write side | 6 | WP-014 | ready |
| [WP-023](WP-023-sim-identity-and-discovery.md) | Simulator identity and discovery | 6 | WP-014 | ready |
| 018 | Store schema version 3: pending operations | 7 | 017 | planned |
| 025 | Graph client write side | 7 | 019 | planned |
| 029 | Engine foundations and facade | 7 | 015, 017 | planned |
| 030 | Request scheduler | 7 | 015 | planned |
| 041 | IPC server and client | 7 | 016 | planned |
| 020 | Simulator upload sessions | 8 | 019 | planned |
| 021 | Simulator delta model 2 (resync, ancestors, packages) | 8 | 019 | planned |
| 022 | Simulator throttling model | 8 | 023 | planned |
| 027 | Graph client identity and discovery | 8 | 023 | planned |
| 026 | Graph client upload sessions | 9 | 020, 025 | planned |
| 031 | Engine delta apply | 9 | 017, 029, 030 | planned |
| 032 | Engine accounts and sign-in | 9 | 027, 029, 030 | planned |
| 034 | Engine download | 9 | 029, 030 | planned |
| 043 | CLI | 9 | 016, 041 | planned |
| 033 | Engine mount manager | 10 | 031, 032 | planned |
| 035 | Engine upload | 10 | 018, 025, 026, 029, 030 | planned |
| 036 | Engine conflicts | 11 | 018, 031, 035 | planned |
| 037 | Engine resync | 11 | 021, 031, 033 | planned |
| 039 | Engine libraries | 11 | 027, 033 | planned |
| 042 | Daemon | 11 | 041, 032, 033, 034, 035 | planned |
| 038 | Engine recovery | 12 | 035, 036, 037 | planned |
| 044 | M1 acceptance | 12 | all | planned |

Deferred out of M1: change notifications over the push protocol (plan packages 024, 028,
040), until the protocol has been observed on a real account. No wave has more than five
packages or more than two foundation packages.

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
