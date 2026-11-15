# Identity, the fake authority and discovery

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/identity.rs`
(accounts, tenants, `/me`, `/me/drive`, `/organization`), `src/authority.rs` (the fake
sign-in authority), `src/discovery.rs` (sharing links, sites, library names, blocked and
read-only libraries). It also touches `src/server.rs` (the authority's router and the policy
layer), `src/lib.rs` (the authority's listener) and `src/log.rs` (masking).

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
or [`sharepoint_libraries.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/sharepoint_libraries.md),
and the section it is in. Those notes quote the Microsoft pages they cite. A source marked
**(not re-read)** is a Microsoft page that the research notes do not quote. WP-023 did not
re-read it either, because the package works without network access. The statement comes
from background knowledge of that page and must be checked against it before anyone relies
on it. A behaviour marked **unverified** has no source. Where it matters, it has a switch
whose default is the case that is harder for a client.

## Use

```rust
let sim = GraphSim::start().await?;
let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
let priya = sim.add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")?;
let site = sim.add_site(&tenant, "Finance")?;
let library = sim.add_library(&site)?;
sim.sign_in_as(&priya)?;
// Point the client's authority at sim.authority_url() and its Graph base at sim.base_url().
let link = sim.add_sharing_link(&library, &sim.root_id(&library)?)?;
sim.set_library_access(&library, LibraryAccess::blocked())?;
```

## Origins and layers

The simulator listens on three loopback ports. The Graph origin and the download origin are
described in [`server.md`](server.md). The authority origin, `sim.authority_url()`
(`http://127.0.0.1:<port>/common/`), stands in for `https://login.microsoftonline.com/common/`.
Its router has the request log (`Origin::Authority`) and the fault layer, and no bearer
check. Its first path segment (`common`, `organizations`, `consumers` or a tenant ID) is
accepted whatever it is.

The Graph origin's layers, from the outside in:

1. request log
2. scripted faults
3. bearer-token check
4. **library policy**, which is new
5. routes and fallback

The policy layer is the innermost. The reasons:

- **Inside the bearer check.** A request without a valid token gets `401` first. A client
  then refreshes its token rather than taking a `401` for a policy block. Graph checks the
  token before anything else, so the order also matches.
- **Inside the fault layer.** A test's scripted fault still applies to a blocked library,
  just as it does to any other request, so fault scripts mean the same thing everywhere.
- **Outside every route, including the fallback.** It covers all routes under
  `/v1.0/drives/{id}`: the ones that exist now, write routes added by other packages
  (WP-019, WP-020) with no change to their handlers, and paths that match no route.

The download origin is **not** covered. Whether a pre-authenticated URL issued before a
block keeps working after it is **unverified** (gate G2).

## Locks

`Shared` holds two locks: the drives' `State` and the identity registry (`Directory`, with
the authority's and discovery's state inside it). No code holds both at once. A handler or
control call that needs both takes one, drops it and then takes the other. Neither lock is
held across an `.await`. Because they are never nested, there is no lock order to get wrong.
One example: the token endpoint reads the clock from `State`, issues tokens under
`Directory`, and only then adds the access token to `State`'s accepted tokens.

## Simulated

### Authority

