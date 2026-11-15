# Content download

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/content.rs`
(`/content`, the redirect, the download origin and byte ranges).

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in.

## Simulated

| Behaviour | Source |
|---|---|
| `GET .../items/{id}/content` needs the bearer token in both modes. | Download driveItem content; MC1492958 via research section 2 |
| `download = Redirect` (default): `302 Found` with `Location` on the download origin, a fresh `tempauth` key each time; that URL needs no token. Get-item and list-children include `@microsoft.graph.downloadUrl` for files in this mode. Only the newest 1,024 keys stay valid; an older or made-up key gets `401`, as an expired URL would. | Download driveItem content ("302 Found ... preauthenticated download URL", "don't need to include an Authorization header") |
| `download = Direct`: `/content` returns the content itself. | MC1492958 mirror (research section 2): after 2027-04-01 apps must "handle direct content responses instead of following 302 redirects" |
| `range = Honoured` (default): a single `bytes=a-b`, `bytes=a-` or `bytes=-n` range gets `206` with `Content-Range: bytes a-b/len` and exactly those bytes; a range outside the file gets `416` with `Content-Range: bytes */len`. Several ranges or a malformed header are ignored (200). | Download driveItem content (206 example); RFC 9110 |
| `range = Ignored`: `Range` is ignored and the whole body comes back with `200`. | Download driveItem content ("may be ignored and an HTTP 200 response would be returned with the full contents") |
| In redirect mode `Range` on `/content` itself is not honoured; it applies at the download URL. The range switch applies to whichever URL serves content, so "Range on authenticated `/content`" (gate G1, **unverified**) is covered by `Direct` with either range setting. | Download driveItem content ("append the Range header to the actual downloadUrl ... and not to the request for /content"); ADR-0011, ADR-0017 consequences |

## Departures from Graph

- **Download URLs** expire only by count (oldest of 1,024), not by time; the driveItem
  resource says `@microsoft.graph.downloadUrl` is invalidated after a short period
  (1 hour) and the download page that it "might expire within minutes". A URL always
  serves the item's current content.
