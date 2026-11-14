# ADR-0015: SharePoint: a library is its own mount; shortcuts later; deletes never propagate from unmounting

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** G2 (see `docs/architecture/overview.md`)

## Context

Graph treats a library as another drive with its own delta stream, and driveItem delta
accepts `Files.*` scopes on work accounts, so `Files.ReadWrite.All` is enough once the drive
ID is known
([delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)).
abraunegg needs a configuration directory, sign-in and service per library; rclone needs one
remote per library
([abraunegg SharePoint guide](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/sharepoint-libraries.md),
[rclone OneDrive docs](https://rclone.org/onedrive/)).

Discovery (research note `sharepoint_libraries.md`, section 1):

- `/shares/{encoded}` resolves a sharing link to a drive item. `Prefer: redeemSharingLink`
  grants durable access like clicking the link; `redeemSharingLinkIfNecessary` only for the
  request ([shares](https://learn.microsoft.com/en-us/graph/api/shares-get?view=graph-rest-1.0)).
  Whether a plain library URL always resolves is unverified.
- `filesFolder` resolves a Teams channel; after the private-channel migration it returns a
  drive root ([filesFolder](https://learn.microsoft.com/en-us/graph/api/channel-get-filesfolder?view=graph-rest-1.0)).
  The calls for listing a user's teams and their permissions were not verified.
- Site search and followed sites need `Sites.Read.All`
  ([site search](https://learn.microsoft.com/en-us/graph/api/site-search?view=graph-rest-1.0)).

Policy and library settings:

- Unmanaged-device controls give browser-only access or "Access Denied" to machines not
  hybrid-joined or Intune-compliant, can be per site, and take up to 24 hours to apply
  ([unmanaged devices](https://learn.microsoft.com/en-us/sharepoint/control-access-from-unmanaged-devices)).
  rclone documents AADSTS65005 when a tenant has not provisioned the app. The exact Graph errors
  are unknown (gate G2).
- Microsoft's client syncs libraries read-only when they require check-out, have validation
  columns or draft restrictions, or (per a second page) required columns; local edits then
  "won't sync"
  ([restrictions](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa),
  [Sync SharePoint files](https://support.microsoft.com/en-us/office/sync-sharepoint-and-teams-files-with-your-computer-6de9ede8-5b6e-4503-80b2-6190f3354a88)).
  The Graph calls to detect these were not fetched; detection is unverified.
- Names: reserved characters and names, `~$` prefix, `_vti_` anywhere, `forms` at a library
  root, case-insensitive namespace, 400-character path limit; at most 300,000 synced items in
  total is advised ([restrictions](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)).
- Shortcuts need a second change stream, were reported absent from business delta
  ([onedrive-api-docs #1427](https://github.com/OneDrive/onedrive-api-docs/issues/1427)), and
  deleting the folder instead of the shortcut "deletes it from everyone's OneDrive"
  ([Add shortcuts](https://support.microsoft.com/en-us/office/add-shortcuts-to-shared-folders-in-onedrive-for-work-or-school-d66b1347-99b7-4470-9360-ffc048d35a33)).

## Decision

A library is a drive and is mounted as its own mount (ADR-0007), at
`~/<Org>/<Site> - <Library>` (overview, On disk).

**Adding a library.** The user can paste a sharing link, browse Teams, or enter a drive ID.
A pasted link is resolved through `/shares/{encoded}` with
`Prefer: redeemSharingLinkIfNecessary` for the preview; the link is redeemed only when the
user confirms. Browsing Teams uses `filesFolder` and takes whatever item it returns, including
a drive root for private channels. Site search is an optional extra that requests
`Sites.Read.All` when the user opens it (ADR-0010). Before confirming, the flow shows the
library's item count and warns as the total across all mounts approaches 300,000 items.

**Blocked.** When organisation policy denies access (unmanaged-device controls, AADSTS65005
consent refusal), the mount enters `Blocked`. One blocked library never stalls other mounts.

**Read-only libraries.** Required check-out, validation columns, draft restrictions and
required columns are detected once per library. In such a library the VFS refuses writes,
creates, renames and deletes with `EROFS`, and items carry the `read-only` modifier.

**Names.** Names are validated before upload: reserved characters and names, `~$` prefix,
`_vti_`, `forms` at a library root, case collisions, and the 400-character path limit. An
invalid local name is not uploaded; the item shows `error` with the reason.

**Shortcuts.** "Add shortcut to My files" support comes later, in milestone M5.

**Delete safety.** Removing a mount never deletes remote content. Unmount and "stop syncing"
leave local copies in place. Deleting a shortcut's folder locally never deletes the target
folder's content for everyone without explicit confirmation.

## Alternatives considered

**One sign-in per library** (abraunegg, rclone). The gap this project exists to close.
Rejected.

**Shortcuts as the primary model** (Microsoft's recommendation). Second change stream,
unreliable stubs and dangerous delete semantics. Deferred to M5.

**Uploading into read-only libraries and reporting failures** (Windows strands edits
silently). Rejected for an up-front `EROFS`.

**`Sites.Read.All` at sign-in.** Broader than needed and refused by some tenants. Rejected.

## Consequences

- The store, scheduler (ADR-0011) and status model must handle a mount-level `Blocked` that
  coexists with healthy mounts in the same account.
- Name validation lives in `mydrive-core` and is shared by all mounts.
- Library discovery has no "list all my libraries" call; users need a link, a team or an ID.
- Open concern: AADSTS65005 is a consent error raised at sign-in for the whole tenant, so it
  will usually block every mount of that account, not one library.
- Open concern: listing a user's teams may need a scope outside the baseline in ADR-0010;
  this was not verified.
- Open concern: two accounts in the same tenant collide on `~/<Org>`; no rule is decided.

## Rules for implementers

- **R1.** Each library mount has its own `mount_id`, delta token and backing directory.
- **R2.** Link preview uses `redeemSharingLinkIfNecessary`; `redeemSharingLink` is sent only after
  explicit user confirmation.
- **R3.** The Teams flow mounts the item `filesFolder` returns and never derives a path.
- **R4.** A policy denial puts only the affected mount into `Blocked`; other mounts' requests
  continue.
- **R5.** In a read-only library, `write`, `create`, `mkdir`, `rename`, `unlink` and `setattr`
  changing content fail with `EROFS`.
- **R6.** A name failing validation is never uploaded and its item has sync = `Error` with a reason.
- **R7.** Adding a library shows its item count and warns when the total would exceed a configured
  threshold near 300,000.
- **R8.** `mydrive mount remove`, unmount and "stop syncing" issue no remote delete and remove no
  local copy of hydrated content.
- **R9.** A local delete of a shortcut's folder never reaches the target drive without explicit
  confirmation.

## Revisit when

- G2 reports the Graph errors under unmanaged-device policy and how shortcut stubs appear.
- M5 starts shortcut work.
- Intune broker sign-in is added (ADR-0010).
- Read-only detection proves unreliable from Graph data.
