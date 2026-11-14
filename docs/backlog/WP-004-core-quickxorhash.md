# WP-004: quickXorHash

- **Status:** done (#5)
- **Milestone:** M0
- **Wave:** 1
- **Depends on:** —
- **Gates:** —
- **Owned paths:** `crates/mydrive-core/src/hash.rs`, `NOTICE` (only to record provenance)
- **ADRs:** ADR-0011, ADR-0012, ADR-0002
- **Flows / journeys:** F04

## Goal

quickXorHash is the only content hash Microsoft Graph provides on every account type. The
engine needs it to tell whether local and remote content match. A wrong implementation would
make the client re-upload or re-download everything, or worse, believe different files are
the same.

## In scope

- A streaming hasher: create, `update(&[u8])` any number of times, `finalize()` to the
  20-byte digest.
- A digest type with `Eq`, `Debug`, and conversion to and from the base64 form Graph returns
  in `file.hashes.quickXorHash`.
- A convenience function hashing a `std::io::Read`.

## Out of scope

- Hashing files on a background thread, caching hashes (engine, M1).
- SHA-1 or SHA-256 (ADR-0011 says not to use them).

## Acceptance criteria

- AC1: The digest of the empty input matches the reference.
- AC2: Digests match known reference vectors for inputs shorter than, equal to and longer
  than the algorithm's internal block, including at least one input over 1 MiB.
- AC3: Feeding the same bytes in any chunking produces the same digest.
- AC4: Base64 encoding and decoding round-trip, and malformed or wrong-length base64 is
  rejected with a typed error.
- AC5: No `unsafe`, and no dependency beyond `base64`, which the lead has already added to
  this crate's manifest.

## Required tests

- Reference vectors with their source recorded in a comment.
- Property test for AC3.

## Notes

Microsoft documents the algorithm with a reference implementation in the OneDrive developer
docs. rclone has a Go implementation with test vectors (MIT). If you take test vectors or
adapt code from another project, record the project, commit and licence in a header comment
and in `NOTICE` (ADR-0002). Prefer writing the implementation from Microsoft's description
and using third-party vectors only as test data.

If you cannot obtain trustworthy reference vectors, stop and report; do not invent expected
values by running your own implementation.

## Open questions

None.
