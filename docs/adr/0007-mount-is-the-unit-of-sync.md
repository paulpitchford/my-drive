# ADR-0007: The mount is the unit of sync; one sign-in, many drives

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

- Graph treats a SharePoint document library as just another drive. Delta is requested per
  drive (`/drives/{id}/root/delta`, `/sites/{id}/drive/root/delta`,
  `/groups/{id}/drive/root/delta`) and returns that drive's hierarchy only
  ([delta docs](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)).
  The research's conclusion is that the unit of sync is the drive, not the account
  ([report, SharePoint section](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#sharepoint-libraries-make-every-drive-a-separate-sync-root)).
- No Linux client offers one sign-in holding many drives. abraunegg needs a separate
  configuration directory, re-authorisation, sync folder and copied service unit per library
  ([abraunegg SharePoint guide](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/sharepoint-libraries.md));
  rclone needs one remote per library ([rclone OneDrive docs](https://rclone.org/onedrive/)).
- Tenant policy can block one library and not another; the client needs a per-library
  "blocked by organisation policy" state, and one library failing must never stall the others
  ([unmanaged devices](https://learn.microsoft.com/en-us/sharepoint/control-access-from-unmanaged-devices);
  [report, tenant policy](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#tenant-policy-not-scopes-is-what-blocks-a-linux-client)).
- Since about June 2025, OneDrive Personal responses return the same drive ID in mixed case
  across fields; abraunegg normalises to lowercase
  ([onedrive-api-docs #1902](https://github.com/OneDrive/onedrive-api-docs/issues/1902)).
- The same folder reachable twice (for example as a library and through a shortcut) must be
  detected by drive and item ID and merged or refused
  ([report, shortcuts](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#sync-a-library-as-its-own-root-treat-shortcuts-as-a-compatibility-feature)).
- Throttling is per user and app per tenant; one scheduler must own the tenant's budget
  ([throttling guidance](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)).

## Decision

We use the overview's vocabulary: **account**, **tenant**, **drive**, **mount**, **item**.

- One daemon per user holds any number of accounts. Each account holds any number of mounts.
- A mount is a drive, or a folder within a drive, attached to a local mount point. It is
  identified by `mount_id`.
- Each mount has its own delta token, its own engine worker and its own backing directory
  (`$XDG_DATA_HOME/mydrive/mounts/<mount_id>/`).
- Mounts are isolated: an error, an organisation-policy block, a `410 Gone` resync or a
  crash of one mount's worker never stalls or blocks another mount, including other mounts
  of the same account.
- `drive_id` is normalised to lowercase at the boundary, in `mydrive-graph`, as each Graph
  response is parsed. Nothing above that layer ever sees a mixed-case drive ID.
- The same remote folder may not be mounted twice. A new mount whose root (`drive_id`,
  `item_id`) equals the root of an existing mount is refused. Detection is by
  (`drive_id`, `item_id`), never by path or name.
- Mounts of one account share that account's tokens, and mounts in one tenant share that
  tenant's request scheduler (ADR-0011). These are the only intended couplings: an expired
  sign-in affects every mount of that account and no other account's mounts.

## Alternatives considered

**One process per drive.** What abraunegg and rclone effectively require. Simple isolation,
but one sign-in cannot span drives, every process competes for the same tenant throttling
budget with no shared scheduler, and the user manages a service per library.

**Account-centric model.** One sync tree and one engine per account, with libraries as
subtrees. Matches how sign-in works, but a policy block or resync in one library stalls the
whole account, and delta streams are per drive anyway, so the model would hide the real
structure.

## Consequences

- Adding a library is adding a mount, under an existing sign-in.
- Per-mount status (including `blocked`) is natural, and the UI and CLI list mounts, not
  accounts.
- The engine runs many workers in one process and needs supervision that restarts a failed
  worker without affecting others.
- Each mount's backing directory and store rows are separate, so a mount can be removed
  without touching other mounts; removing a mount never deletes remote content (ADR-0015).
- **Open concern:** the research found delta reliably available only from a drive root on
  business accounts ([report, shortcuts](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#sync-a-library-as-its-own-root-treat-shortcuts-as-a-compatibility-feature)).
  A mount of a folder within a business drive may therefore need a root delta of the whole
  drive filtered locally, and two folder mounts of one drive would then run two root delta
  streams for the same drive.
- **Open concern:** "never stalls another" and a shared tenant budget interact: a mount with
  heavy traffic consumes budget that other mounts in the tenant need. The scheduler needs a
  fairness rule (ADR-0011).
- **Open concern:** the decision forbids mounting the same folder twice but does not say
  whether a mount may be nested inside another mount of the same drive (for example the drive
  root and one of its folders). This needs a ruling before the mount-add work package.

## Rules for implementers

- **R1.** Every `drive_id` stored, logged, compared or sent over IPC is lowercase. A test feeds
  mixed-case drive IDs in every Graph response field that carries one and asserts that only
  lowercase values leave `mydrive-graph`.
- **R2.** Each mount has exactly one delta token, stored against its `mount_id`.
- **R3.** Each mount's engine worker is independent: injecting a failure (HTTP error, policy
  block, `410 Gone`, worker panic) into one mount's worker does not delay hydration or delta
  processing in another mount beyond the shared scheduler's back-off.
- **R4.** Adding a mount whose root (`drive_id`, `item_id`) equals an existing mount's root is
  refused with an error that names the existing mount.
- **R5.** No two mounts share a backing directory; the backing directory path contains the
  `mount_id`.
- **R6.** Mount identity, conflict detection and item lookup never use a path or display name
  as a key.

## Revisit when

- Gate G2 shows how shortcut stubs appear in delta (shortcuts graft another drive's items
  into a mount; ADR-0015).
- Folder-level delta on business drives proves reliable, or proves unusable.
- Users report the per-tenant scheduler starving one mount.
