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
| `delta_repeats` | `EveryChange` | Graph documents both "latest state for each item, not each change" and "the same item may appear more than once". How often repeats happen is not documented, so the default is the harder case (see [`docs/delta.md`](docs/delta.md)). `LatestStateOnce` is the one-occurrence model. |
| `folder_deletes` | `FolderOnly` | Whether Graph reports each descendant of a deleted folder is unverified. Under `FolderOnly` only the folder is reported: its descendants never appear in the feed after the delete, even if they changed since the token, so the client must work out that everything below is gone (harder). `EveryDescendant` reports the folder, then each descendant. |
| `parent_path_outside_delta` | `true` | Graph includes `parentReference.path` in get-item and list-children; only delta omits it. `false` removes it everywhere. |
| `personal_folder_ctag` | `false` | The driveItem resource says cTag "isn't returned if the item is a folder". The get-item example shows a cTag on a folder, so `true` exists for the other case. |
| `content_etag_header` | `false` | Whether content responses carry an `ETag` header is unverified (F03 open question 3, gate G1); absent is harder. |
| `hash_empty_files` | `false` | Whether zero-byte files report a quickXorHash is unverified (research section 4 gaps); absent is harder. |
| `simple_replace_honours_if_match` | `false` | Whether a simple `PUT .../content` replace checks `If-Match` is unverified (F04 question 3, ADR-0012 context); not checking, so a stale eTag overwrites, is harder (see [`docs/write.md`](docs/write.md)). |
| `rename_sets_modified` | `false` | Whether a rename or move changes `lastModifiedDateTime` is unverified. `false` keeps the behaviour the control API had before writes existed; see [`docs/write.md`](docs/write.md) for why this default was not flipped. |
| `quota_bytes` | `1099511627776` | 1 TiB, the `quota.total` the drive resource reports. Exceeding it answers `507` (errors page). |
| `simple_upload_max_bytes` | `250000000` | Simple upload "only supports files up to 250 MB" (driveitem-put-content); the smaller, decimal reading is harder. The `413` above it is **unverified**. |
| `rewrite` | `Later` | SharePoint libraries rewrite some uploaded files (rclone; research section 4). When the change shows is unverified; after the upload response, as a separate change, is harder. `InResponse` and `Off` exist. Libraries only. |
| `rewrite_extensions` | `["doc", "docx", "xls", "xlsx", "ppt", "pptx", "eml", "msg", "tif", "tiff"]` | Office files (rclone), and eml, msg and TIFF as abraunegg reports (ADR-0012 context). |
| `folder_conflict_default` | `Rename` | Create folder's default conflict behaviour is undocumented; silently creating `<name> 1` is harder than an error. |

## Simulated

Each feature has its own document under `docs/`, listing what is simulated with its source
and how the simulator departs from Graph there. Each lives in its own source file, with its
routes and its part of the `GraphSim` control API.

| Feature | Document | Source file |
|---|---|---|
| Server and auth: the two origins, bearer tokens, error bodies | [`docs/server.md`](docs/server.md) | `src/server.rs`, `src/auth.rs` |
| Drives and items: drive and item JSON, IDs, eTags, get-item and list-children | [`docs/items.md`](docs/items.md) | `src/items.rs` |
| Delta: token-less enumeration, tokens, repeats, deletes | [`docs/delta.md`](docs/delta.md) | `src/delta.rs` |
| Content download: redirect and direct modes, ranges | [`docs/content.md`](docs/content.md) | `src/content.rs` |
| Faults and the request log | [`docs/faults.md`](docs/faults.md) | `src/fault.rs`, `src/log.rs` |
| Writes: simple upload, create folder, update, delete, `If-Match`, conflicts, locks, quota, library rewriting | [`docs/write.md`](docs/write.md) | `src/write.rs`, with the state in `src/model.rs` |

Declared for the M1 simulator work packages, each adding its document beside these: the
write side (`src/write.rs`, WP-019), upload sessions (`src/session.rs`, WP-020), the second
delta model (`src/delta.rs`, WP-021), throttling (`src/throttle.rs`, WP-022), and identity,
the fake authority and discovery (`src/identity.rs`, `src/authority.rs`,
`src/discovery.rs`, WP-023).

## Not simulated yet

Planned for M1 simulator work packages (WP-007 out of scope):

- Upload: simple `PUT .../content`, upload sessions (`createUploadSession`, 320 KiB
  fragments, `nextExpectedRanges`, `uploadUrl` with or without `Authorization`,
  `deferCommit`), `If-Match`/`If-None-Match` and `412`, `@microsoft.graph.conflictBehavior`,
  `409 nameAlreadyExists`, `507` quota.
- `410 Gone` resync as delta behaviour (a token that expires on its own). The response
  itself, with `resyncChangesApplyDifferences` or `resyncChangesUploadDifferences` and a
  `Location` restart link, can be scripted with a status fault (see Faults above).
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
