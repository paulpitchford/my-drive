# mydrive-graph-sim

A fake Microsoft Graph server for tests (ADR-0017). It runs in-process on `127.0.0.1` with
ephemeral ports and is a dev-dependency only: no non-test code may depend on it
(ADR-0017 R1).

Its value is its faithfulness to Graph, including the behaviours that break sync clients.
A divergence between this simulator and Graph is a bug. Every behaviour below names its
source. "Research" means
[`graph_api_and_windows_parity.md`](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in; the Microsoft pages are the ones that note cites, re-read for
this work package in November 2026.

**Rule for defaults.** Every default is what Microsoft documents. Where the documentation
is silent or the behaviour is unverified, there is a switch, and the default is the case
that is harder for a client to handle.

## Use

```rust
let sim = GraphSim::start().await?;              // needs a Tokio runtime
let drive = sim.add_drive(DriveType::Business)?;
let root = sim.root_id(&drive)?;
let folder = sim.create_folder(&drive, &root, "Reports")?;
sim.create_file(&drive, &folder, "q1.xlsx", bytes)?;
// Point the client at sim.base_url() (".../v1.0") with sim.token() as the bearer token.
sim.push_fault(Fault::status(StatusCode::TOO_MANY_REQUESTS).retry_after(3).on_path("/delta"));
assert!(sim.requests().iter().all(|r| r.header("user-agent").is_some()));
```

`GraphSim` is the control API. Its changes act "as if another device did it": they update
the tree and the delta change log but make no request. Dropping it stops the server.

## Switches

Per drive, in `DriveOptions` (`add_drive_with`, `set_options`).

| Switch | Default | Why this default |
|---|---|---|
| `download` | `Redirect` | Documented today: `/content` returns 302 to a pre-authenticated URL. `Direct` models the announced post-April-2027 behaviour (MC1492958). |
| `range` | `Honoured` | Documented normal case (206). `Ignored` is the documented "may be ignored ... HTTP 200" case. |
| `delta_page_size` | `200` | Not documented for delta; the documented children page size. |
| `children_page_size` | `200` | Documented default page size for list children. |
| `delta_repeats` | `EveryChange` | Graph documents both "latest state for each item, not each change" and "the same item may appear more than once". How often repeats happen is not documented, so the default is the harder case (see below). `LatestStateOnce` is the one-occurrence model. |
| `folder_deletes` | `FolderOnly` | Whether Graph reports each descendant of a deleted folder is unverified. Under `FolderOnly` only the folder is reported: its descendants never appear in the feed after the delete, even if they changed since the token, so the client must work out that everything below is gone (harder). `EveryDescendant` reports the folder, then each descendant. |
| `parent_path_outside_delta` | `true` | Graph includes `parentReference.path` in get-item and list-children; only delta omits it. `false` removes it everywhere. |
| `personal_folder_ctag` | `false` | The driveItem resource says cTag "isn't returned if the item is a folder". The get-item example shows a cTag on a folder, so `true` exists for the other case. |
| `content_etag_header` | `false` | Whether content responses carry an `ETag` header is unverified (F03 open question 3, gate G1); absent is harder. |
| `hash_empty_files` | `false` | Whether zero-byte files report a quickXorHash is unverified (research section 4 gaps); absent is harder. |

## Simulated

### Server and auth

| Behaviour | Source |
|---|---|
| Two origins: the Graph API at `http://127.0.0.1:<port>/v1.0`, and a separate origin (different port) for pre-authenticated download URLs, standing in for a storage host like `*.files.1drv.com`. A client treats them as different hosts, so it can be checked not to forward the bearer token (ADR-0011 R10). | [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0) (302 to another host); research section 2 |
| Every Graph-origin request needs `Authorization: Bearer <token>` with a token the simulator knows; a missing, unknown or revoked token gets `401` with code `InvalidAuthenticationToken`. Each simulator has its own token (`sim.token()`); tests can add and revoke tokens. | [Error responses](https://learn.microsoft.com/en-us/graph/errors) (401 meaning). The code string is what Graph returns for a bad token in practice; the errors page lists no code for 401, so it is **unverified** |
| Errors use the Graph body `{"error": {"code", "message"}}`, without `innerError` (see departures). Codes used: `itemNotFound` (404), `invalidRequest` (400), `invalidRange` (416), `activityLimitReached` (429), `serviceNotAvailable` (503), `accessDenied` (403), `nameAlreadyExists` (409), `generalException` (others). | [Error responses](https://learn.microsoft.com/en-us/graph/errors) for the shape. The code strings come from the OneDrive error-code list, not from that page: **unverified** |

### Drives and items

| Behaviour | Source |
|---|---|
| Drive types `personal`, `business`, `documentLibrary`. Personal drive IDs are 16 hex digits; business and library IDs start `b!`. IDs are generated lowercase, so the 2025 mixed-case quirk is not reproduced (see below). | [drive resource](https://learn.microsoft.com/en-us/graph/api/resources/drive?view=graph-rest-1.0); research section 4 (issue #1902) |
| `GET /drives/{id}`: `id`, `driveType`, `name`, `owner.user`, `quota` (`deleted`, `remaining`, `state`, `total`, `used`). | [Get drive](https://learn.microsoft.com/en-us/graph/api/drive-get?view=graph-rest-1.0) example |
| Items: `id`, `name`, `eTag`, `cTag`, `size`, `createdDateTime`, `lastModifiedDateTime`, `fileSystemInfo`, `parentReference` (`driveId`, `driveType`, `id`, and `path` outside delta), `folder.childCount` or `file.mimeType` and `file.hashes.quickXorHash`, `root: {}` on the root. Folder `size` is the total of its live contents. | [driveItem](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0), [itemReference](https://learn.microsoft.com/en-us/graph/api/resources/itemreference?view=graph-rest-1.0), [Get item](https://learn.microsoft.com/en-us/graph/api/driveitem-get?view=graph-rest-1.0) example |
| A file's eTag `"{GUID},n"` changes on every change to it; its cTag `"c:{GUID},n"` only on content changes. The format is copied from the business get-item example and used for personal drives too, where the real format differs: it is a **guess**, and clients must treat both as opaque. | Get-item example; driveItem resource ("eTag for the entire item", "cTag ... for the content"). Research section 2 gaps notes the semantics were not quoted there |
| cTag: never on folders by default (switch `personal_folder_ctag` for personal folders). On business and libraries, absent in delta and present on files in get-item and list-children. Personal files carry it everywhere. | Delta "properties omitted" table; driveItem resource |
| quickXorHash computed with `mydrive-core`'s `QuickXorHasher`; it is the only hash reported (no `sha1Hash`, `sha256Hash`, `crc32Hash`). Zero-byte files: switch `hash_empty_files`. | [hashes resource](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0); research section 2 |
| Timestamps in whole seconds, formatted `2016-03-21T20:01:37Z`, from a simulated clock starting 2026-01-01T00:00:00Z that tests move with `advance_clock`. The wall clock is never read. Creating an item sets both times; changing a file's content sets `lastModifiedDateTime`. Rename and move leave it unchanged: whether Graph changes it then is **unverified**. | abraunegg `usage.md` (research section 2); get-item example for the format |
| Names are unique among siblings without regard to case; a clash is refused. Names may not be empty or contain `/`. Other naming rules are not enforced. | rclone docs (research section 4, case-insensitive namespace) |
| Personal item IDs look like `<DRIVEID>!<n>`, business ones like `01...` (34 characters). Clients must treat them as opaque. | Get-item and delta examples; research section 4 |

### Endpoints

| Behaviour | Source |
|---|---|
| `GET /drives/{d}/root`, `GET /drives/{d}/items/{id}`. A deleted or unknown item is `404 itemNotFound`. | Get item |
| `GET /drives/{d}/root/children`, `GET /drives/{d}/items/{id}/children`, sorted by name, default page size 200, `$top` sets the page size, `@odata.nextLink` carries `$skiptoken` (the case-folded name of the last child served). Children are kept per folder in name order with a case-folded name index, so a create is logarithmic and a page costs the page, not the folder. | [List children](https://learn.microsoft.com/en-us/graph/api/driveitem-list-children?view=graph-rest-1.0) ("default page size (200 items)") |
| `GET /drives/{d}/root/delta` without a token "enumerates the hierarchy's current state": each live item once (root included), no deleted items, at the position of its latest change, so a parent can come after its child. Follow `@odata.nextLink` until a page carries `@odata.deltaLink`. | [delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0) (parameter `token`); research section 2 |
| Changes made while a client pages through an enumeration appear later in the same feed, so an item already served can appear again with its new state. | delta ("may appear more than once") |
| With a token (a deltaLink), the feed returns only what changed since. Under the default `EveryChange` an item appears once per change since the token, each time showing its state when its page is served. This is **deliberately harsher** than "the latest state for each item, not each change": it exists to exercise the client's last-occurrence rule (ADR-0011 R2). A change that lands between two pages makes the first and last occurrence differ. `LatestStateOnce` gives each changed item once. | delta remarks ("latest state for each item, not each change"; "the same item may appear more than once ... use the last occurrence") |
| No `path` in `parentReference` in delta, ever. Get-item and list-children include it by default (`/drive/root:/A%20B` personal, `/drives/{id}/root:/...` business; the root has none). | delta remarks; itemReference (`path` percent-encoded). The business path form is from memory: **unverified** |
| Renaming or moving an item puts only that item in delta, never its descendants. | delta remarks ("renaming a folder does not return its descendants") |
| Deletes appear with the `deleted` facet, as `{}`. Business omits `cTag` and `name`; personal omits `cTag` and `size`. A folder delete reports the folder only (its descendants then never appear in the feed, not even for changes made since the token), or the folder and then every descendant (switch `folder_deletes`). | delta, deleted resource, "properties omitted" tables; descendant reporting **unverified** |

### Content download

| Behaviour | Source |
|---|---|
| `GET .../items/{id}/content` needs the bearer token in both modes. | Download driveItem content; MC1492958 via research section 2 |
| `download = Redirect` (default): `302 Found` with `Location` on the download origin, a fresh `tempauth` key each time; that URL needs no token. Get-item and list-children include `@microsoft.graph.downloadUrl` for files in this mode. Only the newest 1,024 keys stay valid; an older or made-up key gets `401`, as an expired URL would. | Download driveItem content ("302 Found ... preauthenticated download URL", "don't need to include an Authorization header") |
| `download = Direct`: `/content` returns the content itself. | MC1492958 mirror (research section 2): after 2027-04-01 apps must "handle direct content responses instead of following 302 redirects" |
| `range = Honoured` (default): a single `bytes=a-b`, `bytes=a-` or `bytes=-n` range gets `206` with `Content-Range: bytes a-b/len` and exactly those bytes; a range outside the file gets `416` with `Content-Range: bytes */len`. Several ranges or a malformed header are ignored (200). | Download driveItem content (206 example); RFC 9110 |
| `range = Ignored`: `Range` is ignored and the whole body comes back with `200`. | Download driveItem content ("may be ignored and an HTTP 200 response would be returned with the full contents") |
| In redirect mode `Range` on `/content` itself is not honoured; it applies at the download URL. The range switch applies to whichever URL serves content, so "Range on authenticated `/content`" (gate G1, **unverified**) is covered by `Direct` with either range setting. | Download driveItem content ("append the Range header to the actual downloadUrl ... and not to the request for /content"); ADR-0011, ADR-0017 consequences |

### Faults and the request log

| Behaviour | Source |
|---|---|
| A FIFO queue of scripted faults. Each fault has an optional method and path-fragment matcher and applies to exactly the first later request that matches, then is removed. Actions: a status with a Graph error body and optional `Retry-After`; a delay before handling; a truncated body (announces the full `Content-Length`, sends the head and the first N body bytes, then closes the connection). Faults apply before the token check, on both origins. | ADR-0017 decision 2 and R5; `Retry-After` on 429/503 from the throttling guidance (research section 3) |
| A log of every request: origin, method, path, query, the headers `authorization`, `range`, `if-match`, `if-none-match`, `user-agent`, `prefer`, `deltaexcludeparent`, and the response status. It keeps the newest 10,000 requests (`MAX_LOGGED_REQUESTS`), dropping the oldest first; a long test that asserts on the log calls `clear_requests` before the part it checks. | WP-007 |

## Departures from Graph

- **Repeats in token-based delta** are more frequent than Graph is likely to produce, by
  design (see `delta_repeats`).
- **Error bodies** have no `innerError` (with its `request-id` and `date`), and responses
  carry no `request-id` or `client-request-id` headers.
- **The `deleted` facet** is `{}`; the documented `state` property is never set.
- **Folders' eTag, cTag and `lastModifiedDateTime`** are not derived from their contents:
  a folder's eTag changes only when the folder itself is renamed, moved or deleted, and
  its `lastModifiedDateTime` is its creation time. Its `size` and `childCount` do follow
  its contents. Graph's rules here were not researched.
- **eTag and cTag formats** on personal drives are guesses (business format reused).
- **Download URLs** expire only by count (oldest of 1,024), not by time; the driveItem
  resource says `@microsoft.graph.downloadUrl` is invalidated after a short period
  (1 hour) and the download page that it "might expire within minutes". A URL always
  serves the item's current content.
- **IDs and tokens** are deterministic and recognisably fake. Delta tokens are
  `sim<hex>[-<hex>]`; an unknown token gets `400 invalidRequest`. Graph's response to a bad
  token was not researched.

## Not simulated yet

Planned for M1 simulator work packages (WP-007 out of scope):

- Upload: simple `PUT .../content`, upload sessions (`createUploadSession`, 320 KiB
  fragments, `nextExpectedRanges`, `uploadUrl` with or without `Authorization`,
  `deferCommit`), `If-Match`/`If-None-Match` and `412`, `@microsoft.graph.conflictBehavior`,
  `409 nameAlreadyExists`, `507` quota.
- `410 Gone` resync with `resyncChangesApplyDifferences` and
  `resyncChangesUploadDifferences` and a `Location` nextLink.
- A throttling model (resource units per request, per-user and per-tenant limits). Only
  scripted 429/503 faults exist now.
- Ancestor folders appearing in delta when a child changes, and the `deltaExcludeParent`
  header that suppresses them (its own M1 simulator work package).
- SharePoint rewriting Office files after upload (size and hash change).
- Sharing links, permissions, the `shared` facet, `remoteItem` and shortcuts.
- `subscriptions/socketIo` change notifications.
- A dropped connection before any response (ADR-0017 lists it; WP-007 does not). A
  truncated body is available.
- Read-only and policy-blocked libraries (gate G2).

Known Graph behaviours not simulated, and not yet assigned:

- Mixed-case drive IDs across fields on personal accounts (issue #1902, research section 4).
- `@microsoft.graph.downloadUrl` on file items in delta responses. Whether Graph includes
  it there is **unverified** (the delta page's examples do not show it), so delta items
  never carry it.
- `token=latest`, timestamp tokens (business only), `$top`, `$select` and `$expand` on
  delta; `$select`, `$expand`, `$orderby` on items and children.
- `Prefer: hierarchicalsharing`, `deltashowremovedasdeleted`,
  `deltatraversepermissiongaps`, `deltashowsharingchanges`.
- `If-None-Match` and `304 Not Modified` on get-item, list-children and content.
- `/me/drive`, `/users/...`, `/groups/...`, `/sites/...` routes and path-based addressing
  (`root:/path:`); `includeDeletedItems`.
- `@odata.context`, `createdBy`, `lastModifiedBy`, `webUrl`, `webDavUrl`, `description`,
  MIME types other than `application/octet-stream`, image/photo/video facets, `package`
  (OneNote), `specialFolder`, `malware`.
- `sha1Hash` and `crc32Hash`.
- Time-based download URL expiry and the `WWW-Authenticate` header on 401.
- Name rules beyond case-insensitive uniqueness: invalid characters, reserved names, path
  length limits (research section 4).
- Conditional-access `403 insufficient_claims`.
- `sharedWithMe`, which stops returning data after November 2026 (ADR-0011 R13 forbids
  calling it).
