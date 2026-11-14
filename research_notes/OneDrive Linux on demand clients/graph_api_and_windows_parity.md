# Microsoft Graph / OneDrive API surface and Windows OneDrive client parity (as of November 2026)

Reading notes for the report writer:

- "Cited Findings" are things I read in the linked page during this session (November 2026). Where the page was a third-party mirror or a project's own claim, that is stated.
- "Inferences" are my reasoning from the cited findings.
- Items marked **[background, not verified this session]** come from my prior knowledge of the docs/projects; I did not open a source for them in this session. They are placed under Inferences or Gaps, never under Cited Findings. Treat them as leads to verify, not as sourced facts.
- Two findings are time-critical and recent: the **retirement of pre-authenticated (tempauth) URLs on 1 April 2027** and the **`sharedWithMe` API ceasing to return data after November 2026**. Both directly affect the design of an on-demand client.

---

## 1. Auth: app registration, flows, scopes, token lifetimes, enterprise blockers, shared client IDs, national clouds

### Takeaway
A third-party client is a public/native client using auth code + loopback redirect (device code works for Entra ID accounts but, per abraunegg, not for personal MSAs without Microsoft's explicit authorisation), with delegated `Files.ReadWrite(.All)`, `Sites.ReadWrite.All` (or `Sites.Read.All`) and `offline_access`. The existing open-source clients all ship a shared default client ID and let users substitute their own; the refresh token lasts 90 days and rolls on each use.

### Cited Findings
- Refresh token default lifetime is 90 days for all scenarios other than SPAs and email-OTP flows (those are 24 hours). "Refresh tokens replace themselves with a fresh token upon every use"; the old one is not revoked on use. — [Microsoft Learn: Refresh tokens](https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens)
- Refresh tokens "are bound to a combination of user and client, but aren't tied to a resource or tenant", so one refresh token can obtain access tokens for other resources (relevant to section 2: obtaining a SharePoint-resource token as well as a Graph token). — [Microsoft Learn: Refresh tokens](https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens)
- Revocation table for public-client tokens: a user password change, SSPR, or admin password reset revokes password-based tokens; "User revokes their refresh tokens" and "Admin revokes all refresh tokens for a user" revoke everything. Apps "should handle such revocations gracefully by redirecting the user to an interactive sign-in prompt". — [Microsoft Learn: Refresh tokens](https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens)
- rclone: "If you don't use rclone for 90 days, the refresh token expires"; fix is `rclone config reconnect`. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- abraunegg/onedrive supports three auth methods: interactive browser OAuth2 with a redirect URI, Intune SSO via the Microsoft identity broker (`use_intune_sso`), and OAuth2 Device Authorisation Flow for Entra ID accounts (`use_device_auth`). Its docs state that "Microsoft currently does not allow this flow to be used with personal Microsoft accounts (MSA)" unless Microsoft explicitly authorises the application. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- abraunegg redirect URIs: `http://127.0.0.1:53100/` (loopback browser flow) and `https://login.microsoftonline.com/common/oauth2/nativeclient` (manual copy/paste fallback). A custom registration must be a Public Client Application with both URIs configured. The example authorisation URL uses `client_id=d50ca740-c83f-4d1b-b616-12c519384f0c`. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- abraunegg scopes: baseline (read-only) app permissions are `Files.Read`, `Files.Read.All`, `Sites.Read.All`, `offline_access`; the effective default requested at sign-in is `Files.ReadWrite`, `Files.ReadWrite.All`, `Sites.ReadWrite.All`, `offline_access`. A `read_only_auth_scope` option exists. The project states none of these permissions require admin consent, and that its app has Microsoft publisher verification configured (publisher domain `https://abraunegg.github.io/`). The publisher-verification statement is the project's own claim. — [abraunegg/onedrive application-security.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/application-security.md)
- rclone ships a shared default client ID; its docs say "You may choose to create and use your own Client ID, in case the default one does not work well for you", and that business accounts may need a single-tenant app with custom `auth_url`/`token_url` containing the tenant ID. Suggested permissions for an own app: `Files.Read`, `Files.ReadWrite`, `Files.Read.All`, `Files.ReadWrite.All`, `offline_access`, `User.Read`, and optionally `Sites.Read.All` (only needed to search SharePoint sites during setup; `disable_site_permission` removes it). — [rclone OneDrive docs](https://rclone.org/onedrive/)
- rclone region options: `global`, `us` (US Government), `de` (deprecated; "try global first"), `cn` (operated by Vnet Group in China). — [rclone OneDrive docs](https://rclone.org/onedrive/)
- Graph file APIs carry a national-cloud availability table; createUploadSession, download content and sharedWithMe are each listed as available in Global, US Government L4, US Government L5 (DOD) and China operated by 21Vianet. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0); [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)
- Per-API delegated permissions differ by account type: for delta and download, work/school accepts `Files.Read*`/`Sites.Read*` scopes, personal accounts accept only the `Files.*` family (no `Sites.*`). — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Throttling is per-app per-tenant: "Each tenant hosting the application is considered distinct"; an app hitting its limit in one tenant "will not affect other instances of the application operating in different tenants". Microsoft also warns against "Creating multiple AppIDs for the same application" to dodge throttling. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- Microsoft APIs Terms of Use s.2(b): access credentials "are non-transferable and non-assignable. Keep them secret." — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)

### Inferences
- A shared client ID in an open-source client is the established practice (abraunegg, rclone both do it), and for work/school tenants the SharePoint throttle budget is scoped per app per tenant, so one shared ID does not pool all users worldwide into one bucket for business accounts. I found no equivalent published statement for consumer OneDrive, so the pooling risk for personal accounts is unquantified.
- A shared public client ID cannot be kept secret (public clients have no secret); the practical risks are (a) Microsoft or a tenant admin disabling/blocking that one app ID breaks every user at once, (b) anyone can impersonate the app ID. Offering a user-supplied client ID override, as rclone and abraunegg do, is the mitigation both projects chose.
- Because abraunegg's requested scopes reportedly need no admin consent, the typical enterprise blocker is tenant policy rather than the scopes themselves.
- **[background, not verified this session]** Common enterprise blockers: tenants that disable user consent entirely or restrict it to verified publishers/low-risk permissions (then an admin must consent to the app); Conditional Access policies requiring a compliant/hybrid-joined device (a Linux box only satisfies this with Intune enrolment and the Microsoft identity broker, which is why abraunegg added Intune SSO); CA policies restricted to "approved client apps"; and tenant restrictions v2. Device code flow is increasingly blocked by CA "authentication flows" policies. Verify against Entra docs before relying on specifics.
- **[background, not verified this session]** National cloud endpoints: US Gov L4 `login.microsoftonline.us` / `graph.microsoft.us`; DoD L5 `dod-graph.microsoft.us`; China `login.chinacloudapi.cn` / `microsoftgraph.chinacloudapi.cn`; Microsoft Cloud Germany was shut down in 2021 (consistent with rclone marking `de` deprecated). App registrations are per-cloud, so a shared client ID registered in the global cloud does not work in 21Vianet/GCC High; abraunegg has a `national-cloud-deployments.md` document I did not read.

### Gaps
- I did not read the Entra docs for app registration, PKCE loopback rules, publisher verification requirements, or the consent/Conditional Access policy pages, so there is no sourced statement here on exactly which tenant settings block third-party clients.
- I did not verify what onedriver (jstaf) or newer forks do for client IDs; no finding on it.
- No Microsoft source found in this session confirming abraunegg's statement that device code flow is unavailable for personal MSAs; it is a single-project claim.
- Access token lifetime (commonly ~60-90 minutes) and Continuous Access Evaluation behaviour were not checked.

---

## 2. Sync primitives: delta, upload sessions, ranged download, hashes, eTag/cTag, and related item APIs

### Takeaway
Delta + upload sessions + ranged GET are sufficient to build a full on-demand client, but the ranged-download mechanism documented today depends on pre-authenticated URLs that Microsoft has announced it will retire on 1 April 2027 for SharePoint Online and OneDrive. A new client should be designed from day one to download with a bearer token and to tolerate direct content responses instead of 302 redirects.

### Cited Findings

**Delta**
- Call `delta` with no parameters to enumerate, follow `@odata.nextLink` until an `@odata.deltaLink` is returned, then reuse the deltaLink later. Endpoints: `/me/drive/root/delta`, `/drives/{id}/root/delta`, `/sites/{id}/drive/root/delta`, `/groups/{id}/drive/root/delta`, `/users/{id}/drive/root/delta`. — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- "The delta feed shows the latest state for each item, not each change"; "The same item may appear more than once in a delta feed... You should use the last occurrence you see." — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- `parentReference` in delta results has no `path`, because renaming a folder does not return its descendants: "When using delta you should always track items by id." — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Deleted items carry the `deleted` facet; "you should only delete a folder locally if it is empty after syncing all the changes." — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Properties omitted by delta: OneDrive for Business omits `ctag` on create/modify and `ctag`+`name` on delete; OneDrive consumer omits nothing on create/modify and `ctag`+`size` on delete. — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- `?token=latest` returns an empty result plus the current deltaLink. A URL-encoded timestamp may be used as the token, but "only supported on OneDrive for Business and SharePoint". — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Resync: the service returns `410 Gone` with a `Location` header holding a fresh nextLink and one of two codes. `resyncChangesApplyDifferences`: replace local items with the server's version (including deletes) if you are sure the service was up to date with your local changes at last sync, and upload unknown local changes. `resyncChangesUploadDifferences`: upload local items the service did not return and files that differ, "keeping both copies if you're not sure which one is more up-to-date". — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Initial enumeration must use delta: paging `children` is "not guaranteed to return every single item if any writes take place during the enumeration". — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- Headers: `deltaExcludeParent` returns only changed items without parents; `Prefer: hierarchicalsharing`, and `Prefer: deltashowremovedasdeleted, deltatraversepermissiongaps, deltashowsharingchanges` (business/SharePoint only; the sharing-changes header needs `Sites.FullControl.All` to process permissions correctly). — [driveItem: delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)
- rclone notes "Some drives only support delta from the root", and treats delta listing as opt-in. — [rclone OneDrive docs](https://rclone.org/onedrive/)

**Shared items**
- `GET /me/drive/sharedWithMe` is deprecated: it "will operate in a degraded state until November, 2026, after which it will stop returning data." Items it returned always carry a `remoteItem` facet, accessed via `remoteItem.parentReference.driveId` + `remoteItem.id`; `*.All` scopes are required to open them. — [drive: sharedWithMe (deprecated)](https://learn.microsoft.com/en-us/graph/api/drive-sharedwithme?view=graph-rest-1.0)
- rclone: "Shared with me" is not supported; the workaround is "Add shortcut to My files". Some shortcuts (including Personal Vault) fail to list. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- abraunegg advises treating "each shared-folder shortcut as an independent root" for selective-sync rules. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- Microsoft's own sync client cannot sync the "Shared with me" view of a work/school OneDrive. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)

**Upload**
- Upload session: `POST .../items/{parentId}:/{name}:/createUploadSession` (new) or `.../items/{itemId}/createUploadSession` (replace). Accepts `If-Match` / `If-None-Match` with an eTag or cTag, returning `412 Precondition Failed` on mismatch. `@microsoft.graph.conflictBehavior` is `fail` (default), `replace` or `rename`. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)
- Each PUT must be under 60 MiB; fragments must be sequential; fragment size "MUST be a multiple of 320 KiB (327,680 bytes)"; recommended 5-10 MiB; use sessions for files over 10 MiB. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)
- Do not send `Authorization` on the PUT to `uploadUrl` (may cause 401). Each fragment extends `expirationDateTime`. Resume with `GET uploadUrl` to read `nextExpectedRanges`; `416` means the range was already received; `404` means restart the whole upload; `DELETE uploadUrl` cancels. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)
- `deferCommit: true` holds the commit: complete with a zero-length POST on business/SharePoint, or a final PUT on Personal. `fileSize` and `description` in the item body are Personal-only; `fileSize` over quota returns `507 Insufficient Storage`. A name conflict at commit returns `409 nameAlreadyExists`, recoverable by a PUT with `@microsoft.graph.sourceUrl`. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)
- Maximum file size is 250 GB for sync and individual files. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- rclone default chunk 10Mi, multiple of 320k, not above 250M. — [rclone OneDrive docs](https://rclone.org/onedrive/)

**Download and ranged reads**
- `GET .../items/{id}/content` returns `302 Found` to a pre-authenticated URL, the same as `@microsoft.graph.downloadUrl`. These URLs "might expire within minutes" and need no `Authorization` header. `If-None-Match` with eTag/cTag gives `304`. — [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)
- Range: "You must append the `Range` header to the actual `@microsoft.graph.downloadUrl` URL and not to the request for `/content`." Returns `206`; "If the range can't be generated the Range header may be ignored and an HTTP 200 response would be returned with the full contents". — [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)
- **Recent change (announced October 2026):** Message Center post MC1492958, published 9 October 2026, says Microsoft will retire pre-authenticated ("tempauth") URLs in SharePoint Online and OneDrive on 1 April 2027, rolling out through late April 2027 worldwide and in GCC, GCC High and DoD. Selected Graph file APIs "will no longer return URLs that contain embedded authentication information" and will stop issuing 302 redirects to them. Affected families: download/content, createUploadSession, copy and long-running action monitors, preview, thumbnail, version, and format conversion. Apps must authenticate with Graph access tokens and "handle direct content responses instead of following 302 redirects"; apps opening SharePoint URLs directly need an Entra ID token for the SharePoint Online resource. Admins get opt-in SharePoint Online PowerShell tenant controls to move specific app IDs to the new behaviour early. I read this through a third-party mirror of the Message Center, not Microsoft's own page. — [MC1492958 mirror (mc.merill.net)](https://mc.merill.net/message/MC1492958)
- rclone corroborates: Microsoft retires these URLs for OneDrive for Business and SharePoint on 1 April 2027; upgrade to rclone v1.76 or later. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- The Graph download page I read (page metadata last updated August 2025) still documents the 302 + pre-authenticated URL + Range-on-downloadUrl model and has no retirement notice. — [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)

**Hashes**
- `quickXorHash` "is the only value that is guaranteed to be available for both OneDrive for work or school and OneDrive for home." `sha256Hash`: "This property isn't supported. Don't use." `sha1Hash` and `crc32Hash` are "if available". "Not all services provide a value for all hash properties listed." — [hashes resource type](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0)
- rclone: SHA1 was phased out for Personal from July 2023; since rclone 1.62 all account types default to QuickXorHash. — [rclone OneDrive docs](https://rclone.org/onedrive/)

**Versions, recycle bin, timestamps**
- Each change creates a new version that counts against quota, including setting modification time after a copy. Deleted files go to the recycle bin; rclone exposes a `hard_delete` option for permanent deletion. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- "Microsoft OneDrive does not support fractional-second precision in file timestamps—only whole seconds." — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- Thumbnails are not generated for images over 100 MB; no PDF previews over 100 MB. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)

### Inferences
- On-demand partial reads (FUSE `read()` on a placeholder) currently map to: fetch `@microsoft.graph.downloadUrl`, then `GET` with `Range`. After April 2027 on business/SharePoint the client will instead need `GET .../content` with a bearer token and a `Range` header, and must accept a 200/206 body directly. The current doc says Range is not honoured on `/content`, so the post-retirement Range behaviour is the single most important thing to test empirically before committing to an architecture. The MC post as I read it does not state it.
- Because the server may ignore Range and return 200 with the full body, the reader must check the status code and not assume 206.
- The same retirement lists createUploadSession, so `uploadUrl` will presumably stop being pre-authenticated and the long-standing "do not send Authorization on PUT" rule will invert for affected tenants. A client should decide per response rather than hard-coding either behaviour.
- With `sharedWithMe` ending this month, the only durable route to shared content is shortcuts in the user's own drive ("Add shortcut to My files"), which surface as `remoteItem` entries in the main delta feed and require a separate delta against the remote drive. That matches what rclone and abraunegg already steer users towards.
- Missing `ctag` in business delta responses means content-change detection there must rely on `eTag`, `quickXorHash`, size and `lastModifiedDateTime`, not cTag.
- Timestamp-as-token gives business/SharePoint a cheap "what changed since T" recovery path that does not exist for Personal.
- **[background, not verified this session]** Simple (non-session) `PUT .../content` upload is documented for files up to 250 MB; special folders via `/me/drive/special/{name}` (documents, photos, approot, etc.); quota via `GET /me/drive` `quota` facet; versions via `/items/{id}/versions` with `restoreVersion`; sharing via `createLink` and `invite`; permissions via `/items/{id}/permissions`; thumbnails via `/items/{id}/thumbnails`; `permanentDelete` action; business recycle-bin enumeration/restore is not in Graph v1.0 for drives (SharePoint REST only), Personal has `restore`. Check each API page.

### Gaps
- Not verified: post-April-2027 behaviour of `Range` on `/content`, and whether consumer OneDrive is in scope. The MC mirror says the message "covers OneDrive but does not say whether OneDrive consumer accounts are affected"; rclone's wording names only OneDrive for Business and SharePoint.
- I did not read Microsoft's primary Message Center/dev-blog post for the tempauth retirement; only a mirror and rclone's docs.
- No source read this session for: the 250 MB simple-upload ceiling, special folders, quota, version history API, createLink/invite, permissions, thumbnails API, recycle bin API, long-polling. These are standard Graph features but are unsourced here.
- eTag vs cTag semantics (eTag changes on any item change, cTag on content change) are not quoted from a source in this session beyond their use in `If-Match`/`If-None-Match`.
- Shared-folder delta specifics (delta on a remote drive scoped to a shared folder, and personal-vs-business differences there) were not found in a primary source.

---

## 3. Change notification and throttling

### Takeaway
Graph offers a WebSocket (socket.io) endpoint for drive change notifications under delegated permissions on both personal and work accounts, which avoids the public-HTTPS-endpoint requirement of webhooks and is the right fit for a desktop client. Throttling for business/SharePoint is published in resource units; delta-with-token is deliberately cheap at 1 unit, and the user-agent decoration format is prescribed.

### Cited Findings
- `GET /me/drive/root/subscriptions/socketIo` (also `/drives/{id}/root/...`, `/drives/{id}/list/...`, `/groups/{id}/drive/root/...`, `/sites/{id}/lists/{id}/drive/root/...`) "Allows you to receive near-real-time change notifications for a drive and list using socket.io." It is in v1.0. Delegated permissions are listed for both work/school and personal Microsoft accounts (`Files.Read` least privileged); Application permission is "Not supported". — [Get websocket endpoint](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0)
- The response contains a `notificationUrl` which is a socket.io endpoint; the client must use `transports: ['websocket']` because the default "polling" transport "isn't supported". The sample listens for a `notification` event. — [Get websocket endpoint](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0)
- Webhooks require a publicly resolvable HTTPS URL; abraunegg's setup needs an FQDN or DDNS, an nginx reverse proxy, a Let's Encrypt certificate, TLS 1.2/1.3 only, and port forwarding to a local listener (default TCP 8888). Options: `webhook_enabled`, `webhook_public_url`. — [abraunegg/onedrive webhooks.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/webhooks.md)
- Subscription scope: per a search-result summary of Microsoft docs/Q&A, on personal OneDrive you can subscribe to the root or any subfolder, on OneDrive for Business only the root folder. I did not open the underlying page. — [Microsoft Q&A (via search result)](https://learn.microsoft.com/en-us/answers/a/340669)
- abraunegg polls on a `monitor_interval`; the docs' example output shows 300 seconds, and a `monitor_fullscan_frequency` option controls periodic full true-up scans. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- Throttled requests get `429` or `503`, both with `Retry-After`. "Throttled requests count towards usage limits, so failure to honor Retry-After may result in more throttling." Persistent offenders may be blocked entirely (continuous 503, tenant notified in Message Center). — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- Resource-unit cost per Graph request: 1 for single-item query, delta with a token, and file download; 2 for multi-item queries (list children, delta without a token) and create/update/delete/upload; 5 for any permission operation including `$expand=permissions`. Requests inside a JSON batch are costed individually. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- Per-user limits: 3,000 requests per 5 minutes; 50 GB ingress per hour; 100 GB egress per hour; 50 delegation token requests per 5 minutes. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- Per-app-per-tenant limits scale with licence count: 1,250 RU/min and 1,200,000 RU/24h for 0-1,000 licences, up to 6,250 RU/min and 6,000,000 RU/24h for 50,000+; 400 GB/hour ingress and egress per app per tenant. Tenant-wide: 18,750 to 93,750 RU per 5 minutes. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- User-Agent decoration: `ISV|CompanyName|AppName/Version` for ISV apps, `NONISV|CompanyName|AppName/Version` for enterprise apps. "Well-decorated traffic will be prioritized over traffic that isn't properly decorated." Traffic is undecorated if it lacks an AppID/AppTitle and a User-Agent in this format. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- rclone recommends the same for SharePoint throttling: `--user-agent "ISV|rclone.org|rclone/v1.55.1"`. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- Microsoft names sync engines explicitly among apps that will "likely" be throttled if they ignore the scanning best practices, notes that "one user syncing a large amount of data across 10 machines at the same time could trigger throttling", and says off-peak (nights/weekends in the tenant's region) is less likely to throttle. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- **Conflict within the same page on RateLimit headers:** the best-practice summary says "Use the `Retry-After` and `RateLimit` HTTP headers" and that RateLimit headers "send you early signals", while the dedicated section says "SharePoint Online does not return or support IETF RateLimit headers... applications should not depend on them". — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- The general Graph throttling-limits page gives no numeric limits for drive/driveItem resources; it points to the SharePoint article. — [Microsoft Graph service-specific throttling limits](https://learn.microsoft.com/en-us/graph/throttling-limits)

### Inferences
- The "does not return or support RateLimit headers" section reads as the newer text (the page was updated August 2026 and earlier versions documented `RateLimit-Limit/Remaining/Reset`). A client should treat `Retry-After` as the only reliable signal and treat any RateLimit headers as optional hints.
- For a desktop client the sensible notification stack is: socket.io WebSocket as a wake-up signal, followed by a delta call (1 RU with token), with a slow polling fallback (order of minutes) when the socket is down. Webhooks are impractical for end users behind NAT.
- A 3,000 requests / 5 minutes per-user cap is ample for steady-state sync but can be hit by an on-demand client that issues one metadata or ranged-download request per small `read()`; read-ahead and chunk caching are necessary, not optional.
- Permission reads cost 5 RU each, so showing "shared" overlay state should come from the `shared` facet in delta rather than per-item permission calls.

### Gaps
- No published throttling numbers found for consumer OneDrive (personal accounts); the SharePoint article is about SharePoint Online/OneDrive for Business.
- I found no official statement of socket.io notification payload contents, connection lifetime, or reconnection/renewal requirements; the doc page shows only the sample. Whether abraunegg now uses this endpoint was not confirmed (its webhooks doc does not mention WebSockets; the part of usage.md I read did not cover it).
- Webhook subscription maximum lifetime for driveItem was not sourced (abraunegg's doc shows only one example expiring about 76 minutes after creation).
- Long polling: I found no current Graph documentation of a long-poll mechanism for drives. **[background, not verified this session]** none exists in Graph v1.0 for driveItem.
- Microsoft's scan guidance page (aka.ms/ScanGuidance) was not read; it likely contains recommended polling cadence.

---

## 4. Known API quirks that hurt sync clients, and 2024-2026 changes

### Takeaway
The long-standing quirks (SharePoint rewriting Office files after upload, name/character rules, case-insensitivity, OneNote packages, Personal Vault) are all still documented by the open-source clients. The 2025-2026 period added three breaking or soon-to-break changes: inconsistent driveId casing on Personal accounts, the end of `sharedWithMe`, and the tempauth URL retirement.

### Cited Findings
- SharePoint libraries (not Personal or OneDrive for Business, per rclone) can silently modify uploaded Office files, causing size and hash mismatches; rclone's workaround is `--ignore-checksum --ignore-size`. Replacing or deleting files on SharePoint may also return "item not found". — [rclone OneDrive docs](https://rclone.org/onedrive/)
- Invalid characters: `" * : < > ? / \ |`. No leading or trailing spaces. Reserved names: `.lock`, `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, `desktop.ini`, anything beginning `~$`; `_vti_` cannot appear anywhere in a name; `forms` is not allowed at a library root. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Names also cannot end with a period; path limits are "400 characters for OneDrive Business & SharePoint" and "430 characters for OneDrive Personal". — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)
- rclone states the full path including filename must be under 400 characters, and maps invalid characters to look-alike Unicode characters. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- Case-insensitive namespace: `Hello.doc` and `hello.doc` cannot coexist. — [rclone OneDrive docs](https://rclone.org/onedrive/). Microsoft's own client does not support case-sensitive volumes on macOS or folders with `SetCaseSensitiveInfo` on Windows. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- OneNote: rclone hides OneNote files by default "because Open and Update don't work on them". The Windows client shows a stub `.url` file that opens the notebook online; notebooks are limited to 2 GB. — [rclone OneDrive docs](https://rclone.org/onedrive/); [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Personal Vault appears as a shortcut that fails to list via the API. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- Scale: Microsoft recommends no more than 300,000 items across synced storage (a public preview for Windows/macOS raises this to 1,000,000 per sync instance). rclone reports folders of ~100,000 files may fail to list with `UnknownError`. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa); [rclone OneDrive docs](https://rclone.org/onedrive/)
- Authenticated proxies are not supported by Microsoft's client; `.tmp` files are not synced; symbolic links and junction points are not supported. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- **2025 Personal driveId casing regression:** since about 14 June 2025, OneDrive Personal Graph responses return the same drive ID in mixed case across fields (e.g. lowercase in top-level `id` and `parentReference.driveId`, uppercase in `webUrl` and `item.id`), previously uppercase (example `66D53BE8A5056ECA`). Affected: `/me/drive`, `/me/drive/root`, `/root/children`, `/drives/{id}/items/{id}`, and delta. The issue was opened 16 June 2025 by the abraunegg maintainer and showed no Microsoft response when read. — [OneDrive/onedrive-api-docs issue #1902](https://github.com/OneDrive/onedrive-api-docs/issues/1902)
- abraunegg's fix was to normalise all driveIds to lowercase before storing (PR #3347, shipped in v2.5.7), after users hit "FOREIGN KEY constraint failed" in the local database. — [abraunegg/onedrive issue #3336](https://github.com/abraunegg/onedrive/issues/3336)
- A separate February 2025 abraunegg issue (#3115) is titled as an application crash "due to 15 character driveId API bug" on shared-folder path calculation, fixed in PR #3116. I saw this only in a search-result summary. — [abraunegg/onedrive issues (search result)](https://www.GitHub.com/abraunegg/onedrive/issues)
- During 2024-2025, shared folders in personal OneDrive appeared as `.url` internet shortcuts in Microsoft's own Windows and Mac clients for months; The Register reported it in April 2025. Seen via search results only. — [The Register, April 2025](https://www.theregister.com/2025/04/14/microsoft_onedrive_file_sync_apps_broken/)
- `sharedWithMe` deprecation (stops returning data after November 2026) and tempauth URL retirement (1 April 2027): see section 2. — [drive: sharedWithMe (deprecated)](https://learn.microsoft.com/en-us/graph/api/drive-sharedwithme?view=graph-rest-1.0); [MC1492958 mirror](https://mc.merill.net/message/MC1492958)
- abraunegg's current known-issues page lists only three items, none of them a Graph data-model quirk: rename/move in one-shot mode being processed as delete + re-upload, unexplained exits tied to curl/OpenSSL connection resets, and AADSTS70000 on stale auth codes. — [abraunegg/onedrive known-issues.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/known-issues.md)

### Inferences
- Treat driveId (and by extension item IDs containing the drive ID prefix) as case-insensitive keys; normalise on ingest. This is a concrete, tested workaround from the most widely used Linux client.
- The Personal-account ID instability and the months-long shared-folder breakage in Microsoft's own clients are consistent with a backend change for consumer OneDrive, but none of the sources I read states that Microsoft migrated personal accounts to a SharePoint-based backend. The one hint is a search-result snippet of a Microsoft Tech Community thread saying shared-folder sync breaks while the connected OneDrives are "on different versions of the backend".
- SharePoint post-upload file modification means an upload's returned size/hash can differ from the local file for Office formats in document libraries. A sync engine must record the server's post-upload eTag/hash as the baseline rather than re-comparing against the local file, or it will loop download/upload.
- There is a documented discrepancy on path limits (rclone: under 400 for all; abraunegg: 400 business, 430 personal). Using 400 as the conservative limit is safe.

### Gaps
- No primary Microsoft source found confirming a migration of OneDrive Personal to a SharePoint/ODSP backend, any change of driveId format (e.g. to `b!...` style), or a timeline. Reported here as unverified.
- Zero-byte / unknown-size files, and files lacking a quickXorHash, were not covered by any source I read.
- I did not read abraunegg's changelog or its SharePoint-specific docs, where the SharePoint enrichment handling and Personal shared-folder workarounds are described in more detail.
- Issue #3115's details (what exactly a 15-character driveId is and how it arises) were not read directly.

---

## 5. Windows client feature inventory and third-party feasibility

### Takeaway
Most user-visible Windows OneDrive features are local behaviours layered over the same cloud operations Graph exposes, so they are feasible for a third party; the exceptions are Personal Vault, block-level differential sync, and tight Office co-authoring integration. Only the icon semantics and a handful of limits below are sourced from Microsoft in this session; the feasibility column is my assessment.

### Cited Findings
- File state icons: blue cloud outline = online-only, uses no local space, cannot be opened offline; white circle with green tick = locally available, can be freed; solid green circle with white tick = "Always keep on this device"; sync arrows = sync in progress; person icon = shared with other people; red circle with white cross = file or folder cannot be synced; folder with chain link = shortcut to a shared folder; grey circle with flat bar = admin has blocked sync of the file type (commonly `.pst`); three little blue lines = new file. — [What do the OneDrive icons mean?](https://support.microsoft.com/en-us/office/what-do-the-onedrive-icons-mean-11143026-8000-44f8-aaa9-67c985aa49b3)
- Tray/account icons: paused symbol = syncing paused; grey line through cloud = not signed in; yellow triangle (Windows 10) or brown exclamation (Windows 11) = account needs attention; red no-entry = account blocked; blue "i" = informational messages. — [What do the OneDrive icons mean?](https://support.microsoft.com/en-us/office/what-do-the-onedrive-icons-mean-11143026-8000-44f8-aaa9-67c985aa49b3)
- Differential sync is supported by the Microsoft client for `.docx`, `.pptx`, `.xlsx` and globally for other file types, but not OneNote files. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Multiple accounts: one personal account and nine work or school accounts per device. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Admins can block specific file types from syncing for work/school accounts; IRM-protected libraries sync read/write on recent builds. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Graph upload sessions send whole-file byte ranges sequentially; no delta/patch-range upload of an existing file is described. — [createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)
- Ranged download is available (subject to the April 2027 change). — [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)
- Personal Vault fails to list via the API in rclone. — [rclone OneDrive docs](https://rclone.org/onedrive/)
- The icons page does not describe a padlock icon. — [What do the OneDrive icons mean?](https://support.microsoft.com/en-us/office/what-do-the-onedrive-icons-mean-11143026-8000-44f8-aaa9-67c985aa49b3)

### Inferences
Feasibility assessment (mine). Rows other than those backed by the findings above rest on **[background, not verified this session]** knowledge of the Windows client and Graph.

| Windows feature | Third-party feasibility via Graph | Notes |
|---|---|---|
| Files On-Demand placeholders (cloud / tick / pinned states) | Feasible | Local concern: FUSE or similar plus a metadata DB from delta; hydration via ranged GET. State icons need file-manager integration (emblems/extensions), not Graph. |
| Free up space (dehydrate) | Feasible | Purely local: drop cached content, keep metadata, provided the local copy is known to be uploaded. |
| Always keep on this device | Feasible | Local pin flag plus background download. |
| Storage Sense auto-dehydration | Feasible | Local policy on last-access time; no API involved. |
| Sync-in-progress, error, paused, shared icons | Feasible | Shared state from the item's `shared` facet / remoteItem; the rest is local state. |
| Padlock (read-only / locked) icon | Partly | Not on Microsoft's icons page as read; read-only state would have to be derived from permissions, which cost 5 RU per call. |
| Known Folder Move / backup | Feasible | Local redirection of Desktop/Documents/Pictures into the sync root (XDG user dirs on Linux); no server feature required. |
| Personal Vault | Not feasible | No public API; rclone cannot list it. |
| Differential (block-level) sync | Not feasible for upload | Graph requires re-uploading the whole file. Download side can be approximated with Range but there is no server-provided block map. |
| Office co-authoring integration and lock handling | Mostly not feasible | Windows relies on Office desktop apps talking to the service directly; on Linux the realistic equivalents are "open in Office for the web" via `webUrl` and handling `423 Locked` / `resourceLocked` errors on upload. |
| Sharing dialog from the file manager | Feasible (own UI) | createLink / invite / permissions APIs with a custom dialog; the native Microsoft share UI itself is not available. |
| Version history | Feasible | versions list/restore APIs. |
| "View online" | Feasible | Open `webUrl`. |
| Pause sync, bandwidth limits, metered-network handling | Feasible | Entirely client-side. |
| Selective sync | Feasible | Client-side filter over delta results; largely superseded by on-demand. |
| Multiple accounts | Feasible | Separate token caches and sync roots; Microsoft's own limit is 1 personal + 9 work. |
| SharePoint / Teams library sync | Feasible | `/sites/{id}/drive` and `/drives/{id}` delta with `Sites.Read(Write).All`; watch for the SharePoint file-modification quirk. |
| "Add shortcut to My files" shared folders | Feasible, fragile | remoteItem handling; this area broke even in Microsoft's own clients in 2024-2025. |
| Activity centre | Feasible | Local log of the client's own operations; server-side activity feeds are optional. |
| Conflict naming rules | Feasible | Client policy; `conflictBehavior=rename` server-side or local rename on `412`. Windows appends the device name to the losing copy. |
| Files restore / ransomware detection | Partly | Per-file version restore and recycle-bin restore are possible; the whole-drive "Restore your OneDrive" point-in-time rollback and Microsoft's ransomware alerts are service/web features without a public API as far as I know. |

- The delta doc's resync rules ("keeping both copies if you're not sure which one is more up-to-date") are effectively Microsoft's stated conflict policy for third parties.

### Gaps
- I did not read Microsoft's support/admin pages for Files On-Demand, Storage Sense, Known Folder Move, Personal Vault, Files Restore, bandwidth/metered settings, or the Windows conflict-naming rule, so none of those feature descriptions is sourced here.
- Whether any public API exists for Personal Vault unlock, whole-drive restore, or ransomware-detection signals was not checked against current Graph beta docs.
- Lock semantics for co-authoring (`423`, checkout/checkin, `resourceLocked`) were not sourced.

---

## 6. Microsoft Linux client / SDK plans and documentation of the official sync protocol

### Takeaway
I found no evidence in this session of an official Linux OneDrive sync client or of public documentation for the protocol the Windows/macOS client uses. This question is essentially unanswered by sources and should be reported as such.

### Cited Findings
- Microsoft's restrictions-and-limitations article for the sync app covers Windows and macOS specifics and does not mention Linux. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Microsoft's guidance for third-party "sync engines" is to use Graph (delta with a token) rather than CSOM/REST: "We highly recommend you choose Microsoft Graph APIs over CSOM and REST APIs when possible", and it lists sync engines among apps expected to follow the scan guidance. — [Avoid getting throttled or blocked in SharePoint Online](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)
- Microsoft's own client supports capabilities Graph does not document for third parties: differential sync and (in preview) up to 1,000,000 items per sync instance. — [Restrictions and limitations in OneDrive and SharePoint](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa)
- Microsoft does ship Linux identity integration that a third-party client can use: abraunegg's Intune SSO mode authenticates through the Microsoft identity broker. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)

### Inferences
- The existence of differential sync in the official client, with no corresponding Graph API, implies the official client uses service interfaces beyond public Graph. That is an inference from the feature gap, not a documented fact.
- Graph SDKs (the delta doc shows C#, Go, Java, JavaScript, PHP, PowerShell and Python snippets) run on Linux, but they are generic REST wrappers; there is no sync SDK.

### Gaps
- No search was run specifically for Microsoft roadmap items or statements about a Linux OneDrive client, so "none exists" is unverified. **[background, not verified this session]** As of my knowledge (mid-2026) Microsoft had never shipped or announced one; the long-standing feedback-portal request remained open.
- No public documentation of the official client's sync protocol was found or searched for in depth. **[background, not verified this session]** Third-party analyses describe the Windows client using private endpoints (e.g. a `/_api/SPFileSync/sync/...` family on SharePoint and legacy `skyapi`/`storage.live.com` for consumer) and MS-FSSHTTP for Office co-authoring; MS-FSSHTTP is published in Microsoft's Open Specifications. Needs verification.

---

## 7. Legal / ToS considerations and branding

### Takeaway
The Microsoft APIs Terms of Use permit building such a client but give Microsoft unilateral rights to throttle, change or cut off API access at any time, and include a few clauses directly relevant to a sync client (no working around technical limitations, no reverse engineering, privacy-statement and data-deletion duties, and a OneDrive-specific ban on storing system backups).

### Cited Findings
- Licence (s.3a): "a limited, non-exclusive, non-assignable, non-transferable, revocable license to use the Microsoft APIs to develop, test, and support your Application". Terms last updated October 2025. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.3(b)(7): you may not "Attempt to circumvent the limitations Microsoft sets on your use of the Microsoft APIs", which Microsoft sets "in its sole discretion". s.3(b)(6): no "unreasonable amount of bandwidth". — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.3(b)(8): you may not use the APIs "in any manner that works around any technical limitations of the Microsoft APIs or of the accessed Microsoft Offering, or reverse engineer, decompile or disassemble the Microsoft APIs, except and only to the extent that applicable law expressly permits". — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.3(b)(4)-(5): no copying of data "except as necessary to enable an intended usage scenario for your Application"; request no more than the minimum permissions needed. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.3(b)(16): do not "obscure or alter the source of queries coming from an Application to hide a violation". s.3(b)(14): no redistributing, reselling or sublicensing access to the APIs. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.3(e), OneDrive-specific: an application "may not enable storage of system data in Microsoft OneDrive", including "(i) computer system back-up data, (ii) team, organization, or departmental level data". — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.2(b): registration must be accurate and kept up to date; "All activities that occur using your Access Credentials are your responsibility." — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.5: must obtain consents, keep locally stored data current, delete data when the user uninstalls or abandons the app, and publish a privacy statement "as protective as the Microsoft Privacy Statement"; where the app handles Microsoft-account sign-in UI itself, the statement must link to `https://account.live.com/consent/Manage` and/or `https://myapps.microsoft.com` for consent revocation. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.4: Microsoft may monitor, limit or terminate access; may request up to two full-feature instances of the application for compliance checks; vulnerabilities related to the API connection must be reported to MSRC. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- s.6 and s.10: Microsoft "MAY CHANGE OR DISCONTINUE THE AVAILABILITY OF SOME OR ALL OF THE MICROSOFT APIs AT ANY TIME FOR ANY REASON WITH OR WITHOUT NOTICE" and may terminate the terms "in our sole discretion at any time, for any reason". Liability is capped at USD $5.00; the developer indemnifies Microsoft (s.9). — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- The terms reference the Microsoft Trademark & Brand Guidelines (aka.ms/trademarks-usage) in the Intune section; the general sections I read contain no naming rule for apps that use OneDrive. — [Microsoft APIs Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)
- Existing practice: the leading third-party clients are named "onedrive" (abraunegg/onedrive) and use "OneDrive" descriptively in docs. — [abraunegg/onedrive usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)

### Inferences
- The terms as read contain no clause prohibiting a third-party or open-source sync client, and no open-source licence restriction on the client's own code; the feedback clause (s.7) only restricts feedback given to Microsoft.
- s.3(b)(8) is the clause that makes using the official client's private sync endpoints (section 6) a contractual risk, in addition to the technical one. Staying on documented Graph endpoints avoids it.
- s.3(e) is relevant to a "backup my whole machine" or Known-Folder-Move-style feature: syncing a user's work files is fine, marketing the client for system backups to OneDrive is what the clause targets.
- A shared client ID concentrates s.2(b) responsibility on whoever registered the app: all users' API activity occurs under that registrant's credentials.
- **[background, not verified this session]** Microsoft's trademark guidelines generally allow referential use ("X for OneDrive", "works with OneDrive") but not using Microsoft marks as the app's own name or logo, nor the OneDrive cloud icon. A distinct product name with a "for OneDrive" descriptor and a non-affiliation disclaimer is the conservative choice. The fact that long-lived projects use the bare name does not establish that it is permitted.

### Gaps
- I did not read the Microsoft Trademark & Brand Guidelines, so there is no sourced rule on naming a client.
- I did not read the Microsoft Services Agreement (consumer) for clauses on third-party app access to personal OneDrive accounts.
- No record searched for of Microsoft taking action against a third-party OneDrive client or its client ID.
