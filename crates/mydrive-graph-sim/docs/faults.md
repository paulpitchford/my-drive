# Faults and the request log

Part of the [`mydrive-graph-sim` documentation](../README.md). Code: `src/fault.rs`
(the fault types, the middleware that applies them and `push_fault`, `pending_faults`,
`clear_faults`), `src/conn.rs` (the connection tap behind body truncation) and
`src/log.rs` (the log types, the middleware that records requests and `requests`,
`clear_requests`).

"Research" means
[`graph_api_and_windows_parity.md`](../../../research_notes/OneDrive%20Linux%20on%20demand%20clients/graph_api_and_windows_parity.md)
and the section it is in.

## Simulated

| Behaviour | Source |
|---|---|
| A FIFO queue of scripted faults. Each fault has an optional method and path-fragment matcher and applies to exactly the first later request that matches, then is removed. Actions: a status with a Graph error body (its `code` is the one listed in [`server.md`](server.md) for that status unless the fault sets one, such as `resyncChangesApplyDifferences`), an optional `Retry-After` and an optional `Location` (the documented `410 Gone` resync responses carry both a code and a `Location`); a delay before handling; a truncated body (announces the full `Content-Length`, sends the head and the first N body bytes, then closes the connection). Faults apply before the token check, on every origin, the authority's included ([`identity.md`](identity.md)). | ADR-0017 decision 2 and R5; `Retry-After` on 429/503 from the throttling guidance (research section 3); the `410` codes and `Location` from [delta](https://learn.microsoft.com/en-us/graph/api/driveitem-delta?view=graph-rest-1.0) (WP-008) |
| A log of every request on every origin, the authority's included: origin, method, path, query, the headers `authorization`, `range`, `if-match`, `if-none-match`, `user-agent`, `prefer`, `deltaexcludeparent`, and the response status. Secrets the authority issues are masked ([`identity.md`](identity.md)). It keeps the newest 10,000 requests (`MAX_LOGGED_REQUESTS`), dropping the oldest first; a long test that asserts on the log calls `clear_requests` before the part it checks. | WP-007 |
