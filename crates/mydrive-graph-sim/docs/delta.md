# Delta

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/delta.rs` (the
route and the token format), with the change log and paging in `src/model.rs`.

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in.

## Simulated

| Behaviour | Source |
|---|---|
| `GET /drives/{d}/root/delta` without a token "enumerates the hierarchy's current state": each live item once (root included), no deleted items, at the position of its latest change, so a parent can come after its child. Follow `@odata.nextLink` until a page carries `@odata.deltaLink`. | [delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0) (parameter `token`); research section 2 |
| Changes made while a client pages through an enumeration appear later in the same feed, so an item already served can appear again with its new state. | delta ("may appear more than once") |
| With a token (a deltaLink), the feed returns only what changed since. Under the default `EveryChange` an item appears once per change since the token, each time showing its state when its page is served. This is **deliberately harsher** than "the latest state for each item, not each change": it exists to exercise the client's last-occurrence rule (ADR-0011 R2). A change that lands between two pages makes the first and last occurrence differ. `LatestStateOnce` gives each changed item once. | delta remarks ("latest state for each item, not each change"; "the same item may appear more than once ... use the last occurrence") |
| No `path` in `parentReference` in delta, ever. Get-item and list-children include it by default (`/drive/root:/A%20B` personal, `/drives/{id}/root:/...` business; the root has none). | delta remarks; itemReference (`path` percent-encoded). The business path form is from memory: **unverified** |
| Renaming or moving an item puts only that item in delta, never its descendants. | delta remarks ("renaming a folder does not return its descendants") |
| Deletes appear with the `deleted` facet, as `{}`. Business omits `cTag` and `name`; personal omits `cTag` and `size`. A folder delete reports the folder only (its descendants then never appear in the feed, not even for changes made since the token), or the folder and then every descendant (switch `folder_deletes`). | delta, deleted resource, "properties omitted" tables; descendant reporting **unverified** |

## Departures from Graph

- **Repeats in token-based delta** are more frequent than Graph is likely to produce, by
  design (see `delta_repeats` in the README's switch table).
- **The `deleted` facet** is `{}`; the documented `state` property is never set.
- **Delta tokens** are `sim<hex>[-<hex>]`; an unknown token gets `400 invalidRequest`.
  Graph's response to a bad token was not researched.
