# WP-011: Gate experiments G1 and G5

- **Status:** blocked on the owner (app registration)
- **Milestone:** M0
- **Wave:** 4
- **Depends on:** WP-008, WP-009
- **Gates:** answers G1 and G5
- **Owned paths:** `crates/mydrive-graph/examples/**`, `docs/gates/**`
- **ADRs:** ADR-0005, ADR-0010, ADR-0011
- **Flows / journeys:** F01, F03

## Goal

Answer the two unknowns that decide how hydration and headless sign-in are built, against the
real service: whether `Range` is honoured on authenticated `/content` requests (G1), and
whether device-code sign-in works for personal accounts (G5).

## In scope

- An example program that signs in with read-only scope (`Files.Read offline_access`) using
  the paste flow, picks a file over a given size, and requests a byte range of its content
  three ways: following the redirect, with the bearer token and redirects disabled, and with
  the header that opts into the post-April-2027 behaviour if one is documented. It prints
  status codes, relevant headers (never tokens or pre-authenticated URLs) and byte counts.
- A second example that attempts device-code sign-in and reports the outcome.
- `docs/gates/G1.md` and `docs/gates/G5.md`: method, date, account type, raw observations
  (redacted), conclusion, and what it means for the ADR.

## Out of scope

- Any write to the account. Any change to ADRs (the lead does that from the findings).

## Acceptance criteria

- AC1: The examples build and their argument parsing and output formatting are tested
  against `mydrive-graph-sim`.
- AC2: The examples request only read scopes and cannot modify the account.
- AC3: Output contains no token, no pre-authenticated URL and no account identifier.
- AC4: The gate documents are filled in from a real run by the owner, for a personal account
  at minimum; a work-account run is recorded as pending if no tenant is available.

## Required tests

- As AC1.

## Notes

Running against a real account needs the owner: they register the app, supply the client ID
through an environment variable, and sign in themselves. The implementer builds and tests
against the simulator and stops there; AC4 is completed with the owner.

## Open questions

- Client ID: pending the owner's app registration.
