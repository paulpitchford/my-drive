# WP-011: Gate experiments G1 and G5

- **Status:** ready
- **Milestone:** M0
- **Wave:** 4
- **Depends on:** WP-008, WP-009
- **Gates:** answers G1 and G5
- **Owned paths:** `crates/mydrive-graph/examples/g1_*.rs`,
  `crates/mydrive-graph/examples/g5_*.rs`, `crates/mydrive-graph/tests/g1_*.rs`,
  `crates/mydrive-graph/tests/g5_*.rs`, `docs/gates/G1.md`,
  `docs/gates/G5.md`
- **ADRs:** ADR-0005, ADR-0010, ADR-0011
- **Flows / journeys:** F01, F03

## Goal

Answer the two unknowns that decide how hydration and headless sign-in are built, against the
real service: whether `Range` is honoured on authenticated `/content` requests (G1), and
whether device-code sign-in works for personal accounts (G5).

## In scope

- An example program that signs in with read-only scope (`Files.Read offline_access`, plus `openid`: see the
  ruling below) using
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
- AC3: The report (standard output) and the gate documents contain no token, no
  pre-authenticated URL and no account identifier; they record only the kind of account
  (personal or work). The confirmation prompt below is on the terminal (standard error) and is
  not part of the report.
- AC4: The gate documents are filled in from a real run by the owner, for a personal account
  at minimum; a work-account run is recorded as pending if no tenant is available.

## Required tests

- As AC1.

## Notes

Running against a real account needs the owner to sign in themselves. The app is registered
and its client ID is the compiled-in default. The registration lists `Files.ReadWrite.All`,
not `Files.Read`; the examples still request only `Files.Read offline_access`, which the
identity platform lets a user consent to without its being listed. If the service refuses
that, record it in the gate document and stop; do not widen the scope. The implementer builds and tests
against the simulator and stops there; AC4 is completed with the owner.

Lead's ruling, 2026-11-15. The owner's one address is both a personal and a work account, and
the address reaches a tenant where the owner is only a guest. So each example also requests
`openid`, which grants no access to Graph or to files and is the only documented way to learn
the tenant at sign-in (the `tid` claim of the ID token; ADR-0010 and Q37 already expect the
product to read it). The example decodes `tid` for display only, maps the fixed consumer
tenant to "personal" and anything else to "work", prints the kind and the tenant ID to
standard error, and sends no request to Graph until the user types `yes`. The kind is never
typed by the user. "Do not widen the scope" above means no further Graph permission.

## Open questions

None.
