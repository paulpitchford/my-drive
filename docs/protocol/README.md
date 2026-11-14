# my-drive IPC protocol, version 1

The wire protocol between `mydrived` and every client: the CLI, the UI, the tray and the
file-manager adapters. Decided in [ADR-0009](../adr/0009-ipc-versioned-json-rpc-over-unix-socket.md),
amended by [ADR-0019](../adr/0019-protocol-pagination-and-open-value-sets.md) (pagination
and open value sets) and [ADR-0020](../adr/0020-protocol-results-are-bounded-and-cursors-survive-change.md)
(bounded messages and keyset cursors). The Rust types are in `crates/mydrive-ipc`. How
adapters use it is shown in [F11](../flows/F11-status-for-file-managers.md).

Every JSON example on this page is parsed by a test (`crates/mydrive-ipc/tests/protocol_examples.rs`)
into the matching Rust type and serialised back to the same JSON, and every request marked
as rejected must produce exactly the error example that follows it. An example that drifts
from the code fails the build. The `<!-- example: ... -->` comments label the examples for
that test; every JSON block on this page must carry one. The shell and Python cursor loops
below are run against a fake daemon by `crates/mydrive-ipc/tests/readme_examples.rs`.

## Transport and framing

- A Unix stream socket at `$XDG_RUNTIME_DIR/mydrive/daemon.sock`. The directory has mode
  `0700` and the socket `0600`; only the user's own processes can connect.
- JSON-RPC 2.0. Each message, in either direction, is **one JSON object on one line**,
  terminated by `\n`. Encoders emit compact JSON, so newlines inside strings are always
  escaped.
