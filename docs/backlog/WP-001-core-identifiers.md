# WP-001: Core identifiers

- **Status:** ready
- **Milestone:** M0
- **Wave:** 1
- **Depends on:** —
- **Gates:** —
- **Owned paths:** `crates/mydrive-core/src/ids.rs`
- **ADRs:** ADR-0006, ADR-0007
- **Flows / journeys:** —

## Goal

Give every other crate one correct, type-safe way to name accounts, tenants, drives, items
and mounts, so that an item can never be confused with a path and a drive ID can never be
compared in the wrong case.

## In scope

- `DriveId`: a non-empty string normalised to lowercase on construction.
- `ItemId`: a non-empty opaque string, stored exactly as given.
- `ItemKey { drive_id, item_id }`: the identity of an item everywhere in the system.
- `AccountId`, `TenantId`, `MountId`: opaque newtypes. `MountId` is safe to use as a single
  directory name: 1 to 64 characters from `a-z`, `0-9` and `-`, not starting with `-`.
- For each: fallible constructor returning a `thiserror` error, `as_str`, `Display`,
  `FromStr`, `Debug`, `Clone`, `Eq`, `Hash`, `Ord`, and transparent `serde` (a JSON string).

## Out of scope

- Generating new mount or account IDs (needs randomness; belongs to the engine).
- Paths. No type in this module holds or derives a filesystem path.

## Acceptance criteria

- AC1: `DriveId` built from `"B!AbC123"` equals one built from `"b!abc123"`, and `as_str`
  returns the lowercase form.
- AC2: `ItemId` preserves case and is compared exactly.
- AC3: Every constructor rejects the empty string with a typed error.
- AC4: `MountId` rejects `/`, `.`, `..`, uppercase letters, whitespace, a leading `-`, and
  anything longer than 64 characters.
- AC5: Every type round-trips through `serde_json` as a plain string, and deserialising goes
  through the same validation and normalisation as the constructor.
- AC6: `ItemKey` is usable as a `HashMap` and `BTreeMap` key.

## Required tests

- Property test: for any valid input, `parse(display(x)) == x` for every type.
- Property test: `DriveId` construction is idempotent and case-insensitive.
- A test doc-commented with the ADR-0007 rule on lowercase `drive_id` normalisation.

## Notes

Personal accounts have returned drive IDs in mixed case across fields since June 2025
(README, "Microsoft Graph"); that is why normalisation happens here, once, at the type.

## Open questions

None.
