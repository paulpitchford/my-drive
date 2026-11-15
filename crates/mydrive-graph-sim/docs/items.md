# Drives and items

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/items.rs` (the
drive, item and children routes, and the control API that builds and changes the tree),
with the tree itself in `src/model.rs` and its JSON in `src/json.rs`.

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in.

## Simulated

| Behaviour | Source |
|---|---|
| Drive types `personal`, `business`, `documentLibrary`. Personal drive IDs are 16 hex digits; business and library IDs start `b!`. IDs are generated lowercase, so the 2025 mixed-case quirk is not reproduced (see the README). | [drive resource](https://learn.microsoft.com/en-us/graph/api/resources/drive?view=graph-rest-1.0); research section 4 (issue #1902) |
| `GET /drives/{id}`: `id`, `driveType`, `name`, `owner.user`, `quota` (`deleted`, `remaining`, `state`, `total`, `used`). | [Get drive](https://learn.microsoft.com/en-us/graph/api/drive-get?view=graph-rest-1.0) example |
| Items: `id`, `name`, `eTag`, `cTag`, `size`, `createdDateTime`, `lastModifiedDateTime`, `fileSystemInfo`, `parentReference` (`driveId`, `driveType`, `id`, and `path` outside delta), `folder.childCount` or `file.mimeType` and `file.hashes.quickXorHash`, `root: {}` on the root. Folder `size` is the total of its live contents. | [driveItem](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0), [itemReference](https://learn.microsoft.com/en-us/graph/api/resources/itemreference?view=graph-rest-1.0), [Get item](https://learn.microsoft.com/en-us/graph/api/driveitem-get?view=graph-rest-1.0) example |
| A file's eTag `"{GUID},n"` changes on every change to it; its cTag `"c:{GUID},n"` only on content changes. The format is copied from the business get-item example and used for personal drives too, where the real format differs: it is a **guess**, and clients must treat both as opaque. | Get-item example; driveItem resource ("eTag for the entire item", "cTag ... for the content"). Research section 2 gaps notes the semantics were not quoted there |
| cTag: never on folders by default (switch `personal_folder_ctag` for personal folders). On business and libraries, absent in delta and present on files in get-item and list-children. Personal files carry it everywhere. | Delta "properties omitted" table; driveItem resource |
| quickXorHash computed with `mydrive-core`'s `QuickXorHasher`; it is the only hash reported (no `sha1Hash`, `sha256Hash`, `crc32Hash`). Zero-byte files: switch `hash_empty_files`. | [hashes resource](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0); research section 2 |
| Timestamps in whole seconds, formatted `2016-03-21T20:01:37Z`, from a simulated clock starting 2026-01-01T00:00:00Z that tests move with `advance_clock`. The wall clock is never read. Creating an item sets both times; changing a file's content sets `lastModifiedDateTime`. Rename and move leave it unchanged: whether Graph changes it then is **unverified**. | abraunegg `usage.md` (research section 2); get-item example for the format |
| Names are unique among siblings without regard to case; a clash is refused. Names may not be empty or contain `/`. Other naming rules are not enforced. | rclone docs (research section 4, case-insensitive namespace) |
| Personal item IDs look like `<DRIVEID>!<n>`, business ones like `01...` (34 characters). Clients must treat them as opaque. | Get-item and delta examples; research section 4 |

## Endpoints

| Behaviour | Source |
|---|---|
| `GET /drives/{d}/root`, `GET /drives/{d}/items/{id}`. A deleted or unknown item is `404 itemNotFound`. | Get item |
| `GET /drives/{d}/root/children`, `GET /drives/{d}/items/{id}/children`, sorted by name, default page size 200, `$top` sets the page size, `@odata.nextLink` carries `$skiptoken` (the case-folded name of the last child served). Children are kept per folder in name order with a case-folded name index, so a create is logarithmic and a page costs the page, not the folder. | [List children](https://learn.microsoft.com/en-us/graph/api/driveitem-list-children?view=graph-rest-1.0) ("default page size (200 items)") |

The delta feed is documented in [`delta.md`](delta.md), content download in
[`content.md`](content.md).

## Departures from Graph

- **Folders' eTag, cTag and `lastModifiedDateTime`** are not derived from their contents:
  a folder's eTag changes only when the folder itself is renamed, moved or deleted, and
  its `lastModifiedDateTime` is its creation time. Its `size` and `childCount` do follow
  its contents. Graph's rules here were not researched.
- **eTag and cTag formats** on personal drives are guesses (business format reused).
- **IDs and tokens** are deterministic and recognisably fake.
