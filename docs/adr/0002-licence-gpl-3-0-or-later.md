# ADR-0002: Licence the project under GPL-3.0-or-later

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

The research recommends starting a fresh client while borrowing designs and, where useful,
code from existing projects
([report, "Two mature clients exist"](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#two-mature-clients-exist-and-neither-delivers-the-windows-triad)).
The licences of the most useful sources, as read from the GitHub API and READMEs on
2026-11-14
([existing_clients.md, Q1 and Q5](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/existing_clients.md)):

| Project | Licence | What is worth borrowing |
|---|---|---|
| [abraunegg/onedrive](https://github.com/abraunegg/onedrive) | GPL-3.0 | Graph edge cases, shared items, SharePoint handling |
| [jstaf/onedriver](https://github.com/jstaf/onedriver) | GPL-3.0 | Working FUSE on-demand design |
| [konedrive](https://github.com/kartas39/konedrive) | GPL-3.0-or-later (per README) | Rust daemon, cloud thumbnails in the file-manager plugin |
| [rclone](https://rclone.org/commands/rclone_mount/) | MIT | Sparse VFS cache with byte-range tracking, OneDrive backend |
| [twodrive](https://github.com/LumiaBlack51/twodrive) | MIT | Rust daemon, CLI, Nautilus extension, pin/release semantics |

Code from the GPL-3.0 projects can only be reused in a GPL-3.0-compatible project; MIT code
can be reused under almost any licence
([existing_clients.md, Q5 inferences](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/existing_clients.md)).
Whether abraunegg and onedriver are "GPL-3.0-only" or "GPL-3.0-or-later" was not checked; the
GitHub API reports only "GPL-3.0". The licence of Nextcloud's shell-integration scripts was
not verified
([file_manager_integration.md, section 3](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).

The repository's `LICENSE` file already contains the GNU GPL version 3 text.

## Decision

my-drive is licensed under the GNU General Public License, version 3 or (at the user's
option) any later version.

- The repository `LICENSE` file is the verbatim GPL v3 text.
- Every crate's `Cargo.toml` declares `license = "GPL-3.0-or-later"` (SPDX identifier).
- Code may be borrowed from other projects only if its licence is GPL-3.0-compatible and has
  been checked. Each borrowed piece records its provenance (source project, commit, licence)
  in a header comment in the file that contains it, and in a top-level `NOTICE` file.
- Code is never copied from a project whose licence has not been checked.
- File-manager adapters and other plugins run as separate processes from their host file
  manager and are separate works; our licence does not extend to the host file manager.
  Adapters in this repository are themselves GPL-3.0-or-later unless an ADR says otherwise.

## Alternatives considered

**MIT.** Maximises downstream reuse and matches rclone and twodrive. Rejected because it
would forbid reusing any code from abraunegg, onedriver or konedrive, which hold most of the
existing Linux OneDrive knowledge.

**Apache-2.0.** Adds an explicit patent grant. Rejected for the same reason as MIT: GPL-3.0
code cannot be incorporated into an Apache-2.0 project. (The one-way compatibility of
Apache-2.0 code into GPL-3.0 projects is general licensing knowledge, not from the research.)

## Consequences

- Code and designs from all five projects above are available to us, subject to the
  provenance rules.
- Anyone redistributing my-drive or a derivative must provide source under the GPL.
- Commercial or MIT-licensed projects cannot take our code without adopting the GPL.
- We must maintain the `NOTICE` file and provenance headers; a missing header is a review
  failure, not a cosmetic issue.
- Third-party crates pulled in as dependencies must also have GPL-3.0-compatible licences.
- **Open concern:** if a borrowed file is GPL-3.0-only (not "or later"), that file cannot be
  relicensed as GPL-3.0-or-later; it must keep its own SPDX tag, and the combined binary is
  then effectively distributable under GPL-3.0 only. The licence variant of abraunegg and
  onedriver was not checked.
- **Open concern:** the statement that plugins run as separate processes holds for the
  Omarchy Files plugin interface (Files "never loads plugin code", per the README's Omarchy
  section) and for Flea, but not for nautilus-python extensions or Dolphin overlay plugins,
  which load into the file manager's own process
  ([file_manager_integration.md, section 1](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).
  Those adapters still run separately from the daemon, but licence compatibility with each
  in-process host should be checked when that adapter is written (ADR-0014).

## Rules for implementers

- **R1.** Every `Cargo.toml` in the workspace has `license = "GPL-3.0-or-later"`, unless the
  crate contains code under a different compatible licence, in which case the SPDX
  expression names both and the `NOTICE` file explains why.
- **R2.** Every file containing code taken or adapted from another project has a header
  comment naming the source project, the commit (full hash) and the source licence, and the
  same three facts appear in an entry in the top-level `NOTICE` file.
- **R3.** A pull request that adds borrowed code states in its description how the source
  licence was checked (file and commit where the licence was read). Reviewers reject
  borrowed code without this.
- **R4.** No code is borrowed from a project under a licence that is not GPL-3.0-compatible,
  or whose licence has not been checked.
- **R5.** CI checks dependency licences against an allow-list of GPL-3.0-compatible licences
  and fails on anything else.

## Revisit when

- A dependency or a body of code we need is available only under a GPL-incompatible licence.
- The project wants its libraries reused by non-GPL projects; a separate, more permissive
  licence for specific library crates would need a new ADR and must not contain borrowed
  GPL code.
