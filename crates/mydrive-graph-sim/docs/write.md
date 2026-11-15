# Writes

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/write.rs` (the
write routes, the refusals, `commit_upload` and `set_locked`), with the state in
`src/model.rs`.

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in. The Microsoft pages below were re-read for this work package in
November 2026: [driveitem-put-content](https://learn.microsoft.com/en-us/graph/api/driveitem-put-content?view=graph-rest-1.0)
(upload small files), [driveitem-post-children](https://learn.microsoft.com/en-us/graph/api/driveitem-post-children?view=graph-rest-1.0)
(create folder), [driveitem-update](https://learn.microsoft.com/en-us/graph/api/driveitem-update?view=graph-rest-1.0),
[driveitem-delete](https://learn.microsoft.com/en-us/graph/api/driveitem-delete?view=graph-rest-1.0),
the [driveItem resource](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0),
the [fileSystemInfo resource](https://learn.microsoft.com/en-us/graph/api/resources/filesysteminfo?view=graph-rest-1.0),
[Graph error responses](https://learn.microsoft.com/en-us/graph/errors) and the
[OneDrive error codes](https://learn.microsoft.com/en-us/onedrive/developer/rest-api/concepts/errors).

**One model for both sides.** Every write goes through the same model operations as the
control API (`create_file`, `create_folder`, `modify_file`, `rename`, `move_item`,
`delete`), at the simulator's clock. A write made over HTTP and the same change made "as if
another device did it" are therefore indistinguishable in delta: same entries, eTags,
cTags, sizes, hashes and times, in both repeat modes. A test checks this change by change.

## Endpoints

| Behaviour | Source |
|---|---|
| `PUT /drives/{d}/items/{id}/content` replaces a file's content: the driveItem with the same ID, new size, quickXorHash, eTag and cTag, answering the status `replace_status` names: `201 Created` by default, `200 OK` with `Ok`. | driveitem-put-content ("To replace an existing item"; its "Updating an existing file" example shows `201 Created`). Which status Graph sends for a replace is **unverified**; `201` is the documented example and the harder case for a client that reads `201` as "a new item" |
| `PUT /drives/{d}/items/{parent-id}:/{name}:/content` creates a file: `201 Created`. If the name is taken (compared without regard to case), `@microsoft.graph.conflictBehavior` in the query string decides; the default is `replace`, which replaces that file's content and keeps its ID and stored name (status as `replace_status` says). A folder of that name is a `409` even under `replace`. Only a single name segment is supported, not a nested path. | driveitem-put-content ("To upload a new file"); driveItem resource ("The default for PUT is *replace*", "should be included in the URL"). The simulator's choices, all **unverified**: keeping the stored name on a case-different replace, and the `409` for a folder in the way under `replace` |
| Spelling of the conflict behaviour, on every route that reads it (create by name, create folder in the query string and the body): only the documented spellings are accepted, the name `@microsoft.graph.conflictBehavior` and the values `fail`, `replace` and `rename`, exactly. Any other value, including a different case such as `Fail`, and the name in a different case, such as `@microsoft.graph.conflictbehavior`, answer `400` and change nothing. | driveItem resource (the three values, in lower case). Whether Graph reads other spellings is **unverified**. If it ignored them it would fall back to its default, which for PUT is the destructive `replace`; if the simulator read them, a client that misspelt `fail` would pass a "never overwrite" test that real Graph could fail. A `400` is never more lenient than either |
| A simple upload takes the body as the content, whatever its `Content-Type` (not checked). A body larger than `simple_upload_max_bytes` (default 250,000,000) answers `413` and stores nothing, refused from `Content-Length` before the body is read, or as soon as more bytes arrive. | driveitem-put-content ("only supports files up to 250 MB"); the errors page for what `413` means. What Graph does above the limit, and the code it uses, are **unverified**; the decimal reading of "250 MB" is the smaller limit, so the harder one |
| `POST /drives/{d}/items/{parent-id}/children` and `POST /drives/{d}/root/children` create a folder from `{"name", "folder": {}}`: `201 Created`. The conflict behaviour comes from the query string, else the body, else `folder_conflict_default` (default `Rename`). A body without a `folder` facet answers `400`: only folders are created here. `replace` is not simulated for folders and answers `400`. | driveitem-post-children (request, `201`, `rename` example, which puts the value in the body). The default behaviour is undocumented (**unverified**); silently creating `<name> 1` is harder than an error. The query string winning over the body is the simulator's choice (**unverified**), following the driveItem resource's "should be included in the URL" |
| `rename` picks `<stem> <n>.<ext>` (or `<name> <n>` without an extension), the smallest free `n` from 1, compared without regard to case. | driveItem resource ("choose a new name"); the exact pattern is **unverified** |
| `PATCH /drives/{d}/items/{id}` with any of `name`, `parentReference.id` (a move) and `fileSystemInfo`, as one change: `200 OK` with the driveItem. eTag changes, cTag does not. Other properties are ignored. `@microsoft.graph.conflictBehavior` on an update, in the query string or the body and with its name in any ASCII case, is **not simulated** and answers `400`: Graph may replace the item in the way, and a simulator that answered a harmless `409` would hide a destructive client bug. A `parentReference.driveId` other than the item's drive answers `400` (moving between drives is not simulated); a `parentReference` without `id` answers `400` (path addressing is not simulated). | driveitem-update (request, `200`, "move an item ... by updating the item's parentReference"); driveItem resource (cTag "isn't changed if only the metadata is changed") |
| `DELETE /drives/{d}/items/{id}`: `204 No Content`. The item and everything below it are gone; delta reports them as for a control-API delete, following `folder_deletes`. A second delete answers `404`. | driveitem-delete (`204`, "moves the items to the recycle bin") |
| `fileSystemInfo` `createdDateTime` and `lastModifiedDateTime` supplied on create folder and update (and to `commit_upload` by upload sessions) are kept, truncated to whole seconds; offsets are converted to UTC. A content replace without them sets `lastModifiedDateTime` to the time of the change. A simple upload has no body for them. | fileSystemInfo resource ("you should include them in the request"; "If the file's content is updated and these properties are not provided, lastModifiedDateTime automatically resets to the current time"); whole seconds from abraunegg `usage.md` (research section 2) |
| Write routes need the bearer token, and the request log records each write's `if-match` header, as it does for every request. Only the first `If-Match` header line of a request is read; a second line is ignored. | [`server.md`](server.md), [`faults.md`](faults.md) |

## Preconditions

| Behaviour | Source |
|---|---|
| `If-Match` on update and delete: if the value matches neither the item's eTag nor its cTag, `412 Precondition Failed` and nothing changes. `*` matches any existing item; a list of quoted tags matches if one of them does; a weak tag (`W/"..."`) never matches. | driveitem-update and driveitem-delete ("the eTag (or cTag) provided doesn't match ... `412 Precondition Failed`", "the item isn't deleted"); RFC 9110 for `*`, lists and strong comparison |
| `If-Match` on a simple replace (either form, when it replaces an existing file): checked only when `simple_replace_honours_if_match` is on. Off by default: a stale eTag does not stop the replace. When on, `If-Match` on a create where no item exists fails with `412`. | **Unverified**: the put-content page lists no `If-Match` header (ADR-0012 context; F04 question 3). Not honouring it is harder, and the overview rules that the engine replaces through sessions until a real-account check |
| `If-Match` on create folder is ignored. | driveitem-post-children lists no such header |
| `412` uses code `resourceModified`. | OneDrive error codes ("usually an eTag mismatch"); that Graph uses this code on `412` is **unverified** |

## Refusals

Every refusal changes nothing at all: no eTag bump, no delta entry, no partial content, no
change to the quota. The model checks everything before it changes anything. The order the
checks are made in is the simulator's (**unverified** in Graph):

- replace by ID: `404` (drive), `413`, then the commit's order;
- create by name: `404` (drive), `400` (bad `conflictBehavior`), `413`, then the commit's
  order;
- the commit (`commit_upload`, also used by upload sessions): `404` (item or parent), `400`,
  `409`, `423`, `412`, `507`;
- create folder: `404` (drive), `400` (body), `404` (parent), `400` (parent or name),
  `409`;
- update: `404`, `400` (unreadable body or a `conflictBehavior`), `423`, `412`, then `404`,
  `400` or `409` from the rename or move;
- delete: `404`, `400` (the root), `423`, `412`.

So an oversized replace of an unknown item answers `413`, not `404`, and a bad conflict
behaviour on an oversized create answers `400`, not `413`.

| Status and code | When | Source |
|---|---|---|
| `404 itemNotFound` | Unknown or deleted drive, item or parent; a move to an unknown parent | Errors page; F04 expects `404` for a missing parent. The errors page's `409` example ("the specified parent folder might not exist") is the other candidate: **unverified** |
| `400 invalidRequest` | Malformed JSON or property types; an unusable name (empty or with `/`); content for a folder; a file as parent; a move into itself or below itself; renaming, moving or deleting the root; a bad `conflictBehavior` value or a misspelt (differently cased) value or name; `replace` for a folder; a bad date-time | Errors page for `400`; the conditions and the code are **unverified** |
| `409 nameAlreadyExists` | Conflict behaviour `fail` on a taken name; a file upload onto a folder's name; a rename or move onto a taken name (always: update creates nothing) | OneDrive error codes ("The specified item name already exists"); createUploadSession (research section 2); case-insensitive names from rclone (research section 4) |
| `413 invalidRequest` | A simple upload over `simple_upload_max_bytes` | Errors page for `413`; code **unverified** |
| `423 resourceLocked` | Replacing, updating or deleting an item locked with `GraphSim::set_locked`, or deleting a folder with a locked item below it | Errors page ("The resource that is being accessed is locked"); driveitem-delete's `prefer: bypass-shared-lock` implies locks refuse deletes. The code is from research section 5 (background, not verified): **unverified**, as is refusing a rename or move of a locked item |
| `507 quotaLimitReached` | The drive's live files would exceed `quota_bytes` (a replace counts only the difference) | Errors page ("The maximum storage quota has been reached"); OneDrive error codes; createUploadSession (research section 2) |

## SharePoint rewriting

| Behaviour | Source |
|---|---|
| On `documentLibrary` drives only, an uploaded file whose extension (case ignored) is in `rewrite_extensions` is stored with different bytes: the simulator appends a fixed marker. Content, size and quickXorHash always describe the same stored bytes, so a download returns bytes that hash to the served hash. Personal and business drives never rewrite. | rclone ("SharePoint libraries (not Personal or OneDrive for Business) can silently modify uploaded Office files", research section 4); ADR-0012 context (Office, "reportedly eml/msg/TIFF", abraunegg). What the real change to the bytes is, is not simulated |
| `rewrite = Later` (default): the upload is committed as uploaded, and the rewrite follows as a separate change with a new eTag and cTag. The upload response describes the uploaded bytes (their size, hash and eTag), which are already stale; later reads and delta show the rewritten state. | When the change becomes visible is **unverified**; `Later` is the harder case. ADR-0012's two-hash rule (store the server's reported hash after upload) holds as written in `InResponse` mode. In `Later` mode the response carries the uploaded bytes' hash and an eTag that delta then supersedes with the rewritten ones; how the engine treats that is an open question the lead has recorded |
| `rewrite = InResponse`: the rewrite is part of the upload, one change; the response already reports the rewritten size and hash. | The other case of the same unknown |
| The quota is checked against the size stored at commit: under `InResponse` the rewritten size, under `Later` the uploaded size, because the service cannot refuse an upload for a rewrite that has not happened yet. The later rewrite is then applied regardless, so `Later` can leave the drive's usage above `quota_bytes`. Times are not changed by the rewrite. | Simulator's choice (**unverified**) |
| Applies to simple uploads and to `commit_upload` (upload sessions), never to control-API changes. | The control API models another device's change as already stored |

## Locks

`GraphSim::set_locked(drive, item, true)` locks an item, as a checkout or co-authoring
session elsewhere would; writes to it then answer `423` (see Refusals). The control API's
own changes ignore locks. A deleted item cannot be locked.

## Switches

All in `DriveOptions`; the defaults are listed and checked in the README's switch table.

| Switch | Effect | Status |
|---|---|---|
| `simple_replace_honours_if_match` | Whether a simple replace checks `If-Match` | **unverified**; default off (harder) |
| `rename_sets_modified` | Whether a rename or move (HTTP or control API) sets `lastModifiedDateTime` to the time of the change | **unverified**; see below |
| `quota_bytes` | The quota behind `507` | Default 1 TiB, the `quota.total` the drive JSON reports |
| `simple_upload_max_bytes` | The simple-upload body limit behind `413` | Documented as "250 MB"; the answer above it is **unverified** |
| `rewrite` | `Off`, `InResponse` or `Later` | Rewriting sourced from rclone; timing **unverified**, default `Later` (harder) |
| `rewrite_extensions` | Which extensions a library rewrites | rclone (Office), abraunegg via ADR-0012 (eml, msg, TIFF) |
| `folder_conflict_default` | Create folder's conflict behaviour when the request names none | **unverified**; default `Rename` (harder) |
| `replace_status` | The status of a successful upload that replaced a file: `Created` (`201`) or `Ok` (`200`) | **unverified**; default `Created`, the documented example and the harder case |

**`rename_sets_modified` default.** Which value is harder is arguable: `true` tests that a
client does not take a new `lastModifiedDateTime` for a content change, `false` that it does
not rely on one to notice a rename. The default is `false` because the switch also governs
control-API renames, and the simulator renamed without touching `lastModifiedDateTime`
before writes existed ([`items.md`](items.md)); changing it would change existing
behaviour.

## For upload sessions (WP-020)

`write::commit_upload(shared, state, UploadCommit { drive, target, content, if_match,
conflict, file_times })` commits a completed upload, with the caller holding the state lock,
and returns the response to send: `201` for a new file, `replace_status` for a replaced one,
or a refusal that changed nothing (`404`, `400`, `409`, `423`, `412`, `507`). It checks
`If-Match` whenever given a value, applies the conflict behaviour, the lock, the quota and
library rewriting, and keeps `file_times`.

What a session needs at creation time is `pub(crate)` in `write.rs`, so WP-020 need not edit
it:

- `if_match(&HeaderMap) -> Option<String>` reads the header (first line only);
  `precondition_holds(&str, &Item) -> bool` checks it against an item's eTag or cTag.
- `ConflictBehavior::parse(&str)`; `conflict_in_query(&HashMap<String, String>)` and
  `conflict_in_body(&Map<String, Value>)`, each `Result<Option<ConflictBehavior>, Refusal>`.
  `parse` checks only the value; the two `conflict_in_*` functions also refuse the name in
  another case, so a session reads the parameter through them, not by key lookup.
- `json_object(&[u8]) -> Result<Map<String, Value>, Refusal>`;
  `parse_file_system_info(Option<&Value>) -> Result<FileTimes, Refusal>`;
  `parse_timestamp(&str) -> Option<i64>`.
- Refusal builders, each a `Response`: `precondition_failed()` (`412`), `name_conflict()`
  (`409`), `locked()` (`423`), `quota_exceeded()` (`507`); and `Refusal`, which renders with
  `into_response()`, with `Refusal::bad(message)` for a `400`.

A session route at the `items/{x}:/{name}:/...` position must name its first parameter
`item`, as the routes here do.

## Departures from Graph

- **`fileSystemInfo` and the item's own times are one pair.** Graph keeps
  `createdDateTime` and `lastModifiedDateTime` as the service saw them and `fileSystemInfo`
  as the client supplied them. The simulator stores one pair, so supplied times show in
  both. **A client that reads the top-level `createdDateTime` or `lastModifiedDateTime`
  instead of `fileSystemInfo` would pass here but fail against Graph.** Separating them
  needs a change in `src/json.rs` (a follow-up for the package that owns it).
- **Folders' eTag, cTag and times** do not follow their contents (as in
  [`items.md`](items.md)); the driveItem resource says a folder's cTag changes "when content
  or metadata of any descendant ... is changed". HTTP writes and control-API changes agree
  on this, so the delta comparison holds, but neither matches Graph.
- **Quota** counts only live files: no versions, no recycle bin. The drive JSON's
  `quota.total` stays 1 TiB whatever `quota_bytes` says (it is rendered in `src/json.rs`).
- **No versions and no recycle bin**: a replace keeps no old version, and a deleted item
  cannot be restored.
- **Not simulated**: `If-None-Match` on writes; `@microsoft.graph.sourceUrl`; path-based
  addressing (`root:/a/b:`); moving between drives; `copy`; `permanentDelete`; `prefer:
  bypass-shared-lock`; check-in and check-out; read-only and blocked libraries (WP-023, in
  front of these routes); name rules beyond empty, `/` and case-insensitive uniqueness;
  `innerError` codes such as `entityTagDoesNotMatch`.
