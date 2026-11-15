# Server and auth

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/server.rs` (the
two origins' routers and the shared error helpers) and `src/auth.rs` (the bearer-token
check and `token`, `add_token`, `revoke_token`).

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in.

## Simulated

| Behaviour | Source |
|---|---|
| Two origins: the Graph API at `http://127.0.0.1:<port>/v1.0`, and a separate origin (different port) for pre-authenticated download URLs, standing in for a storage host like `*.files.1drv.com`. A client treats them as different hosts, so it can be checked not to forward the bearer token (ADR-0011 R10). | [Download driveItem content](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0) (302 to another host); research section 2 |
| Every Graph-origin request needs `Authorization: Bearer <token>` with a token the simulator knows; a missing, unknown or revoked token gets `401` with code `InvalidAuthenticationToken`. Each simulator has its own token (`sim.token()`); tests can add and revoke tokens. | [Error responses](https://learn.microsoft.com/en-us/graph/errors) (401 meaning). The code string is what Graph returns for a bad token in practice; the errors page lists no code for 401, so it is **unverified** |
| Errors use the Graph body `{"error": {"code", "message"}}`, without `innerError` (see departures). Codes used: `itemNotFound` (404), `invalidRequest` (400), `invalidRange` (416), `activityLimitReached` (429), `serviceNotAvailable` (503), `accessDenied` (403), `nameAlreadyExists` (409), `generalException` (others). | [Error responses](https://learn.microsoft.com/en-us/graph/errors) for the shape. The code strings come from the OneDrive error-code list, not from that page: **unverified** |

## Departures from Graph

- **Error bodies** have no `innerError` (with its `request-id` and `date`), and responses
  carry no `request-id` or `client-request-id` headers.
