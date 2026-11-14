# Definition of done

A work package is done when every line below is true. "Mostly" is not done; list what is
missing in the pull request instead.

## Behaviour

- [ ] Every acceptance criterion in the WP is met and has a test that proves it.
- [ ] Every ADR "Rules for implementers" item the WP implements has a test that names it
      (for example `/// ADR-0012 R3`).
- [ ] Failure paths are handled, not just the happy path: network errors, partial results,
      cancellation, restart in the middle.
- [ ] No behaviour was added that the WP did not ask for.

## Data safety

- [ ] No path exists where local content that has not reached the server is deleted,
      overwritten or dehydrated.
- [ ] No path exists where missing content is returned as zeros or an empty file.
- [ ] Anything that deletes or overwrites has a test for the case where it must refuse.
- [ ] State written to the store is consistent if the process dies at any point.

## Code

- [ ] `just check` passes.
- [ ] Only the WP's owned paths changed.
- [ ] Crate boundaries hold: no new upward or sideways dependency.
- [ ] No `unsafe` outside `mydrive-vfs`; any there has a `SAFETY:` comment.
- [ ] No `unwrap`, `expect`, `todo!`, `unimplemented!` or `dbg!` outside tests.
- [ ] No new dependency outside `[workspace.dependencies]`.
- [ ] Public items have doc comments.
- [ ] Errors say what was being done, to which item, and why it failed.
- [ ] Nothing secret is logged: no tokens, no file contents, no pre-authenticated URLs.

## Tests

- [ ] Tests run without network access and without a real Microsoft account.
- [ ] Tests are deterministic: no sleeps for timing, no dependence on wall-clock time
      (inject a clock), no dependence on test order.
- [ ] A test fails if the behaviour it covers is removed. (Check at least one by breaking
      the code.)

## Documentation

- [ ] Vocabulary matches `docs/architecture/overview.md`.
- [ ] Protocol or on-disk format changes are reflected in `docs/protocol/` or the schema
      docs in the same pull request.
- [ ] The pull request lists anything deferred, discovered or uncertain.
