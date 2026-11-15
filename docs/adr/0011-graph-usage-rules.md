# ADR-0011: Graph usage rules: items by ID, quickXorHash, one request scheduler per tenant, Retry-After only

- **Status:** Accepted; R2 refined by ADR-0022, R4 clarified in the overview
- **Date:** 2026-11-14
- **Depends on gates:** G1 for ranged reads (see `docs/architecture/overview.md`)

## Context

Microsoft directs third-party sync engines to Graph, and delta, upload sessions and ranged
downloads are sufficient for an on-demand client
([throttling guidance](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)).
The primitives carry rules that existing clients learned the hard way:

- Delta omits `parentReference.path`, a folder rename does not return its descendants, the
  same item can appear more than once in a feed ("use the last occurrence"), and a `410 Gone`
  resync arrives as `resyncChangesApplyDifferences` or `resyncChangesUploadDifferences`, the
  latter "keeping both copies if you're not sure"
  ([delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0)).
- Business delta omits `cTag`. `quickXorHash` is the only hash guaranteed on both account
  types; `sha256Hash` is documented as "isn't supported. Don't use."
  ([hashes](https://learn.microsoft.com/en-us/graph/api/resources/hashes?view=graph-rest-1.0)).
  Timestamps are whole seconds
  ([abraunegg usage.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/usage.md)).
- Throttling is per user and per app per tenant; throttled requests count against the limit;
  `Retry-After` is the signal. The same page both recommends and disclaims IETF RateLimit
  headers. A decorated `ISV|Company|App/Version` User-Agent is prioritised
  ([throttling guidance](https://learn.microsoft.com/en-us/sharepoint/dev/general-development/how-to-avoid-getting-throttled-or-blocked-in-sharepoint-online)).
  No limits are published for personal accounts.
- `subscriptions/socketIo` gives near-real-time drive notifications under delegated
  permissions for both account types; webhooks need a public HTTPS endpoint
  ([socketIo](https://learn.microsoft.com/en-us/graph/api/subscriptions-socketio?view=graph-rest-1.0),
  [abraunegg webhooks.md](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/webhooks.md)).
- Today `/content` returns 302 to a pre-authenticated URL, `Range` must go to that URL, and the
  server may ignore `Range` and return 200 with the whole body
  ([download](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)).
  MC1492958, read through a third-party mirror, retires pre-authenticated URLs on 2027-04-01
  for download, upload sessions and thumbnails; apps must send bearer tokens and accept direct
  content ([mirror](https://mc.merill.net/message/MC1492958)). Whether `Range` works on
  authenticated `/content` and whether personal accounts are in scope is unknown (gate G1).
- Upload fragments must be multiples of 320 KiB and under 60 MiB; the current doc says not to
  send `Authorization` to `uploadUrl`
  ([createUploadSession](https://learn.microsoft.com/en-us/graph/api/driveitem-createuploadsession?view=graph-rest-1.0)).
- `/me/drive/sharedWithMe` stops returning data after November 2026
  ([sharedWithMe](https://learn.microsoft.com/en-us/graph/api/drive-sharedwithme?view=graph-rest-1.0)).
- The API terms forbid working around technical limits and bar storing system back-up data in
  OneDrive ([Terms of Use](https://learn.microsoft.com/en-us/legal/microsoft-apis/terms-of-use)).

## Decision

`mydrive-graph` uses documented Microsoft Graph v1.0 endpoints only.

**Identity and change detection.** Items are tracked by (`drive_id`, `item_id`), never by
path. Within one delta feed the engine applies the last occurrence of each item. Both `410
Gone` variants are handled as documented; where it cannot tell which side is newer, it keeps
both copies (conflict naming in ADR-0012). Content change is detected by eTag, size and
`quickXorHash`; `cTag` is not relied on and `sha256Hash` is not used. Timestamps are stored and
compared at whole-second precision.

**Request budget.** One request scheduler per tenant (per account for personal accounts) owns
the request budget for every mount in that tenant. Per-mount workers submit requests to it and
never back off on their own. `Retry-After` on 429 or 503 is the only back-off signal honoured;
RateLimit headers are ignored. Every request carries the User-Agent
`ISV|<company>|<app>/<version>`.

**Change notification.** Each mount obtains a `subscriptions/socketIo` endpoint and uses a
notification only as a wake-up to call delta. When the socket is down, the mount falls back to
slow polling. Webhooks are not used.

**Downloads.** The client never assumes a pre-authenticated URL. It requests `/content` with
the bearer token and handles both a 302 and a direct content response. Every ranged read checks
the status: 206 is a range; 200 is the whole body and is handled as such.

**Uploads.** Upload-session fragments are multiples of 320 KiB, under 60 MiB, default 10 MiB.
Whether a PUT to `uploadUrl` carries an `Authorization` header is decided per session from the
server's responses, not hard-coded.

**Not used or attempted.** `/me/drive/sharedWithMe`; Personal Vault; differential upload;
Office desktop co-authoring.

**Terms of use.** The client does not work around technical limits (no extra app IDs, no
undocumented endpoints to dodge throttling) and is not marketed as a system backup tool.

## Alternatives considered

**Path-keyed state.** Simpler to inspect, but delta gives no paths and renames do not return
descendants. Rejected.

**Per-mount back-off.** Easier to build, but independent workers collectively overrun a
per-tenant budget. Rejected.

**Honouring RateLimit headers.** The documentation contradicts itself on whether they are
sent. Rejected until it is consistent.

**Webhooks or polling only.** Webhooks need a public endpoint; polling alone is slow or costly.

## Consequences

- The scheduler is a shared component that every Graph call passes through, including
  hydration reads, so read-ahead and chunk caching are needed to stay inside 3,000 requests per
  5 minutes per user.
- Shared content arrives only through shortcuts (ADR-0015, milestone M5).
- Range-based hydration stays provisional until G1; ADR-0005 falls back to whole-file.
- Open concern: the 302 path still sends `Range` to the redirect target as today's docs
  require; how that changes after April 2027 is unknown until G1.

## Rules for implementers

- **R1.** No store table or engine map uses a path as the key for an item.
- **R2.** When an item occurs more than once in a delta page set, only its last occurrence is
  applied.
- **R3.** `resyncChangesApplyDifferences` and `resyncChangesUploadDifferences` each have a handler;
  when local and server content differ and neither is known newer, both copies are kept.
- **R4.** Content equality uses eTag, size and `quickXorHash`; no code reads `cTag` or
  `sha256Hash` for this.
- **R5.** Timestamps are truncated to whole seconds before storage and comparison.
- **R6.** All Graph requests for a tenant (or personal account) go through one scheduler instance;
  a 429 or 503 seen by any mount delays all of that tenant's requests until `Retry-After`
  expires.
- **R7.** RateLimit response headers do not change scheduling.
- **R8.** Every request carries the `ISV|<company>|<app>/<version>` User-Agent.
- **R9.** A socketIo notification triggers a delta call and never changes item state by itself.
- **R10.** Downloads send the bearer token to `/content`, accept 302 and direct 200/206, and do not
  forward the bearer token to a redirect host.
- **R11.** A ranged read that receives 200 treats the body as the whole file from offset 0.
- **R12.** Fragment sizes are multiples of 327,680 bytes and under 60 MiB.
- **R13.** No code calls `/me/drive/sharedWithMe`.

## Revisit when

- G1 reports how `Range` behaves on authenticated `/content`.
- 2027-04-01 passes and the upload URL behaviour is observed.
- Microsoft makes the RateLimit header documentation consistent, or publishes personal-account
  limits.
- Graph adds differential upload or a documented replacement for `sharedWithMe`.