- **Every message is bounded.** A line is at most **1 MiB** (1,048,576 bytes, not counting
  the `\n`), in both directions and for every kind of message: requests, listings, action
  results, notifications and errors. The daemon never sends a longer line: every listing is
  [paginated](#pagination), action results carry a [sample](#item-actions) of skipped items,
  large changes are split across notifications, and human-readable messages are capped at
  1 KiB. Requests are bounded by the [`hello` limits](#hello), sized so that a request
  repeated with the longest cursor still fits.
- A longer line from a client is discarded up to the next `\n` and answered with
  `invalid-request`; the connection stays open and the next line is read normally. Lines
  that are not UTF-8 or not one JSON document get `parse-error`. These errors carry
  `"id": null`, so they cannot be matched to a request: **a client that gets an error with
  `"id": null` while it has requests in flight closes the connection and reconnects.**
- Blank lines are ignored.
- JSON-RPC batches (a JSON array of requests on one line) are not supported and get one
  `invalid-request` error with `"id": null`.
- **Request IDs** are an integer in the signed 64-bit range (-9223372036854775808 to
  9223372036854775807), written as plain digits with an optional leading `-`, or a string
  of at most 128 UTF-8 bytes. Anything else is refused with `invalid-request` and
  `"id": null`: a longer string, `null`, a boolean, an array, an object, and these numeric
  forms, **even when the value is a whole number**: a fraction (`1.5`), a decimal point
  (`1.0`, `0.0`), an exponent (`1e3`, `1E0`), `-0`, and an integer outside the signed
  64-bit range (`9223372036854775808`, `-9223372036854775809`). Forms that are not JSON
  numbers at all (`01`, `+1`, `0x1`, `NaN`) make the whole line a `parse-error`.
- **Duplicate keys.** Send each key once per object. If a request repeats a key, at any
  level (the envelope, `params` or deeper), the daemon reads the last occurrence and
  ignores the earlier ones. Clients must not rely on this.
- Parameters are always passed by name, as an object. Positional (array) parameters get
  `invalid-params`.

## Conventions

- Field names are `snake_case`. Enumerated values are lowercase kebab-case strings, never
  objects or tagged unions. Each enumerated field is [closed or open](#value-sets).
- **Paths** are absolute, canonical UTF-8 paths under a mount point, never backing-directory
  paths, at most 4,096 bytes long. A path that is empty, relative, longer than that, contains
  a NUL byte, has a `.` or `..` component, an empty component (`//`) or a trailing slash
  (other than `/` itself) is rejected with `invalid-params`. A path outside every mount gets
  `not-in-a-mount`. File names under a mount are always valid UTF-8 (the VFS refuses any
  other name), so a path that is not valid UTF-8 is never under a mount: clients must never
  send one, and must not "repair" one into a different string.
- **Large integers.** `seq`, `skipped_count` and `bytes_freed` are unsigned 64-bit
  integers, and a client may receive values above 2^53 (9007199254740992), beyond which a
  double-precision float cannot hold every integer exactly. A client whose JSON parser
  reads numbers as doubles (JavaScript's `JSON.parse`, for example) must read these fields
  as 64-bit or arbitrary-precision integers, or it will compare and show wrong values.
- Optional fields are omitted when they have no value. **A client or daemon treats an
  absent field and a `null` one the same way**, for every optional field, including
  `params` itself.
- **Unknown fields are ignored**, by the daemon and by clients, at every level. Clients also
  ignore unknown notification methods and unknown capabilities.
- **Reasons are codes.** Wherever a program might act on a reason (a refusal, a policy block,
  a read-only refusal, a cursor to restart, a blocked mount, an item in `error` or
  `blocked`), it is a kebab-case code from an open set, with a separate human-readable
  message. Programs never parse messages.
- **Messages are capped at 1 KiB**, measured in UTF-8 bytes before JSON escaping, and cut
  on a character boundary. This applies to every `message`, `blocked_message` and item
  `message`.
- Clients draw from `status` and `modifiers`. They never derive a status from the axes
  themselves; the axes are there for clients that want detail. **A folder's `status` is a
  summary of its contents computed by the daemon; clients display it as given.** The exact
  aggregation rule is the daemon's, not the client's.

## Value sets

*Closed* sets never gain a value within a major version; a client may reject an unknown
value. *Open* sets may gain values in any daemon release; a client must accept an unknown
value without failing the response and use the fallback below. Only the display status is
closed: the state axes are open on the wire (ADR-0020).

| Field | Set | Values | Client fallback for an unknown value |
|---|---|---|---|
| `item.status` | closed | `blocked`, `error`, `syncing`, `pinned`, `local`, `online-only` | — |
| `item.hydration` | open | `placeholder`, `partial`, `full` | Keep it; show the word |
| `item.pin` | open | `inherit`, `pinned`, `unpinned` | Keep it; show the word |
| `item.effective_pin` | open | `pinned`, `unpinned` | Keep it; show the word |
| `item.sync` | open | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked` | Keep it; show the word |
| `item.modifiers` | open | `read-only`, `shortcut` | Ignore it |
| `item.reason` | open | `conflict`, `name-not-allowed`, `storage-full`, `read-only`, `download-failed`, `hash-mismatch`, `local-change-lost`, `organisation-policy` | Show `message` |
| `mount.state` | open | `active`, `paused`, `blocked`, `resyncing` | Treat the mount as not active; show the word |
| `mount.blocked_reason` | open | `organisation-policy`, `tenant-refused` | Show `blocked_message` |
| `account.state` | open | `signed-in`, `needs-sign-in` | Treat the account as not active; show the word |
| `account.kind` | open | `personal`, `work` | Show the word |
| `error.code` | open | the [error table](#errors) | Treat as a failure; show `message` |
| `refused.data.reason` | open | `not-in-sync`, `pinned`, `in-use`, `remote-changed`, `insufficient-space` | Show `message` |
| `refused.data.sync` | open | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked` | Show the word |
| `blocked-by-policy.data.reason` | open | `organisation-policy`, `tenant-refused` | Show `message` |
| `read-only.data.reason` | open | `library-read-only`, `no-write-permission` | Show `message` |
| `invalid-params.data.reason` | open | `restart-listing` | Treat as a failure; show `message` |
| `hello.capabilities` | open | none in version 1 | Ignore it |

## Versioning

- The protocol has an integer **major version**. This page describes version `1`.
- Within a major version, changes are **additive only**: new methods, new optional
  parameters, new result fields, new limits, new notification methods, and new values of
  open sets. Clients must tolerate all of these.
- **A limit added in a later version may only constrain something new** (a new method or a
  new parameter), never a request a version 1 client can already send.
- Removing anything, renaming anything, making an optional parameter required, adding a value
  to a closed set, or changing what a field or value means requires a new major version.
- The first request on every connection is `hello`. The client lists the major versions it
  supports; the daemon picks the highest one both support, or answers `unsupported-version`
  with the list it supports.
- A daemon may support several major versions at once and must keep each working for as long
  as it is supported.

## Connections

1. Connect and send `hello`. Any other request first gets `hello-required` and has no
   effect. A second `hello` on the same connection gets `invalid-request`.
2. Send requests and read responses. **Match each response to its request by `id`**:
   responses may arrive in a different order from the requests.
3. The daemon sends no response to a notification (a message without `id`) from a client,
   and ignores it.
4. While the daemon is starting, requests other than `hello` and `daemon.status` get
   `not-ready`; retry later.
5. A connection that calls `status.subscribe` becomes a change stream: from then on it
   carries `status.changed` notifications only, and any request on it gets
   `invalid-request`. A client that also needs requests opens a second connection.
6. **Subscribe first, then list.** A client that shows live state subscribes on one
   connection, then lists what it shows on another, so no change between the two is lost.
   For each entry it keeps the newer state by the [`seq` rules](#statuschanged), whether it
   came from a listing page or from `status.changed`.
7. The daemon closes a subscribed connection that falls too far behind in reading, rather
   than buffering without bound. **A client treats any close of either connection the same
   way: reconnect, `hello`, re-subscribe, and re-request what it is showing.**

## Pagination

`item.status`, `mount.list` and `account.list` results may be split across several calls.
The request may carry `cursor`; the result carries `next_cursor` when more remains.
**`next_cursor` is never an empty string; when it is absent (or `null`) the listing is
complete.** To read a whole listing, repeat the call with **the same parameters** plus
`cursor` set to the previous `next_cursor`, until a result has no `next_cursor`. A client
must not cache a partial listing as complete. No page is empty while more remains.

**Cursors** are opaque, non-empty strings: do not parse, build or change them. A cursor is
bound to its request: used with a different directory or a different batch of paths, or
altered in any way, or in a form or version this daemon does not issue, it gets
`invalid-params` with `data.reason` `restart-listing`: start again from the first page. A
cursor never goes stale because the directory changed. A directory cursor is bound to the
directory's **path**: if the directory is replaced by another at the same path during the
loop, the loop continues into the new one, and its entries count as changed.

The longest cursors, for sizing. These figures describe the cursors this version of the
daemon issues; clients still treat cursors as opaque and must not rely on their form. Each
cursor is ASCII, so it takes the same number of bytes inside a JSON string.

- A directory cursor is 36 + 2 × (bytes of the last name returned). That is at most 546
  bytes **if names are at most 255 bytes**, the usual Linux limit. The state store can hold
  an item with a longer name (it is kept with sync `error`), and a cursor after such a name
  is longer by the same formula; names are still bounded by the 4,096-byte path limit.
- An `item.status` batch cursor is at most 56 bytes, assuming the batch position fits in
  an unsigned 64-bit integer (20 digits). With the version 1 limit of 10,000 paths per batch
  the position has at most 4 digits, so the cursor is at most 40 bytes.
- A `mount.list` cursor is 3 + 2 × (bytes of the `mount_id`), so at most 131 bytes, because
  a `mount_id` is at most 64 ASCII characters.
- An `account.list` cursor is 3 + 2 × (bytes of the account ID). Account IDs have no fixed
  length limit in the protocol, so neither does this cursor.

**Order.** A directory's children come in name order, comparing names as bytes (UTF-8), not
in locale order; a client that wants another order sorts them itself. Mounts come in byte
order of `mount_id`, accounts in byte order of `account_id`. A batch of paths comes in
request order.

**Guarantee under change** (ADR-0020). While a directory is listed through a cursor loop:

- An entry that exists, unchanged, for the whole cursor loop is delivered exactly once.
- An entry added, removed or renamed during the loop may or may not appear. A subscribed
  client learns of it from `status.changed`; a client that does not subscribe sees it on its
  next listing.

So a client that does not subscribe can rely on this: each listing it completes contains
every entry that existed unchanged while it was listing, each once, and nothing it shows is
more out of date than its most recent listing. To stay current it must list again; to be
told of changes it must subscribe.

**Large batches.** An `item.status` batch holds at most `limits.max_status_paths` paths
taking at most `limits.max_status_path_bytes`; a larger batch is refused with
`invalid-params`, and a client with more paths splits them into several batches.

A cursor loop in shell with `socat` and `jq`, printing every child of a directory. It fails
cleanly, with a message and a non-zero exit status, if the daemon refuses a request, closes
the connection or sends a reply that is not JSON. **It streams: each page is printed as it
arrives, so when it fails it may already have printed part of the listing.** A consumer must
check its exit status and, if it is not zero, discard what it read, never treat it as the
whole directory. Bash reads a socket slowly, a byte at a time; large listings belong in
Python.

```sh
#!/usr/bin/env bash
# Usage: list-dir DIRECTORY. Prints one JSON entry per child.
set -uo pipefail
dir="${1:?usage: list-dir DIRECTORY}"
sock="$XDG_RUNTIME_DIR/mydrive/daemon.sock"
trap '' PIPE  # a closed connection makes writes fail instead of killing the script
coproc DAEMON { socat - "UNIX-CONNECT:$sock"; }
exec {to_daemon}>&"${DAEMON[1]}" {from_daemon}<&"${DAEMON[0]}"

fail() { printf 'list-dir: %s\n' "$1" >&2; exit 1; }

# send REQUEST: write one line, read one reply into $reply, and fail on anything but a result.
send() {
  printf '%s\n' "$1" >&"$to_daemon" 2>/dev/null || fail "connection closed"
  IFS= read -r reply <&"$from_daemon" || fail "connection closed"
  [ -n "$reply" ] || fail "connection closed"
  kind=$(jq -r 'if has("result") then "result" elif has("error") then "error" else "other" end' \
    <<<"$reply" 2>/dev/null) || fail "unreadable reply"
  case "$kind" in
    result) ;;
    error) fail "$(jq -r '"error \(.error.code): \(.error.message)"' <<<"$reply")" ;;
    *) fail "unexpected reply" ;;
  esac
}

send '{"jsonrpc":"2.0","id":0,"method":"hello","params":{"client_name":"list-dir","protocol_versions":[1]}}'
cursor=null
id=1
while :; do
  send "$(jq -cn --arg d "$dir" --argjson c "$cursor" --argjson id "$id" \
    '{jsonrpc: "2.0", id: $id, method: "item.status",
      params: ({directory: $d} + (if $c == null then {} else {cursor: $c} end))}')"
  jq -c '.result.entries[]' <<<"$reply"
  cursor=$(jq -c '.result.next_cursor // null' <<<"$reply")
  [ "$cursor" = null ] && break
  id=$((id + 1))
done
```

The same in Python. It prints nothing until the listing is complete, and reports a refused
request, a closed connection or a reply that is not JSON with a message and a non-zero exit
status:

```python
#!/usr/bin/env python3
"""Usage: list_dir.py DIRECTORY. Prints one JSON entry per child."""
import json
import os
import socket
import sys


def list_directory(path):
    sock = socket.socket(socket.AF_UNIX)
    sock.connect(os.path.join(os.environ["XDG_RUNTIME_DIR"], "mydrive", "daemon.sock"))
    lines = sock.makefile("r", encoding="utf-8")
    next_id = 0

    def call(method, params):
        nonlocal next_id
        next_id += 1
        request = {"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}
        try:
            sock.sendall((json.dumps(request) + "\n").encode())
        except OSError as e:
            raise ConnectionError("connection closed") from e
        try:
            line = lines.readline()
        except OSError as e:
            raise ConnectionError("connection closed") from e
        except ValueError as e:  # not UTF-8
            raise ConnectionError("unreadable reply") from e
        if not line:
            raise ConnectionError("connection closed")
        try:
            reply = json.loads(line)  # one request in flight, so this is its reply
        except ValueError as e:
            raise ConnectionError("unreadable reply") from e
        if not isinstance(reply, dict):
            raise ConnectionError("unreadable reply")
        if "error" in reply:
            raise RuntimeError(f"error {reply['error']['code']}: {reply['error']['message']}")
        return reply["result"]

    call("hello", {"client_name": "list_dir.py", "protocol_versions": [1]})
    entries, cursor = [], None
    while True:
        params = {"directory": path}
        if cursor:
            params["cursor"] = cursor
        result = call("item.status", params)
        entries.extend(result["entries"])
        cursor = result.get("next_cursor")
        if not cursor:  # absent or null: the listing is complete
            return entries


if __name__ == "__main__":
    try:
        for entry in list_directory(sys.argv[1]):
            print(json.dumps(entry))
    except (ConnectionError, RuntimeError) as e:
        sys.exit(f"list_dir.py: {e}")
```

## Methods

| Method | Does |
|---|---|
| [`hello`](#hello) | Handshake: negotiate the protocol version and learn the limits |
| [`daemon.status`](#daemonstatus) | Daemon version and readiness |
| [`account.list`](#accountlist) | Accounts and their sign-in state |
| [`mount.list`](#mountlist) | Mounts and their state |
| [`item.status`](#itemstatus) | State of a batch of paths, or of a directory and every child |
| [`item.pin`](#item-actions) | Always keep on this device |
| [`item.unpin`](#item-actions) | Clear the pin, back to `inherit` |
| [`item.hydrate`](#item-actions) | Download now without pinning |
| [`item.dehydrate`](#item-actions) | Free up space |
| [`status.subscribe`](#statussubscribe) | Turn this connection into a change stream |

| Notification | Sent when |
|---|---|
| [`status.changed`](#statuschanged) | Item state changes, on a subscribed connection |

### hello

Parameters: `client_name` (string, for logs) and `protocol_versions` (the major versions the
client supports). Result: `protocol_version` (the chosen major), `capabilities` (strings
naming optional features; version 1 defines none, and clients ignore names they do not know)
and `limits`:

- `max_line_bytes`: the longest line in either direction, 1,048,576 in version 1;
- `max_action_paths`: the most paths in one item action request, 100 in version 1;
- `max_action_path_bytes`: the most bytes those paths may take together, 524,288;
- `max_status_paths`: the most paths in one `item.status` batch, 10,000;
- `max_status_path_bytes`: the most bytes those paths may take together, 524,288.

Path bytes are counted as the daemon counts them: each path in its minimal JSON string
encoding, quotes included (escaping only `"`, `\` and control characters). A client counting
its own encoding never counts less than the daemon. A request over a limit is refused with
`invalid-params` and has no effect; the client splits it. The status limits leave room for
the longest batch cursor and request ID, so a batch repeated with its cursor always fits one
line. New limits may be added as fields.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client_name": "mydrive-cli", "protocol_versions": [1]}}
```

<!-- example: response hello -->
```json
{"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": 1, "capabilities": [], "limits": {"max_line_bytes": 1048576, "max_action_paths": 100, "max_action_path_bytes": 524288, "max_status_paths": 10000, "max_status_path_bytes": 524288}}}
```

No version in common:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client_name": "old-adapter", "protocol_versions": [7]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 1, "error": {"code": 1009, "message": "no protocol version in common: client offered [7], daemon supports [1]", "data": {"supported_versions": [1]}}}
```

### daemon.status

No parameters. Result: `daemon_version`, `protocol_versions` (all majors the daemon supports)
and `ready`. While `ready` is false the daemon is starting, and requests other than `hello`
and `daemon.status` get `not-ready`.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 2, "method": "daemon.status"}
```

<!-- example: response daemon.status -->
```json
{"jsonrpc": "2.0", "id": 2, "result": {"daemon_version": "0.1.0", "protocol_versions": [1], "ready": true}}
```

### account.list

Parameters: optional `cursor`. Result: `accounts`, in byte order of `account_id`, each with
`account_id`, `kind` (`personal` or `work`, open), `tenant_id` (work accounts only) and
`state` (`signed-in` or `needs-sign-in`, open); and `next_cursor` while more remain
([pagination](#pagination)).

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 3, "method": "account.list", "params": {}}
```

<!-- example: response account.list -->
```json
{"jsonrpc": "2.0", "id": 3, "result": {"accounts": [
  {"account_id": "acct-personal-1", "kind": "personal", "state": "signed-in"},
  {"account_id": "acct-work-1", "kind": "work", "tenant_id": "00000000-0000-0000-0000-000000000001", "state": "needs-sign-in"}
]}}
```

### mount.list

Parameters: optional `cursor`. Result: `mounts`, in byte order of `mount_id`, each with
`mount_id`, `account_id`, `drive_id` (lowercase), `mount_point`, `state` (open), `read_only`,
and, only when `blocked`, `blocked_reason` (a code, open) and `blocked_message` (text for
people, at most 1 KiB); and `next_cursor` while more remain ([pagination](#pagination)).

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 4, "method": "mount.list"}
```

<!-- example: response mount.list -->
```json
{"jsonrpc": "2.0", "id": 4, "result": {"mounts": [
  {"mount_id": "example-site-documents", "account_id": "acct-work-1", "drive_id": "b!exampledriveid", "mount_point": "/home/user/Example/Site - Documents", "state": "blocked", "blocked_reason": "organisation-policy", "blocked_message": "The organisation does not allow access from unmanaged devices", "read_only": true}
], "next_cursor": "m1.6578616d706c652d736974652d646f63756d656e7473"}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 5, "method": "mount.list", "params": {"cursor": "m1.6578616d706c652d736974652d646f63756d656e7473"}}
```

<!-- example: response mount.list -->
```json
{"jsonrpc": "2.0", "id": 5, "result": {"mounts": [
  {"mount_id": "personal", "account_id": "acct-personal-1", "drive_id": "0123456789abcdef", "mount_point": "/home/user/OneDrive", "state": "active", "read_only": false}
]}}
```

### item.status

Parameters: exactly one of

- `paths`: a batch of paths, at most `limits.max_status_paths` of them. The result's
  `entries` has one entry per path, in request order.
- `directory`: one directory. The result's `directory` is the directory's own entry (repeated
  on every page) and `entries` has one entry per child, in byte order of name. Adapters fetch
  a folder this way, with one [cursor loop](#pagination), and never ask once per file.

and optionally `cursor`. The result carries `next_cursor` while more entries remain, and
`seq`, which is optional: the daemon-wide change sequence number the page was read at. A
client orders a page's states against notifications by the [`seq` rules](#statuschanged).

Each entry has `path` and exactly one of `item` or `error`. `item` holds:

- `status`: the display status (for a folder, the daemon's summary of its contents);
- `modifiers`: a list, usually empty, of `read-only` and `shortcut`;
- the axes `hydration`, `pin` (as set on the item), `effective_pin` (after inheritance) and
  `sync`;
- for status `error` or `blocked`, `reason` (a code) and `message` (text for people).

`error` is an [error object](#errors): `not-in-a-mount` for a path outside every mount,
`unknown-path` for a path in a mount that names no known item.

For a `directory` request the whole call fails with a top-level error instead:
`not-in-a-mount` if the directory is outside every mount, `unknown-path` if it names no known
item, and `invalid-params` if it names a file.

Status queries are answered from the daemon's state store. They never touch the mount point
and never cause a download.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 6, "method": "item.status", "params": {"paths": ["/home/user/OneDrive/Documents/notes.md", "/home/user/OneDrive/Documents/report.docx", "/home/user/Downloads/x.pdf"]}}
```

<!-- example: response item.status -->
```json
{"jsonrpc": "2.0", "id": 6, "result": {"seq": 41872, "entries": [
  {"path": "/home/user/OneDrive/Documents/notes.md", "item": {"status": "syncing", "modifiers": [], "hydration": "full", "pin": "inherit", "effective_pin": "unpinned", "sync": "pending-upload"}},
  {"path": "/home/user/OneDrive/Documents/report.docx", "item": {"status": "error", "modifiers": [], "hydration": "full", "pin": "inherit", "effective_pin": "unpinned", "sync": "error", "reason": "storage-full", "message": "OneDrive is full; the change is kept on this device"}},
  {"path": "/home/user/Downloads/x.pdf", "error": {"code": 1003, "message": "/home/user/Downloads/x.pdf is not under any mount point"}}
]}}
```

A directory, first page (real pages hold thousands of entries; this one is cut short to keep
the example readable):

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 7, "method": "item.status", "params": {"directory": "/home/user/OneDrive/Pictures/2026"}}
```

<!-- example: response item.status -->
```json
{"jsonrpc": "2.0", "id": 7, "result": {
  "directory": {"path": "/home/user/OneDrive/Pictures/2026", "item": {"status": "pinned", "modifiers": [], "hydration": "full", "pin": "pinned", "effective_pin": "pinned", "sync": "in-sync"}},
  "seq": 41872,
  "entries": [
    {"path": "/home/user/OneDrive/Pictures/2026/IMG_2001.jpg", "item": {"status": "syncing", "modifiers": [], "hydration": "placeholder", "pin": "inherit", "effective_pin": "pinned", "sync": "pending-download"}},
    {"path": "/home/user/OneDrive/Pictures/2026/IMG_2002.jpg", "item": {"status": "pinned", "modifiers": [], "hydration": "full", "pin": "inherit", "effective_pin": "pinned", "sync": "in-sync"}}
  ],
  "next_cursor": "d1.c8ab48c736ffc35c4d7698ae57bb450d.494d475f323030322e6a7067"
}}
```

The next page, with the same parameters and the cursor; it is the last page, so it has no
`next_cursor`:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 8, "method": "item.status", "params": {"directory": "/home/user/OneDrive/Pictures/2026", "cursor": "d1.c8ab48c736ffc35c4d7698ae57bb450d.494d475f323030322e6a7067"}}
```

<!-- example: response item.status -->
```json
{"jsonrpc": "2.0", "id": 8, "result": {
  "directory": {"path": "/home/user/OneDrive/Pictures/2026", "item": {"status": "pinned", "modifiers": [], "hydration": "full", "pin": "pinned", "effective_pin": "pinned", "sync": "in-sync"}},
  "seq": 41873,
  "entries": [
    {"path": "/home/user/OneDrive/Pictures/2026/Shared album", "item": {"status": "online-only", "modifiers": ["read-only", "shortcut"], "hydration": "placeholder", "pin": "unpinned", "effective_pin": "unpinned", "sync": "in-sync"}}
  ]
}}
```

A relative path is rejected:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 9, "method": "item.status", "params": {"paths": ["OneDrive/notes.md"]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 9, "error": {"code": -32602, "message": "invalid params for item.status: path \"OneDrive/notes.md\" is not a valid protocol path: it is not absolute"}}
```

A cursor the daemon did not issue for these parameters:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 10, "error": {"code": -32602, "message": "invalid params for item.status: cursor \"d0.c8ab48c736ffc35c4d7698ae57bb450d.494d475f323030322e6a7067\" was not issued for these parameters, or by this version; start the listing again from the first page", "data": {"reason": "restart-listing"}}}
```

A directory outside every mount:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 11, "error": {"code": 1003, "message": "/home/user/Downloads is not under any mount point"}}
```

### Item actions

`item.pin`, `item.unpin`, `item.hydrate` and `item.dehydrate` all take `paths`, at most
`limits.max_action_paths` of them taking at most `limits.max_action_path_bytes`; a larger
request is refused with `invalid-params` before anything is done. They return `entries`: one
per requested path, in request order. Each entry has `path` and either:

- `error` alone, when the action was not accepted for that path at all. A refused action
  changes nothing for that path. An entry with `error` never has `skipped`.
- or, when accepted, any of:
  - `skipped_count`: the exact number of descendants of a folder the action did not apply
    to. Present whenever anything was skipped.
  - `skipped`: a **sample** of those descendants, each with `path` and `error`, holding as
    many as fit in the line, possibly none. It is never complete by promise: use
    `skipped_count` for the number, and list the folder with `item.status` for the full
    picture. Skipped descendants are unchanged.
  - `bytes_freed`, for `item.dehydrate`: the local bytes released. Present whenever
    dehydrate ran for the path, including when it freed `0`.

- `item.pin`: set the pin to `pinned`; the daemon hydrates the item (and, for a folder, its
  contents) and keeps it hydrated. Accepted when the pin is recorded; downloads happen in the
  background and are reported by `status.changed`.
- `item.unpin`: set the pin back to `inherit`. Nothing is dehydrated.
- `item.hydrate`: download now, without changing the pin. Accepted when the request is
  recorded; downloads are reported by `status.changed`.
- `item.dehydrate`: free up space. This is the user-facing action. It is decided per file
  before the response is sent: each file is checked to be `in-sync` (refused with
  `not-in-sync` otherwise, before anything changes), then its pin is set to `unpinned` and its
  local content released. The response reports what was freed and what was skipped.
- **On a folder, `item.pin`, `item.unpin` and `item.dehydrate` reset each descendant's own
  explicit pin to `inherit`**, as Windows does, so the folder's new pin governs its whole
  subtree.

Refusals use code `refused` with `data.reason` (open): `not-in-sync` (with `data.sync`),
`in-use`, `remote-changed` or `insufficient-space`. The reason `pinned` is reserved for an
engine-level dehydrate; the user-facing `item.dehydrate` never returns it, because it unpins
first.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 12, "method": "item.pin", "params": {"paths": ["/home/user/OneDrive/Pictures/2026"]}}
```

<!-- example: response item.pin -->
```json
{"jsonrpc": "2.0", "id": 12, "result": {"entries": [{"path": "/home/user/OneDrive/Pictures/2026"}]}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 13, "method": "item.unpin", "params": {"paths": ["/home/user/OneDrive/Pictures/2026"]}}
```

<!-- example: response item.unpin -->
```json
{"jsonrpc": "2.0", "id": 13, "result": {"entries": [{"path": "/home/user/OneDrive/Pictures/2026"}]}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 14, "method": "item.hydrate", "params": {"paths": ["/home/user/OneDrive/Documents/budget.xlsx", "/home/user/Example/Site - Documents/plan.docx", "/home/user/Example/Archive/old.docx"]}}
```

<!-- example: response item.hydrate -->
```json
{"jsonrpc": "2.0", "id": 14, "result": {"entries": [
  {"path": "/home/user/OneDrive/Documents/budget.xlsx"},
  {"path": "/home/user/Example/Site - Documents/plan.docx", "error": {"code": 1006, "message": "the organisation does not allow access from unmanaged devices", "data": {"reason": "organisation-policy"}}},
  {"path": "/home/user/Example/Archive/old.docx", "error": {"code": 1008, "message": "cannot download while offline"}}
]}}
```

Free up space on a folder of 7,000 files, 6,998 of them still uploading and one open: the
sample shows two of the 6,999 skipped files.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 15, "method": "item.dehydrate", "params": {"paths": ["/home/user/OneDrive/Videos/2025", "/home/user/OneDrive/Documents/notes.md", "/home/user/OneDrive/Documents/empty.txt"]}}
```

<!-- example: response item.dehydrate -->
```json
{"jsonrpc": "2.0", "id": 15, "result": {"entries": [
  {"path": "/home/user/OneDrive/Videos/2025", "skipped": [
    {"path": "/home/user/OneDrive/Videos/2025/edit.mp4", "error": {"code": 1004, "message": "dehydrate refused: the file is open", "data": {"reason": "in-use"}}},
    {"path": "/home/user/OneDrive/Videos/2025/clip-0001.mp4", "error": {"code": 1004, "message": "dehydrate refused: sync state is uploading, not in-sync", "data": {"reason": "not-in-sync", "sync": "uploading"}}}
  ], "skipped_count": 6999, "bytes_freed": 734003200},
  {"path": "/home/user/OneDrive/Documents/notes.md", "error": {"code": 1004, "message": "dehydrate refused: sync state is pending-upload, not in-sync", "data": {"reason": "not-in-sync", "sync": "pending-upload"}}},
  {"path": "/home/user/OneDrive/Documents/empty.txt", "bytes_freed": 0}
]}}
```

### status.subscribe

Parameters: `paths`, the mount points or folders to watch; empty, absent or `null` means
every mount. **A path covers its whole subtree.** Result: an empty object. From then on the
connection carries `status.changed` notifications only.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 16, "method": "status.subscribe", "params": {"paths": ["/home/user/OneDrive"]}}
```

<!-- example: response status.subscribe -->
```json
{"jsonrpc": "2.0", "id": 16, "result": {}}
```

A request on a subscribed connection:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 17, "error": {"code": -32600, "message": "mount.list refused: this connection is subscribed and carries notifications only; open a second connection for requests"}}
```

### status.changed

A notification (no `id`, no response) on a subscribed connection. Parameters:

- `seq` (optional): the daemon-wide change sequence number of this change. It only
  increases. A client holding state for an entry, from a listing page or a notification,
  decides which state to keep by these rules:
  - The state with the higher `seq` wins.
  - **On equal `seq`, a notification's state wins over a page's.** Two pages, or two
    notifications, with the same `seq` describe the same change; the one received last
    wins.
  - `seq` is optional on both pages and notifications. When either state has no `seq`,
    they cannot be ordered by it, and the one received last wins.
- `entries`: changed items, in the same shape as `item.status` entries, with each item's new
  state. An entry whose `error` is `unknown-path` means the item no longer exists.
- `refresh`: paths whose state changed too much to enumerate (for example a renamed folder
  with many descendants). For each path in `refresh`, the client discards what it has cached
  for that path and everything below it, and re-requests whatever of it the client is
  showing, from the first page. A `refresh` path may name a file; the client re-requests it
  with `item.status` `paths`. A `refresh` naming an ancestor of a subscribed path is
  delivered to that subscriber too.

`entries` and `refresh` are always present, possibly empty. One change too large for one
line is split across several notifications with the same `seq`, or replaced by a `refresh`
of the paths concerned.

<!-- example: notification -->
```json
{"jsonrpc": "2.0", "method": "status.changed", "params": {
  "seq": 41874,
  "entries": [
    {"path": "/home/user/OneDrive/Pictures/2026/IMG_2001.jpg", "item": {"status": "pinned", "modifiers": [], "hydration": "full", "pin": "inherit", "effective_pin": "pinned", "sync": "in-sync"}},
    {"path": "/home/user/OneDrive/Pictures/2026/old.jpg", "error": {"code": 1002, "message": "no item at /home/user/OneDrive/Pictures/2026/old.jpg"}}
  ],
  "refresh": ["/home/user/OneDrive/Projects"]
}}
```

## Errors

Errors are JSON-RPC error objects: `code` (integer), `message` (for people; at most 1 KiB; it
may change) and optional `data`. Programs act on `code` and `data`, never on `message`. The
codes are stable: once released, a code never changes meaning. Codes are an open set: a
client must handle a code it does not know as a failure and show its `message`. Clients must
also tolerate a `data` of a shape they do not expect.

*Where* says whether a code appears as the top-level error of a response, as a per-path
`error` inside a result (an `item.status` entry, an action entry or a skipped item), or both.

| Code | Name | Meaning | `data` | Where |
|---|---|---|---|---|
| -32700 | `parse-error` | The line was not UTF-8 or not one JSON document | | top level |
| -32600 | `invalid-request` | Not a valid JSON-RPC message (including a batch array or an unacceptable `id`); line over 1 MiB; second `hello`; request on a subscribed connection | | top level |
| -32601 | `method-not-found` | No such method in this version | | top level |
| -32602 | `invalid-params` | Parameters malformed, including any relative or non-canonical path, a cursor not issued for these parameters, a file given as `directory`, or a request over the `hello` limits | `reason` for a cursor | top level |
| -32603 | `internal` | A daemon fault | | both |
| 1001 | `not-ready` | The daemon is still starting; retry later | | top level |
| 1002 | `unknown-path` | Under a mount point, but no such item | | both |
| 1003 | `not-in-a-mount` | Not under any mount point | | both |
| 1004 | `refused` | Understood and refused; nothing changed | `reason`, and `sync` for `not-in-sync` | per path |
| 1005 | `needs-sign-in` | The account must sign in again | | both |
| 1006 | `blocked-by-policy` | Organisation policy refuses access | `reason` | both |
| 1007 | `read-only` | The item or its mount is read-only | `reason` | per path |
| 1008 | `offline` | The request needs the server and the daemon is offline | | both |
| 1009 | `unsupported-version` | `hello` offered no supported version | `supported_versions` | top level |
| 1010 | `hello-required` | A request other than `hello` before `hello` completed; a client bug, retrying does not help | | top level |

An unknown method:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 18, "method": "item.delete", "params": {"paths": ["/home/user/OneDrive/a.txt"]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 18, "error": {"code": -32601, "message": "method \"item.delete\" is not part of this protocol version"}}
```

A request before `hello`:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 19, "error": {"code": 1010, "message": "daemon.status refused: the first request on a connection must be hello"}}
```

The daemon is still starting:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 20, "error": {"code": 1001, "message": "the daemon is starting; try again shortly"}}
```

A line that is not JSON:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "line 3 is not valid JSON: expected value at line 1 column 1"}}
```

The remaining codes:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 21, "error": {"code": 1005, "message": "account acct-work-1 needs to sign in again"}}
```

<!-- example: response item.pin -->
```json
{"jsonrpc": "2.0", "id": 22, "result": {"entries": [{"path": "/home/user/Example/Site - Documents/plan.docx", "error": {"code": 1007, "message": "/home/user/Example/Site - Documents is a read-only library", "data": {"reason": "library-read-only"}}}]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 23, "error": {"code": -32603, "message": "internal error while reading the state store"}}
```
