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

## Simulated

### Server and auth

| Behaviour | Source |
|---|---|
| Two origins: the Graph API at `http://127.0.0.1:<port>/v1.0`, and a separate origin (different port) for pre-authenticated download URLs, standing in for a storage host like `*.files.1drv.com`. A client treats them as different hosts, so it can be checked not to forward the bearer token (ADR-0011 R10). | [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0) (302 to another host); research section 2 |
| Every Graph-origin request needs `Authorization: Bearer <token>` with a token the simulator knows; a missing, unknown or revoked token gets `401` with code `InvalidAuthenticationToken`. Each simulator has its own token (`sim.token()`); tests can add and revoke tokens. | [Error responses](https://learn.microsoft.com/en-us/graph/errors) (401 meaning). The code string is what Graph returns for a bad token in practice; the errors page lists no code for 401, so it is **unverified** |
| Errors use the Graph body `{"error": {"code", "message"}}`. Codes used: `itemNotFound` (404), `invalidRequest` (400), `invalidRange` (416), `activityLimitReached` (429), `serviceNotAvailable` (503), `accessDenied` (403), `nameAlreadyExists` (409), `generalException` (others). | [Error responses](https://learn.microsoft.com/en-us/graph/errors) for the shape. The code strings come from the OneDrive error-code list, not from that page: **unverified** |

### Drives and items

| Behaviour | Source |
|---|---|
| Drive types `personal`, `business`, `documentLibrary`. Personal drive IDs are 16 hex digits; business and library IDs start `b!`. IDs are generated lowercase, so the 2025 mixed-case quirk is not reproduced (see below). | [drive resource](https://learn.microsoft.com/en-us/graph/api/resources/drive?view=graph-rest-1.0); research section 4 (issue #1902) |
| `GET /drives/{id}`: `id`, `driveType`, `name`, `owner.user`, `quota` (`deleted`, `remaining`, `state`, `total`, `used`). | [Get drive](https://learn.microsoft.com/en-us/graph/api/drive-get?view=graph-rest-1.0) example |
| Items: `id`, `name`, `eTag`, `cTag`, `size`, `createdDateTime`, `lastModifiedDateTime`, `fileSystemInfo`, `parentReference` (`driveId`, `driveType`, `id`), `folder.childCount` or `file.mimeType` and `file.hashes.quickXorHash`, `root: {}` on the root. Folder `size` is the sum of its contents. | [driveItem](https://learn.microsoft.com/en-us/graph/api/resources/driveitem?view=graph-rest-1.0), [itemReference](https://learn.microsoft.com/en-us/graph/api/resources/itemreference?view=graph-rest-1.0), [Get item](https://learn.microsoft.com/en-us/graph/api/driveitem-get?view=graph-rest-1.0) example |
| eTag `"{GUID},n"` changes on every change; cTag `"c:{GUID},n"` changes only on content changes. Shapes copied from the get-item example. | Get item example. That eTag tracks the item and cTag the content is in the driveItem resource page; research section 2 gaps notes the semantics were not quoted there |
| cTag: present on personal items everywhere, including folders. On business and libraries, absent in delta, absent on folders, present on files in get-item and list-children. | Delta "properties omitted" table; driveItem resource ("In OneDrive for Business and SharePoint, cTag is not returned for folders"). The page also says generally that cTag "isn't returned if the item is a folder", while the get-item example shows one on a business root: the personal-folder choice is **unverified** |
| quickXorHash computed with `mydrive-core`'s `QuickXorHasher`; it is the only hash reported (no `sha1Hash`, `sha256Hash`, `crc32Hash`). | [hashes resource](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0); research section 2 |
| Zero-byte files report a quickXorHash; switch `hash_empty_files` turns it off. | **Unverified**: research section 4 gaps ("files lacking a quickXorHash, were not covered") |
| Timestamps in whole seconds, formatted `2016-03-21T20:01:37Z`, from a simulated clock starting 2026-01-01T00:00:00Z that tests move with `advance_clock`. The wall clock is never read. | abraunegg `usage.md` (research section 2); get-item example for the format |
| Names are unique among siblings without regard to case; a clash is refused. Names may not be empty or contain `/`. Other naming rules are not enforced. | rclone docs (research section 4, case-insensitive namespace) |
| Personal item IDs look like `<DRIVEID>!<n>`, business ones like `01...` (34 characters). Clients must treat them as opaque. | Get-item and delta examples; research section 4 |

### Endpoints

| Behaviour | Source |
|---|---|
| `GET /drives/{d}/root`, `GET /drives/{d}/items/{id}`. A deleted or unknown item is `404 itemNotFound`. | Get item |
| `GET /drives/{d}/root/children`, `GET /drives/{d}/items/{id}/children`, sorted by name, default page size 200 (switch `children_page_size`), `$top` sets the page size, `@odata.nextLink` carries `$skiptoken`. | [List children](https://learn.microsoft.com/en-us/graph/api/driveitem-list-children?view=graph-rest-1.0) ("default page size (200 items)") |
| `GET /drives/{d}/root/delta`: with no token enumerates everything (root included); follow `@odata.nextLink` until a page carries `@odata.deltaLink`; the deltaLink returns only what changed since. Page size switch `delta_page_size` (default 200). | [delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0); research section 2 |
| An item that changed more than once since the token appears once per change, each time showing its state when the page is served ("the same item may appear more than once ... use the last occurrence"). Switch `delta_repeats = LastChangeOnly` gives one occurrence at the latest change instead, which can put a parent after its child. | delta remarks; research section 2 |
| Changes made while a client pages through delta show up in later pages of the same enumeration. | delta ("not guaranteed ... if any writes take place during the enumeration", said of children; the delta feed reflects the latest state) |
| No `path` in `parentReference` in delta. Get-item and list-children leave it out too by default; switch `parent_path_outside_delta` adds it as real Graph does (`/drive/root:/A%20B` personal, `/drives/{id}/root:/...` business). | delta remarks; itemReference (`path` percent-encoded). See "Departures from Graph" |
| Renaming or moving an item puts only that item in delta, never its descendants. | delta remarks ("renaming a folder does not return its descendants") |
| Deletes appear with the `deleted` facet. Business omits `cTag` and `name`; personal omits `cTag` and `size`. A deleted folder's descendants are reported deleted too, the folder first. | delta, deleted resource, "properties omitted" tables. Reporting each descendant, and the order, are **unverified** choices that make a client apply the whole feed before removing a folder (delta: "only delete a folder locally if it is empty after syncing all the changes") |

### Content download

| Behaviour | Source |
|---|---|
| `GET .../items/{id}/content` needs the bearer token in both modes. | Download driveItem content; MC1492958 via research section 2 |
| `download = Redirect` (default): `302 Found` with `Location` on the download origin, a fresh `tempauth` key each time; that URL needs no token. Get-item and list-children include `@microsoft.graph.downloadUrl` for files in this mode. A made-up key gets `401`. | Download driveItem content ("302 Found ... preauthenticated download URL", "don't need to include an Authorization header") |
| `download = Direct`: `/content` returns the content itself. | MC1492958 mirror (research section 2): after 2027-04-01 apps must "handle direct content responses instead of following 302 redirects" |
| `range = Honoured` (default): a single `bytes=a-b`, `bytes=a-` or `bytes=-n` range gets `206` with `Content-Range: bytes a-b/len` and exactly those bytes; a range outside the file gets `416` with `Content-Range: bytes */len`. Several ranges or a malformed header are ignored (200). | Download driveItem content (206 example); RFC 9110 |
| `range = Ignored`: `Range` is ignored and the whole body comes back with `200`. | Download driveItem content ("may be ignored and an HTTP 200 response would be returned with the full contents") |
| In redirect mode `Range` on `/content` itself is not honoured; it applies at the download URL. The range switch applies to whichever URL serves content, so "Range on authenticated `/content`" (gate G1, **unverified**) is covered by `Direct` with either `Honoured` or `Ignored`. | Download driveItem content ("append the Range header to the actual downloadUrl ... and not to the request for /content"); ADR-0011, ADR-0017 consequences |
| Switch `content_etag_header` (default off) adds the item's eTag as an `ETag` header on content responses. | **Unverified**: F03 open question 3, part of gate G1 |

### Faults and the request log

| Behaviour | Source |
|---|---|
| A FIFO queue of scripted faults. Each fault has an optional method and path-fragment matcher and applies to exactly the first later request that matches, then is removed. Actions: a status with a Graph error body and optional `Retry-After`; a delay before handling; a truncated body (announces the full `Content-Length`, sends fewer bytes, closes the connection). Faults apply before the token check, on both origins. | ADR-0017 decision 2 and R5; `Retry-After` on 429/503 from the throttling guidance (research section 3) |
| A log of every request: origin, method, path, query, the headers `authorization`, `range`, `if-match`, `if-none-match`, `user-agent`, `prefer`, `deltaexcludeparent`, and the response status. | WP-007 |

## Departures from Graph

- **`parentReference.path` outside delta.** Graph includes it in get-item and list-children
  responses. WP-007 AC2 asks that no response contain it, so it is off by default and the
  switch `parent_path_outside_delta` restores Graph's behaviour. Clients must not rely on
  paths anyway (ADR-0011 R1).
- **IDs and tokens** are deterministic and recognisably fake. Delta tokens are `sim<hex>`;
  an unknown token gets `400 invalidRequest`. Graph's response to a bad token was not
  researched.
- **Download URLs** never expire and always serve the item's current content.

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
- SharePoint rewriting Office files after upload (size and hash change).
- Sharing links, permissions, the `shared` facet, `remoteItem` and shortcuts.
- `subscriptions/socketIo` change notifications.
- A dropped connection before any response (ADR-0017 lists it; WP-007 does not). A
  truncated body is available.
- Read-only and policy-blocked libraries (gate G2).

Known Graph behaviours not simulated, and not yet assigned:

- Mixed-case drive IDs across fields on personal accounts (issue #1902, research section 4).
- Ancestors appearing in delta when a child changes, and the `deltaExcludeParent` header
  that suppresses them; parent folders' eTag and `childCount` changes in delta.
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
- Download URL expiry (minutes) and the `WWW-Authenticate` header on 401.
- Name rules beyond case-insensitive uniqueness: invalid characters, reserved names, path
  length limits (research section 4).
- Conditional-access `403 insufficient_claims`.
- `sharedWithMe`, which stops returning data after November 2026 (ADR-0011 R13 forbids
  calling it).