| Behaviour | Source |
|---|---|
| `GET {authority}oauth2/v2.0/authorize` with `client_id`, `response_type=code`, `redirect_uri`, `scope`, `state`, `code_challenge`, `code_challenge_method`. It redirects (`302`) to `redirect_uri` with `code` and `state`, signed in as the account chosen with `sign_in_as`. With `deny_next_sign_in`, the next request redirects with `error` and `error_description` instead. | [Authorization code flow](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow) **(not re-read)**. That the client sends these parameters: ADR-0010, F01 |
| `scope` is optional at the authorise step: without it, the code carries no scopes, and redeeming it gives an access token with an empty `scope` and no refresh token. This is laxer than the platform, which requires `scope`. | Auth code flow page **(not re-read)**. The simulator is laxer on purpose: the client always sends `scope` (ADR-0010 R5) |
| A missing `client_id` or `redirect_uri`, a `redirect_uri` that is not absolute `http(s)`, or no account chosen gives `400` with no redirect. This stands in for the error page the platform shows instead of redirecting to an untrusted address. | Same page **(not re-read)**. The page and the status are **unverified** |
| PKCE (RFC 7636): methods `S256` and `plain`, with `plain` when no method is given. A request without a challenge gets a code that is not bound to a verifier. | [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636) sections 4.2 and 4.3. That the platform accepts both methods and makes PKCE optional for native clients: the auth code flow page **(not re-read)** |
| `POST {authority}oauth2/v2.0/token`, `grant_type=authorization_code`. The code is refused (`400 invalid_grant`) if it is unknown, already used (any attempt uses it up), older than 10 minutes on the simulator's clock, or issued to another `client_id` or `redirect_uri`. It is also refused if the `code_verifier` is missing or does not match the challenge (ADR-0010 R1). | Auth code flow page: codes are short-lived, about 10 minutes, and redeemable once **(not re-read)**. Using up the code on a failed attempt is **unverified** |
| A success is `{token_type: "Bearer", scope, expires_in: 3600, ext_expires_in, access_token}`. `refresh_token` is included only when `offline_access` was requested. `scope` leaves out `offline_access`, `openid`, `profile` and `email`. | Auth code flow page **(not re-read)**. The 60–90 minute lifetime: [Access tokens](https://learn.microsoft.com/en-us/entra/identity-platform/access-tokens) **(not re-read)**. The simulator uses 3600. Which scopes `scope` lists is **unverified** |
| `id_token` is returned only when `openid` was requested. The switch `id_token = Always` returns it every time. Note: the client's initial scopes (ADR-0010 R5) do not include `openid`, so the existing client gets no ID token (lead's open question Q37). | Auth code flow page **(not re-read)** |
| `grant_type=refresh_token` returns a new access token and a new refresh token (ADR-0010 R8). The old refresh token is then refused (`invalid_grant`) under the default `old_refresh_token = Refused`, and keeps working under `Accepted`. | [Refresh tokens](https://learn.microsoft.com/en-us/entra/identity-platform/refresh-tokens): tokens "replace themselves with a fresh token upon every use" and the old one is not revoked on use (research, section 1). **The default departs from that page on purpose**: WP-023 AC2 sets it, because it catches a client that keeps using an old token |
| A refresh may ask only for scopes granted at the authorise step that started its chain, or a subset; without `scope` it gets the chain's grant. A scope beyond that grant (for example `Sites.Read.All`) is refused with `400 invalid_grant` and `AADSTS65001` (consent required), and nothing is issued or rolled. Incremental consent needs a new authorise step, which ADR-0010 R5 relies on. A subset refresh keeps the chain's full grant for later refreshes. | That the platform refuses scopes the user has not consented to: auth code flow and permissions-and-consent pages **(not re-read)**. The exact error (`invalid_grant`, AADSTS65001) is **unverified**, and so is whether the platform allows other scopes consented earlier for the app. The simulator takes the stricter reading |
| A refresh whose requested scopes leave out `offline_access` issues no refresh token, and it still rolls the one presented. Under `old_refresh_token = Refused` the chain then ends. | **Unverified**; the harder case |
| A code presented a second time is refused (`invalid_grant`, AADSTS54005). Under the default `reused_code = RevokeIssuedTokens`, every token issued from its first redemption is revoked: the access tokens (the Graph origin answers `401`), the refresh token and every token rolled from it. Tokens from other sign-ins are untouched. `RefuseOnly` only refuses. | [RFC 6749](https://www.rfc-editor.org/rfc/rfc6749) section 4.1.2: the server "SHOULD revoke (when possible) all tokens previously issued based on that authorization code". Whether the platform does, and the AADSTS number: **unverified** |
| `revoke_refresh_token`, and `revoke_refresh_tokens(account)` (as an admin revocation or password reset does), make later refreshes answer `invalid_grant`. A refresh token older than 90 days on the simulator's clock also answers `invalid_grant`. Access tokens already issued keep working until a test revokes them with `revoke_token`. | Refresh tokens page: 90 days, and the revocation table (research, section 1). That access tokens outlive a revocation until they expire: Access tokens page **(not re-read)** |
| Error bodies are `{error, error_description: "AADSTS<n>: ...", error_codes, timestamp, trace_id, correlation_id}`. The `error` values used are `invalid_grant`, `invalid_request`, `unsupported_grant_type` and `unsupported_response_type`. | Shape: auth code flow page **(not re-read)**. The AADSTS numbers (70000, 70008, 50148, 501481, 50173, 700082, 9002313, 900144, 70003, 70005, 501491, 50034) are **unverified**. A client must act on `error`, not on the number |
| An access token the authority issues is accepted by the Graph origin and is bound to its account. The fixed per-instance token and `add_token` tokens are still accepted and belong to no account, unless `bind_token` binds them. | Simulator design (WP-023) |

### ID tokens

ID tokens are **unsecured JWTs**: header `{"typ":"JWT","alg":"none"}` and an empty signature
(RFC 7519 section 6). The existing client treats the ID token as opaque (`IdToken`, never
decoded or validated), so any shape satisfies it. `alg: none` means that a client which
later starts validating will refuse these tokens loudly instead of trusting a signature
nobody checked. Real ID tokens are RS256-signed.

The claims are `ver` (`2.0`), `iss` (`<authority origin>/<tid>/v2.0`), `aud` (the client ID),
`iat`, `nbf`, `exp` (simulator clock), `sub` (derived from object ID and client ID, so it
differs per app), `oid`, `tid`, `name` and `preferred_username`. Source:
[ID token claims reference](https://learn.microsoft.com/en-us/entra/identity-platform/id-token-claims-reference)
**(not re-read)**.

- **Personal accounts.** `tid` is `9188040d-6c67-4c5b-b112-36a304b66dad`
  (`CONSUMER_TENANT_ID`), which the claims reference documents as the tenant of every
  personal Microsoft account. It is a public constant, not any account's data, and a client
  may key on it (lead's ruling for WP-023). A personal account's `oid` is
  `00000000-0000-0000-<the 16-hex-digit user ID split 4/12>`, and `/me`'s `id` is that
  16-hex-digit ID, which the simulator makes equal to the personal drive's ID. Both are
  **unverified**.
- **Work accounts.** `tid` is the tenant's GUID and `oid` the account's.

Every tenant, object ID, user name and domain is made up by the simulator or the test, and
uses `.invalid` domains.

### Identity endpoints

| Behaviour | Source |
|---|---|
| `GET /me` gives `{id, displayName, userPrincipalName, mail}` for the token's account. | [Get user](https://learn.microsoft.com/en-us/graph/api/user-get?view=graph-rest-1.0) **(not re-read)** |
| `GET /me/drive` gives the account's own drive, as `GET /drives/{id}` renders it. A personal account's drive is `personal` and a work account's is `business` (AC6). The drive ID is as `add_drive` makes it: no mixed-case switch exists yet (research, section 4, issue #1902). | [Get drive](https://learn.microsoft.com/en-us/graph/api/drive-get?view=graph-rest-1.0) **(not re-read)**. Research, section 2 |
| `GET /me/drive` follows the policy on the account's own drive: if it is blocked, `/me/drive` answers `403` with the block's code, as `/drives/{id}` does. Read-only does not affect it, because it is a read. | Provisional, as blocked libraries are (gate G2) |
| `GET /organization` gives a work account's tenant as `{value: [{id, displayName, verifiedDomains: [{name, isDefault, isInitial, type}]}]}`. For a personal account it is `403 accessDenied` by default, or `{value: []}` under `personal_organization = Empty`. | [List organization](https://learn.microsoft.com/en-us/graph/api/organization-list?view=graph-rest-1.0) **(not re-read)**. Personal accounts and the required permission: **unverified** |
| A token bound to no account (the fixed one, say) gets `403 accessDenied` on `/me`, `/me/drive` and `/organization`, with a message naming `bind_token`. | Simulator departure: every real token belongs to a user |

### Discovery

| Behaviour | Source |
|---|---|
| `GET /shares/{u!...}/driveItem` decodes the `u!` + unpadded base64url form (`encode_sharing_url`) and returns the linked item as get-item does. For a library it adds `parentReference.siteId`. An unknown or undecodable link is `404`. | [Access shared items](https://learn.microsoft.com/en-us/graph/api/shares-get?view=graph-rest-1.0) (research `sharepoint_libraries.md`, section 1). `siteId` on itemReference: [itemReference](https://learn.microsoft.com/en-us/graph/api/resources/itemreference?view=graph-rest-1.0) **(not re-read)**. Whether the shares response includes it is **unverified** |
| `Prefer: redeemSharingLink` gives the caller's account durable access to the drive. `redeemSharingLinkIfNecessary` returns the item for this request only, and a later `GET /drives/{id}` is still `403` (AC3, ADR-0015 R2). The request log records the `Prefer` value as sent. | Shares page: "durable access", "for the duration of this request" (research `sharepoint_libraries.md`, section 1) |
| With no `Prefer`, a caller with no access gets `403` (`share_without_prefer = Refused`), or the item with no durable access (`Peek`). | **Unverified** |
| A link into another tenant's drive is `403 accessDenied` for the caller. | **Unverified**; models "people in your organization" links |
| A token bound to an account reaches only its own drive, drives granted with `grant_access`, and drives reached through a redeemed link. Any other existing drive is `403 accessDenied`. Unbound tokens reach every drive, as before WP-023. | Simulator model. The status for "no access" is **unverified** |
| `GET /sites/{site-id}` gives `{id, name, displayName, webUrl, siteCollection.hostname}`. Site IDs have the composite form `<host>,<guid>,<guid>`, with hosts under `sharepoint.example.invalid`. | [Get site](https://learn.microsoft.com/en-us/graph/api/site-get?view=graph-rest-1.0) **(not re-read)**. Whether it needs `Sites.Read.All` is not modelled (F09 open question) |
| `GET /sites/{site-id}` refuses (`403 accessDenied`) an account bound to a token whose tenant is not the site's, which includes every personal account. Unbound tokens read every site. Same-tenant accounts read it whether or not they can reach its libraries. | **Unverified**; models a site visible only inside its organisation |
| If any library of the site is blocked, `GET /sites/{site-id}` answers `403` with that block's code. Unmanaged-device controls "can be per site", so a library block stands in for a per-site policy. | [Control access from unmanaged devices](https://learn.microsoft.com/en-us/sharepoint/control-access-from-unmanaged-devices) for per-site policies (research `sharepoint_libraries.md`, section 1). That `/sites/{id}` is refused, and how: **unverified** (gate G2) |
| `GET /drives/{id}/list` gives a library's list: `{id, name: "Shared Documents", displayName: "Documents", list.template: "documentLibrary", parentReference.siteId, webUrl}`. A drive that is not a library gets `404`. | drive resource, `list` relationship **(not re-read)**. The list's `name` is **unverified** |
| A library's name is always "Documents", in the drive resource (`GET /drives/{id}`, rendered by `json.rs`) and in its list. | Default library name. Other names are not simulated (see below) |

### Blocked and read-only libraries: provisional until gate G2

Both signals are **guesses**. What Graph returns under unmanaged-device policy, and whether
and how a read-only library refuses writes, is what gate G2 and open questions Q12 and Q13
are for. Treat a client test that passes against them as provisional.

| Behaviour | Source |
|---|---|
| `set_library_access(drive, LibraryAccess::Blocked { code })` makes every request whose path is under `/v1.0/drives/{drive}` answer `403` with that Graph error `code` (default `accessDenied`), on every route and method, including routes that do not exist. The message is the "Access Denied. Due to organization policy..." text. A sharing link into the drive gets the same `403`. Other drives, including others in the same tenant, are not affected (AC4, ADR-0015 R4). | The message: [Control access from unmanaged devices](https://learn.microsoft.com/en-us/sharepoint/control-access-from-unmanaged-devices) (research `sharepoint_libraries.md`, section 1), which shows it to users in the browser. That Graph returns it, the status and the code are **unverified** (G2) |
| `LibraryAccess::ReadOnly { code }` refuses every request under the drive whose method writes, which is any method other than `GET` and `HEAD`. It answers `403` with `code` (default `accessDenied`). Reads work. | **Unverified** (Q12, G2). Microsoft's own client syncs such libraries read-only ([restrictions](https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa), ADR-0015). The Graph refusal is a guess |
| The drive ID in the path is percent-decoded and compared without regard to case, as the routes do. | `DriveId` (ADR-0007 R1) |

No resource carries a read-only signal (check-out, validation columns and so on). Detecting
read-only libraries from Graph data is Q12.

## Request log masking (AC7)

What the log keeps, and why. The rules live in `log.rs`, apply on every origin, and change
nothing else that the log records.

- **Issued secrets.** Access tokens (`sim-at-`), refresh tokens (`sim-rt-`) and
  authorisation codes (`sim-code-`) look like `<prefix><instance>-<n>.<secret part>`.
  Wherever one appears in a logged header value, the query string or the path, the log
  keeps `<prefix><instance>-<n>.` and writes `<redacted>` for the secret part. The label
  tells tokens apart (for example "the rolled token was sent") and is not secret. On the
  Graph origin, `authorization: Bearer sim-at-5000-3.<redacted>` is logged.
- **Percent-encoded secrets.** Each path segment, query parameter name and value, and
  header value is searched after decoding `%XX` (and `+` in the query), because a client
  may escape any character of a secret, the `.` included. A component in which decoding
  reveals a secret is logged decoded, with the secret masked: masking the raw form could
  leave escaped characters of the secret part behind. Every other component is logged
  exactly as sent. Decoding is done once, as a server does; a doubly encoded secret is
  not one the simulator would accept and is not looked for.
- **ID tokens.** Anything shaped like a JWT (`eyJ...` with two dots) is replaced whole with
  `<redacted>`.
- **Authority origin only.** The `authorization` header is logged as `<scheme> <redacted>`
  whatever follows the scheme, when the scheme (the part before the first space) is a
  plain word of ASCII letters such as `Bearer`. Otherwise, or when there is no space, the
  whole value is logged as `<redacted>`, so a secret sent in the scheme position is not
  logged either. The values of the query parameters `code`, `code_verifier`,
  `refresh_token`, `access_token`, `id_token`, `client_secret`, `client_assertion`,
  `assertion` and `password` are logged as `<redacted>`. A client that puts a secret in an
  authority URL would otherwise leak it to the log.
- **Bodies are never logged.** The token request's form (code, verifier, refresh token) and
  token responses therefore never reach the log. Neither do redirect `Location` headers,
  which carry the code.
- **A verifier anywhere but the form body is logged.** A PKCE verifier is a random string
  the client makes up, with no shape the log can recognise. Sent where it belongs, in the
  token request's form body, it is never logged. Sent anywhere else, it is logged as sent:
  in a header, in the path, in a query on the Graph or download origin, or in an authority
  query under a parameter name other than `code_verifier`. Only the authority's
  `code_verifier` query parameter is masked, by name. With `code_challenge_method=plain`,
  or with no method (which means `plain`), the `code_challenge` in the authorise query is
  the verifier itself, and it is logged in full: a test that uses `plain` puts its
  verifier in the log.
- **Not masked.** The fixed per-instance token (`sim-token-<port>`) and tokens added with
  `add_token` are logged in full as before, because existing tests compare them. So are
  `state` and `code_challenge` in the authorise query. `state` is not a credential. An
  `S256` challenge is a hash the verifier cannot be recovered from, and a test may want to
  check that the challenge is S256; a `plain` challenge, or one sent without a method, is
  the verifier and therefore a credential, logged in full all the same (see above).

Masking is by shape, so a token a test adds with `add_token` that happens to start with an
issued prefix would be masked too. The same goes for any value shaped like a JWT, even one
that is not a token: that false positive is accepted. On the authority's origin, the masked
parameter names are compared without regard to case and after percent-decoding (`CODE`,
`%63ode` and `refresh%5Ftoken` are all masked).

Issued secrets are derived from a counter, not drawn at random. **The label the log keeps
determines the whole secret**: the secret part is a hash of the label and a fixed string. So
masking protects against a secret being copied by accident, out of a log into a test
failure message or a report. It does not protect against someone who reads the log and
knows how the simulator works. The simulator is not a security boundary, and derived values
keep runs reproducible.

`Debug` of `GraphSim` does not print issued secrets. The identity registry and the
authority's state print counts in place of codes, refresh tokens and bound tokens. The
shared state's `Debug` leaves out the drives' state, which holds the accepted bearer tokens.
`GraphSim`'s own `token` field (the fixed per-instance token) is still printed.

Tests that assert over all of `requests()` now see the authority's requests too, and should
filter on `Origin::Graph` where they mean the Graph API.

## Departures and not simulated

- The authority does not check `redirect_uri` or `client_id` against an app registration.
  It accepts any absolute `http(s)` URI and any client ID, and only requires the token
  request to repeat the ones used at the authorise step.
- The Graph origin does not check an access token's scopes: any accepted token may call any
  route, whatever it was granted. Only the authority enforces scopes, on refresh.
- The authority does not check scopes against an app registration and never asks for
  consent. `organizations` and `consumers` do not restrict which account may sign in.
- Device-code flow, client credentials, `prompt`, `login_hint`, `nonce` and
  `response_mode=fragment|form_post` are not simulated. The OpenID discovery document and
  JWKS keys are not served.
- Scripted faults apply on the authority's origin too, because the queue is shared by all
  origins. A fault with no path filter matches the next request on any origin, so it may
  hit a sign-in. A status fault there answers with a Graph error body, not an OAuth one;
  use `on_path("/oauth2/v2.0/token")` to aim at the token endpoint.
- Access tokens do not expire on their own on the Graph origin: the bearer check
  (`auth.rs`) does not read the clock. Tests revoke them with `revoke_token`.
- Conditional access (`interaction_required`, `insufficient_claims`) and AADSTS65005
  (tenant refuses the app) are not simulated. `deny_next_sign_in` can return any `error` at
  the authorise step.
- The drive resource's own `name` is fixed at "Documents" for every library, because it is
  rendered in `json.rs`. Libraries with other names need a change there (follow-up).
- Teams and channels (`filesFolder`), site search and followed sites, `/sites/{id}/drives`,
  `/users/...` and `/groups/...` are not simulated.
- Policy blocks for a whole tenant (every mount of an account), per-site blocks, and the
  up-to-24-hour delay before a policy applies are not modelled. Blocks are per drive and
  apply at once.
