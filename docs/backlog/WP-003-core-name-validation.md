# WP-003: Name and path validation

- **Status:** ready
- **Milestone:** M0
- **Wave:** 1
- **Depends on:** —
- **Gates:** —
- **Owned paths:** `crates/mydrive-core/src/names.rs`
- **ADRs:** ADR-0015, ADR-0011
- **Flows / journeys:** F04, J05

## Goal

Linux allows file names that OneDrive and SharePoint reject. Decide, before any upload is
attempted, whether a local name can exist on the server, and say exactly why not, so the
engine can show a clear error instead of looping on a failed upload.

## In scope

- A function validating a single name for a given context: account kind (personal, or
  work/SharePoint) and whether the item sits at the root of a library.
- A function validating a full server-relative path length against the 400-character limit.
- A function detecting names in one folder that differ only by case.
- A typed error enumerating each reason (forbidden character and which one, reserved name,
  forbidden prefix or substring, leading or trailing character, too long, case collision).
- Rules taken from Microsoft's restrictions page as recorded in
  `research_notes/OneDrive Linux on demand clients/sharepoint_libraries.md` (forbidden
  characters, reserved device names, `~$` prefix, `_vti_`, `forms` at a library root, `.lock`
  and `desktop.ini`, leading and trailing spaces, trailing periods).
- A doc comment on each rule citing its source.

## Out of scope

- Renaming or "fixing" invalid names. Invalid names are reported, never silently changed.
- Deciding what the engine does with an invalid name (M1).

## Acceptance criteria

- AC1: Each forbidden character listed in the research note is rejected, with the error
  naming the character.
- AC2: Reserved device names are rejected case-insensitively, with and without an extension
  where the source says so.
- AC3: `forms` is rejected only at a library root in the work/SharePoint context.
- AC4: A name valid on Linux and on the server (including non-ASCII, emoji, and names with
  internal spaces and dots) is accepted.
- AC5: Path length is counted the way the source specifies; where the source does not
  specify the counting rule, the function takes the already-encoded server path and the doc
  comment says the rule is unverified.
- AC6: Two names differing only by case are reported as a collision; the comparison is
  Unicode-aware simple case folding, documented as an approximation of the server's rule.
- AC7: Every rule in the module has a test, and the module documents any rule in the
  research note it chose not to implement and why.

## Required tests

- Table-driven tests per rule.
- Property test: any name made only of ASCII letters, digits and inner spaces, not a
  reserved word, within length limits, is accepted in every context.

## Notes

If the research note and Microsoft's page disagree, or a rule is ambiguous (the note flags
that how the 400-character limit is counted was not confirmed), implement the stricter
reading, mark it in the doc comment, and list it in your report.

## Open questions

None.
