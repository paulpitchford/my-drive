# ADR-0008: Three-axis state model with a derived display status

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

Windows and macOS both model on-demand files with independent axes rather than one status
([kernel_ondemand_mechanisms.md, section 5](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/kernel_ondemand_mechanisms.md)):

- **Windows Cloud Files API.** `CF_PIN_STATE` has UNSPECIFIED, PINNED, UNPINNED, EXCLUDED and
  INHERIT. Setting PINNED asks the provider to fetch content asynchronously; UNPINNED asks it
  to dehydrate asynchronously; INHERIT re-evaluates from the parent; any application may set
  pin state ([CF_PIN_STATE](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ne-cfapi-cf_pin_state)).
  `CF_PLACEHOLDER_STATE` carries separate flags including PLACEHOLDER, IN_SYNC, PARTIAL and
  PARTIALLY_ON_DISK ([CF_PLACEHOLDER_STATE](https://learn.microsoft.com/windows/win32/api/cfapi/ne-cfapi-cf_placeholder_state)).
  Dehydration fails if the file is pinned and requires it to be in sync
  ([CfUpdatePlaceholder](https://learn.microsoft.com/windows/win32/api/cfapi/nf-cfapi-cfupdateplaceholder)).
- **macOS File Provider.** Items are dataless or materialized; the system fetches content when
  a dataless file is opened and may make materialized items dataless again, except items with
  pending changes ([Apple: Synchronizing the File Provider Extension](https://developer.apple.com/documentation/fileprovider/synchronizing-the-file-provider-extension)).
  Partial fetching exists (`NSFileProviderPartialContentFetching`)
  ([Apple forums](https://developer.apple.com/forums/thread/812794)).

Existing Linux clients use a single status: twodrive has five emblem states plus a "release
pending" state, and documents edge cases a single status hides, such as pins not propagating
to newly discovered children ([twodrive README](https://github.com/LumiaBlack51/twodrive)).
Nextcloud's socket vocabulary (`OK`, `SYNC`, `ERROR`, ...) cannot distinguish online-only from
local from pinned
([file_manager_integration.md, section 3](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).
Microsoft's OneDrive icon set (cloud, green tick outline, solid green tick, sync arrows, red
cross, lock, chain link) is documented
([OneDrive icons](https://support.microsoft.com/en-us/office/what-do-the-onedrive-icons-mean-11143026-8000-44f8-aaa9-67c985aa49b3)).

## Decision

Every item has three independent state axes, stored separately (ADR-0006), exactly as in the
overview's State model:

| Axis | Values | Meaning |
|---|---|---|
| Hydration | `Placeholder`, `Partial`, `Full` | How much content is local. Folders are `Full` when listed. |
| Pin | `Inherit`, `Pinned`, `Unpinned` | User policy. `Inherit` takes the nearest ancestor's value; the root default is `Unpinned`. |
| Sync | `InSync`, `PendingDownload`, `PendingUpload`, `Uploading`, `Conflict`, `Error`, `Blocked` | Relationship to the server. `Blocked` means organisation policy refuses access. |

Rules:

- `Pinned` (effective) means the engine drives hydration to `Full` and keeps it there.
- Dehydrate is refused unless sync is `InSync` and effective pin is not `Pinned`.
- A file with local changes not yet on the server is never dehydrated or overwritten.

**Display status** is derived from the axes, first match wins:

| Status | Derived when | Windows equivalent |
|---|---|---|
| `blocked` | sync = `Blocked` | none |
| `error` | sync = `Error` or `Conflict` | Red cross |
| `syncing` | sync = `PendingUpload`, `Uploading` or `PendingDownload` | Sync arrows |
| `pinned` | effective pin = `Pinned` and hydration = `Full` | Solid green tick |
| `local` | hydration = `Full` | Green tick outline |
| `online-only` | hydration = `Placeholder` or `Partial` | Cloud |

Two modifiers can accompany any status: `read-only` (lock) and `shortcut` (chain link). They
come from item and mount properties, not from the axes.

The axes are collapsed only for display. The derivation is one function in `mydrive-core`;
no other code derives a display status.

**Mapping to the reference platforms.**

| my-drive | Windows cfapi | macOS File Provider |
|---|---|---|
| Hydration `Placeholder` | placeholder, no data on disk | dataless |
| Hydration `Partial` | PARTIAL / PARTIALLY_ON_DISK | partial content fetch |
| Hydration `Full` | fully hydrated placeholder | materialized |
| Pin `Pinned` | `CF_PIN_STATE_PINNED` | not mapped in the research |
| Pin `Unpinned` | closest to `CF_PIN_STATE_UNPINNED` (see consequences) | not mapped |
| Pin `Inherit` | `CF_PIN_STATE_INHERIT` | not mapped |
| Sync `InSync` | IN_SYNC flag set | no pending changes |
| Other sync values | IN_SYNC flag clear; detail is provider-specific | pending changes or error |

Windows' UNSPECIFIED and EXCLUDED have no counterpart; an item whose chain reaches the root
resolves to the root default `Unpinned`.

## Alternatives considered

**A single status enum.** What most existing clients do (twodrive, Nextcloud's socket
statuses). Simple to render, but it cannot express "pinned and currently uploading" or "local
and in conflict" without multiplying values, it loses the user's pin when an error occurs,
and the dehydrate rule (in sync and not pinned) becomes a list of special cases.

## Consequences

- Adapters receive the axes and the derived status, and reduce them to what their host can
  draw; the derivation is the same everywhere.
- Pin inheritance must be resolved at query time or cached and invalidated on moves and pin
  changes; newly discovered children of a pinned folder are pinned by inheritance, which
  avoids twodrive's documented gap.
- Seven sync values and three hydration values give the engine precise states; tests can
  assert on the axes, not on display strings.
- Our `Unpinned` means "may be dehydrated", not "dehydrate now" as Windows' UNPINNED does.
  On Windows "Free up space" on a pinned item changes its pin. Ruling (overview, "Rulings on
  details"): the engine-level dehydrate request is refused while the item is `Pinned`, and
  the user-facing action (`mydrive free`, "Free Up Space") is an explicit request that first
  sets the pin to `Unpinned` and then dehydrates. It still refuses anything not `InSync`.
- **Open concern:** the derivation shows a pinned file that is not yet `Full` and not
  syncing as `online-only`. This is correct by the table but may surprise users after a
  failed hydration that was then cleared.

## Rules for implementers

- **R1.** The store has three separate columns for hydration, pin and sync; no stored column
  holds a display status (ADR-0006 R5).
- **R2.** Exactly one function in `mydrive-core` maps (hydration, effective pin, sync) to a
  display status. A test covers every combination of the three axes (3 × 2 effective pins ×
  7) and checks it against the table's first-match order.
- **R3.** No crate other than `mydrive-core` contains the display status strings outside
  tests and serialisation of that function's result.
- **R4.** Effective pin for an item with pin `Inherit` is the value of its nearest ancestor
  whose pin is not `Inherit`, or `Unpinned` at the root.
- **R5.** A dehydrate request on an item whose sync is not `InSync`, or whose effective pin is
  `Pinned`, is refused with an error naming the reason and changes no state.
- **R6.** When an item's effective pin becomes `Pinned` and hydration is not `Full`, the
  engine schedules hydration; a pinned file that loses content (for example after recovery)
  is re-hydrated.
- **R7.** A file with sync `PendingUpload`, `Uploading` or `Conflict` is never dehydrated and
  its local content is never overwritten by a download.

## Revisit when

- An exclusion feature (Windows EXCLUDED: temp and lock files never uploaded) is needed.
- Automatic eviction under disk pressure is designed (it must obey the dehydrate rule).
- A file manager can render more than one indicator per file, making the single derived
  status unnecessary for it.
