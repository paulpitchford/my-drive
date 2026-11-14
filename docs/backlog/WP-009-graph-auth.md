# WP-009: Sign-in and token storage

- **Status:** blocked on wave 1
- **Milestone:** M0
- **Wave:** 2
- **Depends on:** —
- **Gates:** — (G5 decides later whether device-code is added)
- **Owned paths:** `crates/mydrive-graph/src/auth/**`, `crates/mydrive-graph/src/lib.rs`,
  `crates/mydrive-graph/Cargo.toml`, `crates/mydrive-graph/tests/auth_*.rs`
- **ADRs:** ADR-0010
- **Flows / journeys:** F01, J01, J14

## Goal

Sign a user in through their own browser and keep their tokens safe, with a headless variant
that works over SSH. Sign-in is the most fragile area in existing Linux clients; this must be
boring and correct.

## In scope

- PKCE verifier and challenge generation; `state` generation and checking.
- Building the authorisation URL for a given client ID, authority, scopes and redirect URI.
- Loopback flow: listen on `127.0.0.1` on an ephemeral port, accept exactly one redirect,
  validate `state`, return the code, show a minimal "you can close this tab" page, and stop
  listening. A timeout ends the wait.
- Paste flow: parse a pasted redirect URL, validate `state`, extract the code or the error.
- Token exchange and refresh against an injectable authority base URL.
- A `TokenProvider` trait (get a valid access token; force a refresh) and an implementation
  that refreshes before expiry and serialises concurrent refreshes into one request.
- A `TokenStore` trait with an in-memory implementation and a Secret Service implementation
  over `zbus`.
- A "needs sign-in" outcome when refresh is rejected, distinct from a transient failure.
- Token types whose `Debug` output is redacted.

## Out of scope

- Device-code flow (after gate G5). Intune broker sign-in (M5).
- Opening the browser (the CLI does that) and any user interface.
- The real client ID: the default is a placeholder constant until the owner registers the app.

## Acceptance criteria

- AC1: The loopback flow completes against a fake authority in tests and returns tokens.
- AC2: A redirect with a wrong or missing `state` is rejected and no token request is made.
- AC3: An error redirect (`error=access_denied`) is returned as a typed error with the
  description.
- AC4: The paste flow accepts the full redirected URL and rejects a URL for a different
  redirect URI.
- AC5: Ten concurrent callers needing a refresh cause exactly one refresh request.
- AC6: A rolled refresh token replaces the stored one before the new access token is handed
  out; if storing fails, the error is returned and the old token is not discarded.
- AC7: `invalid_grant` on refresh produces "needs sign-in"; a network failure does not.
- AC8: No token value appears in `Debug` output, error messages or log events.
- AC9: The listener binds only to the loopback interface.

## Required tests

- A fake authority (token endpoint) local to this WP's tests.
- The Secret Service implementation cannot be tested in CI. One `#[ignore]` test exercising
  a real keyring is permitted; say how to run it. This is the only permitted ignored test.
- A test per ADR-0010 rule implemented here.

## Notes

Never an embedded web view (ADR-0010). Scopes and authority are parameters; this WP does not
hard-code them beyond documented defaults.

## Open questions

None.
