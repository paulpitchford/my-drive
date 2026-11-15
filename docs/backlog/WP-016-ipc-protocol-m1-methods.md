# WP-016: Protocol methods for M1

- **Status:** blocked on WP-014
- **Milestone:** M1
- **Wave:** 6
- **Depends on:** WP-014
- **Gates:** —
- **Owned paths:** `crates/mydrive-ipc/src/methods.rs`, `values.rs`, `error.rs`, `paging.rs`,
  `crates/mydrive-ipc/tests/**`, `docs/protocol/**`
- **ADRs:** ADR-0009, ADR-0019, ADR-0020, ADR-0010, ADR-0023
- **Flows / journeys:** F01, F09, F11, J01, J11, J14, J15

## Goal

Protocol version 1 has ten methods and none for signing in, adding or removing a mount,
pausing, or moving a file in or out. Add what M1 needs now, while version 1 is unreleased and
can still change freely. After release only additions are possible.

## In scope

Types, parsing, the catalogue and its tests for:

- `account.login.start`: mode `browser` or `headless`; returns an attempt ID and the
  authorisation URL. `account.login.finish`: the attempt ID and, for headless, the pasted
  redirect URL; returns the signed-in account and a proposed default mount. It creates no
  mount. `account.logout`.
- `mount.add.preview`: the target is the account's own drive, a drive ID, or a sharing link;
  returns the drive's names and type, the item count if known, whether it is read-only, the
  proposed mount point, and a confirmation token. `mount.add.confirm`: the token and an
  optional mount point. `mount.remove`.
- `sync.pause` (all mounts or one; optional duration) and `sync.resume`.
- `item.import` (copy a local file into a folder of a mount, as a new file or replacing one)
  and `item.export` (copy a hydrated file out to a path), advertised by a capability string
  in `hello` so they can be withdrawn later.
- Additive fields: account user name and display name; mount drive type and item count.

## Out of scope

- The socket server and client (WP-041). Any engine behaviour.
- Teams browsing and site search. `activity.*` methods.

## Acceptance criteria

- AC1: Every new method is in the method list, parses, has a worst-case bound in the bounds
  test, and is refused before `hello` with `hello-required`.
- AC2: Every new method has catalogue entries with request, response and error examples that
  the catalogue tests check, and a row in the "where each error can appear" table.
- AC3: No result or notification type can carry a token, an authorisation code or a PKCE
  verifier: the authorisation URL is the only sign-in datum a client receives, and a test
  builds every new result from inputs containing planted secrets and searches the encoded
  bytes (ADR-0010 R6).
- AC4: The pasted redirect URL in `account.login.finish` is a request parameter that never
  appears in any error or result; a test plants a code in it and searches every response.
- AC5: Every new enumerated field is registered as open or closed and listed in the
  catalogue's value-set table.
- AC6: A confirmation token is opaque, bound to its preview, and refused with `invalid-params`
  when used with a different target or twice.
- AC7: `mount.remove` and `account.logout` results state how many files have unsynced
  changes, so a client can warn before confirming; neither has a parameter that could delete
  remote content.
- AC8: Paths given to `item.import` and `item.export` for the local side are absolute and
  canonical by the same rule as protocol paths; a relative or non-canonical one is
  `invalid-params`.

## Required tests

- As the criteria; the existing catalogue, bounds and README-example tests extended.

## Notes

Read `docs/protocol/README.md` and ADR-0019 and ADR-0020 first; the conventions there
(snake_case fields, kebab-case values, open sets with a fallback, a code beside every message)
apply to everything added. `account.login.finish` returns the account so the client can show
it and ask before creating a mount (overview, "Sign-in never creates a mount").

Proposed CLI conventions this protocol must support, for the CLI package later: exit codes
0 ok, 1 failed, 2 usage, 3 daemon not running, 4 partly done; `--json` prints the result
object on success and an error object on failure.

## Open questions

None.
