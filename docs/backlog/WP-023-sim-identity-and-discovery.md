# WP-023: Simulator identity and discovery

- **Status:** blocked on WP-014
- **Milestone:** M1
- **Wave:** 6
- **Depends on:** WP-014
- **Gates:** — (its guesses about policy errors and read-only signals stay provisional until G2)
- **Owned paths:** `crates/mydrive-graph-sim/src/identity.rs`, `src/authority.rs`,
  `src/discovery.rs`, `src/lib.rs`, `src/server.rs`,
  `crates/mydrive-graph-sim/tests/identity_*.rs`, `tests/discovery_*.rs`,
  `crates/mydrive-graph-sim/docs/identity.md`, `crates/mydrive-graph/tests/sim_identity_*.rs`
- **ADRs:** ADR-0010, ADR-0015, ADR-0017
- **Flows / journeys:** F01, F09, J01, J11, J13

## Goal

Engine and daemon tests need to sign in and discover drives without a real account. The fake
authority used by the sign-in tests lives inside another crate's test files and cannot be
reused. Give the simulator its own authority and the identity and discovery endpoints.

## In scope

- A fake authority: an authorise step a test can drive, a token endpoint checking the PKCE
  verifier against the challenge, refresh with a rolled refresh token, `invalid_grant` after a
  test revokes a token, and ID tokens carrying tenant, object ID and user name claims.
  Personal and work accounts.
- `/me`, `/me/drive`, `/organization`.
- Resolving a sharing link (`/shares/{encoded}/driveItem`), recording which `Prefer` value was
  sent; access to the target drive becomes durable only when the link is redeemed.
- Drive details needed to name a mount (site and library names for a library).
- Tenants as a registry grouping accounts and drives, readable by later simulator packages.
- Libraries that are blocked (every route answers `403` with a configurable code) and
  libraries that are read-only (writes refused), both documented as provisional guesses.

## Out of scope

- Teams and channels; site search. Throttling. Change notifications.

## Acceptance criteria

- AC1: A code redeemed with the wrong verifier is refused; with the right one it yields
  tokens whose ID token carries the account's claims.
- AC2: Refresh returns a new refresh token and the old one keeps working or not according to
  a switch (default: old one refused); a revoked token answers `invalid_grant`.
- AC3: A link resolved with `redeemSharingLinkIfNecessary` does not make the drive reachable
  by a later request for that drive; `redeemSharingLink` does.
- AC4: A blocked library answers `403` with the configured code on every route while another
  drive of the same tenant works.
- AC5: The existing loopback and paste sign-in flows in `mydrive-graph` complete against this
  authority: one test each, in `crates/mydrive-graph/tests/sim_identity_*.rs`. The simulator
  may not depend on `mydrive-graph`, even for tests (the architecture check refuses it).
- AC6: A personal account's `/me/drive` is a personal drive; a work account's is a business
  drive; drive IDs follow the mixed-case switch if one exists.
- AC7: No token the authority issues is written to the request log in full.
- AC8: `docs/identity.md` gives the source for each behaviour and marks the blocked and
  read-only signals as unverified pending gate G2.

## Required tests

- Self-tests per criterion.

## Notes

Shared files, as WP-014 left them: `identity::routes()` and `discovery::routes()` are already
merged into the Graph router. `model.rs` is owned by WP-019 in this wave: do not edit it.
Keep accounts, tenants, tokens, codes and links in state structs defined in your own three
files and held by `Shared` in `server.rs` behind their own lock, beside the existing
`State`; never hold both locks at once unless you document and test one fixed order. Export
your public types from `lib.rs`; leave the line directly below `pub use model::{...};` free,
because WP-019 adds one `pub use write::{...};` line there. The authority is not
wired: it needs its own listener, started in `lib.rs`, and its own router in `server.rs`,
with the request log applied and AC7 holding. You own both files in this wave; keep the
existing listeners, routers and middleware order as they are.

Blocked and read-only libraries are checked in `server.rs`, in a layer in front of every
drive route, so that WP-019's write handlers (built in parallel) need no change: a read-only
library refuses any request whose method writes. Say in `docs/identity.md` where the layer
sits relative to the token check and the fault layer, and why.

Existing tokens: the simulator today accepts one fixed bearer token per instance. Tokens the
authority issues must be accepted by the Graph origin too, and the fixed token must keep
working, because every existing test uses it.

The dev-only rule (ADR-0017 R1) and the architecture check allow the simulator as a
dev-dependency of other crates, not the reverse; check before adding any dependency here.

## Open questions

None.
