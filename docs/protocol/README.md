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
- **Local paths.** `item.import` and `item.export` name one path outside every mount, on
  the local filesystem. It is spelled by exactly the rule for protocol paths above: absolute,
  canonical (no `.` or `..` component, no empty component, no trailing slash other than `/`
  itself), UTF-8 with no NUL byte, at most 4,096 bytes. A relative or non-canonical local path
  (`report.pdf`, `~/report.pdf`, `/home/u/Downloads/`, `/home/u/a/../b`) is rejected with
  `invalid-params`; the daemon never expands `~`, resolves `..` or adds a working directory.
  **The rule is lexical, so it cannot see symbolic links.** The daemon resolves the path on
  the local filesystem, following symbolic links in the directories that lead to it, and
  refuses with `local-file-error` reason `inside-mount` a path whose resolved location is
  under a mount point or the daemon's data directory. `item.import` reads the file the last
  component names, following a symbolic link there to a regular file. `item.export` never
  follows a symbolic link at the last component: with mode `create` the link counts as
  existing, and with mode `replace` the link itself is replaced, not the file it points to.
- **Durations and times** are integers in whole seconds: a duration is a field ending in
  `_secs` (at most 604,800, 7 days), and a point in time is seconds since the Unix epoch,
  UTC (`paused_until`). Both are always below 2^53.
- **Large integers.** `seq`, `skipped_count` and `bytes_freed` are unsigned 64-bit
  integers, and a client may receive values above 2^53 (9007199254740992), beyond which a
  double-precision float cannot hold every integer exactly. A client whose JSON parser
  reads numbers as doubles (JavaScript's `JSON.parse`, for example) must read these fields
  as 64-bit or arbitrary-precision integers, or it will compare and show wrong values. The
  counts `item_count`, `unsynced_files`, `kept_files`, `paused_mounts`, `resumed_mounts` and
  `bytes_copied` are unsigned 64-bit integers too and are read the same way.
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
  `message`, and to the names given for display: `user_name`, `display_name`, `drive_name`
  and `site_name`.
- **Credentials never travel to a client** (ADR-0010 R6). No result, notification or error
  carries an access token, refresh token, ID token, authorisation code, PKCE verifier, the
  redirect address pasted into `account.login.finish`, or a sharing link given to
  `mount.add.*`; see [Sign-in and credentials](#sign-in-and-credentials).
- **A request is never guessed at.** An unknown value of a request field (a `mode`, a
  `target`), a missing required field, or fields that do not fit together are refused with
  `invalid-params`. The daemon never substitutes a default for a value it did not
  understand.
- Clients draw from `status` and `modifiers`. They never derive a status from the axes
  themselves; the axes are there for clients that want detail. **A folder's `status` is a
  summary of its contents computed by the daemon; clients display it as given.** The exact
  aggregation rule is the daemon's, not the client's.

## Value sets

*Closed* sets never gain a value within a major version; a client may reject an unknown
value. *Open* sets may gain values in any daemon release; a client must accept an unknown
value without failing the response and use the fallback below. Only the display status is
closed: the state axes are open on the wire (ADR-0020).

Sets marked *request only* appear only in requests. They are open because a later daemon may
accept more values (a `device-code` login mode, a Teams channel as a mount target); a daemon
refuses a value it does not know with `invalid-params` rather than guessing, and a client
uses a new value only when the daemon advertises it.

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
| `refused.data.reason` | open | `not-in-sync`, `pinned`, `in-use`, `remote-changed`, `insufficient-space`, `already-exists`, `not-a-file`, `name-not-allowed`, `mount-point-not-empty`, `mount-point-overlaps`, `already-mounted`, `no-access`, `unsupported-target` | Show `message` |
| `refused.data.sync` | open | `in-sync`, `pending-download`, `pending-upload`, `uploading`, `conflict`, `error`, `blocked` | Show the word |
| `blocked-by-policy.data.reason` | open | `organisation-policy`, `tenant-refused` | Show `message` |
| `read-only.data.reason` | open | `library-read-only`, `no-write-permission` | Show `message` |
| `invalid-params.data.reason` | open | `restart-listing`, `restart-preview` | Treat as a failure; show `message` |
| `sign-in-failed.data.reason` | open | `attempt-not-found`, `attempt-expired`, `state-mismatch`, `not-a-redirect`, `authorization-error`, `code-rejected`, `no-secret-service`, `loopback-unavailable` | Treat the attempt as gone: start a new sign-in; show `message` |
| `local-file-error.data.reason` | open | `not-found`, `not-a-file`, `permission-denied`, `already-exists`, `inside-mount`, `no-space`, `io-error` | Show `message` |
| `mount.drive_type` | open | `personal`, `business`, `document-library` | Show the word |
| `mount.add.preview.drive_type` | open | `personal`, `business`, `document-library` | Show the word |
| `account.login.finish.proposed_mount.drive_type` | open | `personal`, `business`, `document-library` | Show the word |
| `account.login.start.mode` | open | `browser`, `headless` | Request only: a daemon refuses a mode it does not know with `invalid-params` |
| `mount.add.preview.target` | open | `own-drive`, `drive-id`, `sharing-link` | Request only: a daemon refuses a target it does not know with `invalid-params` |
| `mount.add.confirm.target` | open | `own-drive`, `drive-id`, `sharing-link` | Request only: as for `mount.add.preview.target` |
| `item.import.mode` | open | `create`, `replace` | Request only: a daemon refuses a mode it does not know with `invalid-params` |
| `item.export.mode` | open | `create`, `replace` | Request only: as for `item.import.mode` |
| `hello.capabilities` | open | `item-import-export` | Ignore it |

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
| [`account.login.start`](#accountloginstart) | Begin signing an account in |
| [`account.login.finish`](#accountloginfinish) | Complete the sign-in; returns the account, creates no mount |
| [`account.logout`](#accountlogout) | Sign an account out; local files stay |
| [`mount.list`](#mountlist) | Mounts and their state |
| [`mount.add.preview`](#mountaddpreview) | Show what adding a mount would mount; changes nothing |
| [`mount.add.confirm`](#mountaddconfirm) | Add the mount a preview showed |
| [`mount.remove`](#mountremove) | Stop syncing a mount; never deletes remote content |
| [`sync.pause`](#syncpause) | Pause syncing, for every mount or one, for a time or until resumed |
| [`sync.resume`](#syncresume) | Resume syncing |
| [`item.status`](#itemstatus) | State of a batch of paths, or of a directory and every child |
| [`item.pin`](#item-actions) | Always keep on this device |
| [`item.unpin`](#item-actions) | Clear the pin, back to `inherit` |
| [`item.hydrate`](#item-actions) | Download now without pinning |
| [`item.dehydrate`](#item-actions) | Free up space |
| [`item.import`](#itemimport) | Copy a local file into a mount (capability `item-import-export`) |
| [`item.export`](#itemexport) | Copy a file out of a mount (capability `item-import-export`) |
| [`status.subscribe`](#statussubscribe) | Turn this connection into a change stream |

| Notification | Sent when |
|---|---|
| [`status.changed`](#statuschanged) | Item state changes, on a subscribed connection |

### hello

Parameters: `client_name` (string, for logs) and `protocol_versions` (the major versions the
client supports). Result: `protocol_version` (the chosen major), `capabilities` (strings
naming optional features; clients ignore names they do not know) and `limits`. Version 1
defines one capability:

- `item-import-export`: the daemon offers [`item.import`](#itemimport) and
  [`item.export`](#itemexport). It may be withdrawn in a later release; without it both
  methods get `method-not-found`, as if they did not exist.

The limits:

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
{"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": 1, "capabilities": ["item-import-export"], "limits": {"max_line_bytes": 1048576, "max_action_paths": 100, "max_action_path_bytes": 524288, "max_status_paths": 10000, "max_status_path_bytes": 524288}}}
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
`account_id`, `kind` (`personal` or `work`, open), `tenant_id` (work accounts only),
`state` (`signed-in` or `needs-sign-in`, open), and, when known, `user_name` (the name the
account signs in with, often an email address) and `display_name` (the person's name), both
as Graph reports them, for display only and at most 1 KiB; and `next_cursor` while more
remain ([pagination](#pagination)). `account_id` and `tenant_id` have no length limit in the
protocol.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 3, "method": "account.list", "params": {}}
```

<!-- example: response account.list -->
```json
{"jsonrpc": "2.0", "id": 3, "result": {"accounts": [
  {"account_id": "acct-personal-1", "kind": "personal", "state": "signed-in", "user_name": "sam@example.com", "display_name": "Sam Example"},
  {"account_id": "acct-work-1", "kind": "work", "tenant_id": "00000000-0000-0000-0000-000000000001", "state": "needs-sign-in"}
]}}
```

### Sign-in and credentials

Signing in is two calls, because the user signs in in a browser between them
([F01](../flows/F01-sign-in.md)). The daemon owns everything secret: the PKCE verifier, the
`state` value, the loopback listener, the authorisation code and the tokens. **The only
sign-in datum a client receives is the authorisation URL** (ADR-0010 R6). It carries the PKCE
`code_challenge` with method `S256` and the `state`, never the verifier: the challenge is a
SHA-256 digest of the verifier and the verifier cannot be recovered from it. The protocol
types enforce what they can without knowing the authority. The URL must:

- use `https://`, or `http://` only to a loopback host, and have no user information;
- carry `code_challenge` (43 base64url characters) and `code_challenge_method=S256`, and no
  parameter named `code`, `code_verifier`, `client_secret`, `client_assertion`, `assertion`,
  `access_token`, `refresh_token`, `id_token`, `token` or `password`, in any case;
- name each parameter once, in plain (not percent-encoded) characters;
- be printable ASCII with no fragment, at most 4,096 bytes.

The attempt ID is at most 40 characters from `a-z`, `0-9` and `-`, and a confirmation token
at most 40 from `a-z`, `0-9` and `.`: both too short to hold a verifier (at least 43
characters), a code or a token.

Two request values are treated as credentials and **never appear in any result, notification
or error**: the redirect address pasted into `account.login.finish` (it holds the
authorisation code) and a sharing link given to `mount.add.preview` or `mount.add.confirm`
(whoever holds it may be able to open what it points to). For `account.login.finish`,
`mount.add.preview` and `mount.add.confirm`, an `invalid-params` message says what shape was
expected and repeats no value from the request. Sign-in errors are `sign-in-failed` with a
reason code and a fixed message for each reason, built from nothing the client sent; the
identity platform's own error text from a redirect is not passed on.

Free text remains in a few places, each capped at 1 KiB and each with one content rule:
`message` (the daemon's description of an error, which never quotes a credential or sharing
link), and `user_name`, `display_name`, `drive_name` and `site_name` (names as Graph reports
them, for display only).

### account.login.start

Parameters: `mode`, `browser` or `headless` (open, request only; required). Result:
`attempt_id` and `authorization_url`.

- `browser`: the daemon listens on a loopback port for the redirect. The client opens
  `authorization_url` in the user's browser (the CLI uses `xdg-open`, ADR-0010 R3) and then
  calls `account.login.finish` without `redirect_url`.
- `headless`: the URL uses the headless redirect URI. The client shows it, the user opens it
  on any device and signs in, and the client passes the address the browser ended on to
  `account.login.finish` as `redirect_url`.

An attempt lasts 10 minutes and is then discarded (F01 step 3).

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 30, "method": "account.login.start", "params": {"mode": "browser"}}
```

<!-- example: response account.login.start -->
```json
{"jsonrpc": "2.0", "id": 30, "result": {"attempt_id": "attempt-7f3a", "authorization_url": "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=00000000-0000-0000-0000-000000000000&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53211&scope=Files.ReadWrite.All%20offline_access%20User.Read&state=ZXhhbXBsZS1zdGF0ZQ&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256"}}
```

A mode this daemon does not know is refused, never replaced by a default:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 31, "method": "account.login.start", "params": {"mode": "device-code"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 31, "error": {"code": -32602, "message": "invalid params for account.login.start: unknown mode \"device-code\"; this daemon accepts browser, headless"}}
```

No loopback port could be opened:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 32, "method": "account.login.start", "params": {"mode": "browser"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 32, "error": {"code": 1011, "message": "sign-in could not start: no loopback port could be opened for the browser redirect; sign in with headless mode instead", "data": {"reason": "loopback-unavailable"}}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | `mode` missing or unknown | Fix the request |
| `sign-in-failed` | `loopback-unavailable`, or `no-secret-service` (the message names `mydrive doctor`) | Offer headless mode, or `mydrive doctor` |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### account.login.finish

Parameters: `attempt_id`, and for a headless attempt `redirect_url`, the whole address the
browser ended on, exactly as pasted (at most 8,192 bytes, no whitespace or control
characters; the client may trim surrounding whitespace before sending, the daemon never
does). For a browser attempt `redirect_url` is absent and the response arrives when the
browser has been redirected and the sign-in is complete, or when the attempt ends: a client
sends other requests on the same connection meanwhile and matches responses by `id`.
Giving `redirect_url` for a browser attempt, or leaving it out for a headless one, is
`invalid-params`.

Result: `account`, the signed-in account in the shape of an [`account.list`](#accountlist)
entry, with `user_name` so the client can show who signed in; and `proposed_mount`, absent
when the account's own drive is already mounted: `drive_id`, `drive_type` (open) and
`mount_point`, the default (`~/OneDrive`, or `~/OneDrive - <Org>` for a work account).
**Sign-in never creates a mount** (overview "Rulings on details"): the client shows the
account, asks, and only then calls [`mount.add.preview`](#mountaddpreview) with target
`own-drive`. Signing in again as an account that is already signed in replaces its tokens
and returns the same `account_id`.

A failure is `sign-in-failed` with a reason that says whether the attempt is still pending:

| Reason | Attempt | Meaning |
|---|---|---|
| `not-a-redirect` | pending | The pasted address is malformed or not a redirect to the attempt's URI: paste again |
| `authorization-error` | pending | The identity platform reported an error in the redirect (for example consent refused): paste again or start again |
| `state-mismatch` | cancelled | The redirect does not belong to this attempt (ADR-0010 R10) |
| `attempt-not-found` | gone | No pending attempt has this ID |
| `attempt-expired` | gone | The 10 minutes ran out |
| `code-rejected` | gone | The identity platform refused the code, for example because it expired |
| `no-secret-service` | gone | Nothing could be stored; nothing was persisted (ADR-0010 R7) |

A tenant that refuses the app, or a policy that requires a compliant device, is
`blocked-by-policy` with reason `tenant-refused` or `organisation-policy`; the attempt is gone.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 33, "method": "account.login.finish", "params": {"attempt_id": "attempt-7f3b", "redirect_url": "https://login.microsoftonline.com/common/oauth2/nativeclient?code=M.C5_EXAMPLE.not-a-real-code&state=ZXhhbXBsZS1zdGF0ZQ"}}
```

<!-- example: response account.login.finish -->
```json
{"jsonrpc": "2.0", "id": 33, "result": {
  "account": {"account_id": "acct-personal-1", "kind": "personal", "state": "signed-in", "user_name": "sam@example.com", "display_name": "Sam Example"},
  "proposed_mount": {"drive_id": "0123456789abcdef", "drive_type": "personal", "mount_point": "/home/user/OneDrive"}
}}
```

A pasted address whose `state` does not match:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 34, "method": "account.login.finish", "params": {"attempt_id": "attempt-7f3b", "redirect_url": "https://login.microsoftonline.com/common/oauth2/nativeclient?code=M.C5_EXAMPLE.not-a-real-code&state=b3RoZXItc3RhdGU"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 34, "error": {"code": 1011, "message": "sign-in failed: the response does not belong to this attempt, so the attempt was cancelled; start a new sign-in", "data": {"reason": "state-mismatch"}}}
```

A pasted address with a line break in it is refused without repeating it:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 35, "method": "account.login.finish", "params": {"attempt_id": "attempt-7f3b", "redirect_url": "https://login.microsoftonline.com/common/oauth2/nativeclient?code=M.C5_EXAMPLE.not-a-real-code\n&state=ZXhhbXBsZS1zdGF0ZQ"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 35, "error": {"code": -32602, "message": "invalid params for account.login.finish: expected an object with attempt_id (an attempt ID) and, for a headless attempt, redirect_url (the pasted address); the values given are not repeated here because they may hold a credential or a sharing link"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters, or `redirect_url` given or missing against the attempt's mode | Fix the request; the message repeats no value |
| `sign-in-failed` | See the reasons above | Paste again when the attempt is pending; otherwise start a new sign-in |
| `blocked-by-policy` | The organisation refuses the app or requires a compliant device | Show the message; an administrator must act |
| `offline` | The identity platform or Graph cannot be reached | Retry; a pending attempt stays pending |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### account.logout

Parameters: `account_id`, and optionally `dry_run` (`true` to report what signing out would
find and change nothing; absent or `false` to sign out). There is no parameter that deletes
anything: signing out removes the account's tokens from the Secret Service and never deletes
local or remote content. The account's mounts stay configured; their local changes are kept
and upload after the account signs in again (F01 "Sign-out").

Result: `account_id`, `signed_out` (`false` for a dry run) and `unsynced_files`, the files in
the account's mounts with changes not yet on the server. A client calls with `dry_run` first
and warns when `unsynced_files` is not zero.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 36, "method": "account.logout", "params": {"account_id": "acct-work-1", "dry_run": true}}
```

<!-- example: response account.logout -->
```json
{"jsonrpc": "2.0", "id": 36, "result": {"account_id": "acct-work-1", "signed_out": false, "unsynced_files": 3}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 37, "method": "account.logout", "params": {"account_id": "acct-work-1"}}
```

<!-- example: response account.logout -->
```json
{"jsonrpc": "2.0", "id": 37, "result": {"account_id": "acct-work-1", "signed_out": true, "unsynced_files": 3}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 38, "method": "account.logout", "params": {"account_id": "acct-gone"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 38, "error": {"code": 1012, "message": "no account has the given account_id; list accounts with account.list"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters | Fix the request |
| `unknown-account` | No account has this ID | List accounts again |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### mount.list

Parameters: optional `cursor`. Result: `mounts`, in byte order of `mount_id`, each with
`mount_id`, `account_id`, `drive_id` (lowercase), `mount_point`, `state` (open), `read_only`,
and, only when `blocked`, `blocked_reason` (a code, open) and `blocked_message` (text for
people, at most 1 KiB); when known, `drive_type` (`personal`, `business` or
`document-library`, open) and `item_count` (the items of the drive the daemon knows; while
the mount is `resyncing` for its first enumeration, the items listed so far); and
`next_cursor` while more remain ([pagination](#pagination)).

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
  {"mount_id": "personal", "account_id": "acct-personal-1", "drive_id": "0123456789abcdef", "mount_point": "/home/user/OneDrive", "state": "active", "read_only": false, "drive_type": "personal", "item_count": 14000}
]}}
```

### mount.add.preview

Parameters, the **target**: `account_id`, the account whose access is used, and `target`
(open, request only), one of:

- `own-drive`: the account's own drive, with no other target field;
- `drive-id`: a drive named by `drive_id`;
- `sharing-link`: the drive a sharing link points into, named by `sharing_link` (an
  `https://` address of at most 4,096 bytes with no whitespace or control characters). The
  link is never repeated in any response.

Any other combination (a target field that does not belong to `target`, or a missing one) is
`invalid-params`. The preview never changes the user's access: a sharing link is resolved
for this request only (ADR-0015 R2). In M1 a mount is always a drive's root, so a link to a
folder inside a library previews the whole library.

Result: `confirmation_token`; `account_id`; `drive_id`; `drive_type` (open); `drive_name`,
the drive's name as Graph reports it (for a library, the library's name); `site_name`, for a
library; `item_count`, when known; `read_only`; and `mount_point`, the proposed mount point.
Nothing is created. Names are for display and at most 1 KiB.

The token is opaque, at most 40 characters, and valid for 10 minutes, for one
`mount.add.confirm` with **the same target parameters**. It does not encode the target
readably. The daemon keeps a bounded number of previews waiting; an old preview may be
forgotten, and its token is then refused like an expired one.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 39, "method": "mount.add.preview", "params": {"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": "https://example.sharepoint.com/:f:/s/Finance/EjK3pQ2-example-only"}}
```

<!-- example: response mount.add.preview -->
```json
{"jsonrpc": "2.0", "id": 39, "result": {"confirmation_token": "c1.0000000000000007.5e1d09a2c4b7f813", "account_id": "acct-work-1", "drive_id": "b!financedriveid", "drive_type": "document-library", "drive_name": "Documents", "site_name": "Finance", "item_count": 8400, "read_only": false, "mount_point": "/home/user/Example/Finance - Documents"}}
```

A link to a folder in another person's OneDrive:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 40, "method": "mount.add.preview", "params": {"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": "https://example-my.sharepoint.com/:f:/p/someone/EaB-example-only"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 40, "error": {"code": 1004, "message": "this link points to a shared folder in another person's OneDrive, which cannot be mounted yet", "data": {"reason": "unsupported-target"}}}
```

A target this daemon does not know is refused, and the message repeats no value:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 41, "method": "mount.add.preview", "params": {"account_id": "acct-work-1", "target": "teams-channel", "team_id": "t1"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 41, "error": {"code": -32602, "message": "invalid params for mount.add.preview: expected an object with account_id, and target own-drive alone, drive-id with drive_id, or sharing-link with sharing_link (an https:// address); the values given are not repeated here because they may hold a credential or a sharing link"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters, an unknown `target`, or target fields that do not fit it | Fix the request; the message repeats no value |
| `unknown-account` | No account has this ID | List accounts again |
| `needs-sign-in` | The account must sign in again | Offer `account.login.start` |
| `blocked-by-policy` | Organisation policy refuses access to the drive, or the tenant refuses the app | Show the message; nothing was mounted |
| `refused` | `no-access` (the account cannot open this link or drive), `unsupported-target` (for example a shared folder in another person's OneDrive), `already-mounted` (`data.mount_id` names the mount) | Show the message; nothing changed |
| `offline` | Graph cannot be reached | Retry |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### mount.add.confirm

Parameters: the preview's target parameters, unchanged (`account_id`, `target`, and
`drive_id` or `sharing_link`); `confirmation_token`; and optionally `mount_point`, an
absolute, canonical path to use instead of the proposed one. Only now is a sharing link
redeemed durably (ADR-0015 R2) and the mount created.

Result: `mount`, the new mount in the shape of a [`mount.list`](#mountlist) entry. Its state is
`resyncing` until its first full delta has been applied (overview "First enumeration").

A token is accepted **once**. A token that is unknown, already used, expired, forgotten, or
offered with other target parameters is `invalid-params` with `data.reason`
`restart-preview`: call `mount.add.preview` again. Offering a token with the wrong target
does not use it up. The mount point must not exist, or be an empty directory, and must not be
inside or contain another mount or the daemon's data directory.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 42, "method": "mount.add.confirm", "params": {"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": "https://example.sharepoint.com/:f:/s/Finance/EjK3pQ2-example-only", "confirmation_token": "c1.0000000000000007.5e1d09a2c4b7f813"}}
```

<!-- example: response mount.add.confirm -->
```json
{"jsonrpc": "2.0", "id": 42, "result": {"mount": {"mount_id": "example-finance-documents", "account_id": "acct-work-1", "drive_id": "b!financedriveid", "mount_point": "/home/user/Example/Finance - Documents", "state": "resyncing", "read_only": false, "drive_type": "document-library", "item_count": 0}}}
```

The same token again:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 43, "method": "mount.add.confirm", "params": {"account_id": "acct-work-1", "target": "sharing-link", "sharing_link": "https://example.sharepoint.com/:f:/s/Finance/EjK3pQ2-example-only", "confirmation_token": "c1.0000000000000007.5e1d09a2c4b7f813"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 43, "error": {"code": -32602, "message": "invalid params for mount.add.confirm: the confirmation token is unknown, already used, expired, or was issued for other parameters; call mount.add.preview again", "data": {"reason": "restart-preview"}}}
```

The mount point is a directory that is not empty:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 44, "method": "mount.add.confirm", "params": {"account_id": "acct-personal-1", "target": "own-drive", "confirmation_token": "c1.0000000000000008.0b44c1d7a9e2f356"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 44, "error": {"code": 1004, "message": "/home/user/OneDrive is not empty; choose another mount point", "data": {"reason": "mount-point-not-empty"}}}
```

The drive is already mounted:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 45, "error": {"code": 1004, "message": "this drive is already mounted at /home/user/OneDrive", "data": {"reason": "already-mounted", "mount_id": "personal"}}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters; with `data.reason` `restart-preview`, a token that cannot be used | Fix the request, or preview again |
| `unknown-account` | No account has this ID | List accounts again |
| `needs-sign-in` | The account must sign in again | Offer `account.login.start` |
| `blocked-by-policy` | Organisation policy refuses access | Show the message; nothing was mounted |
| `refused` | `mount-point-not-empty`, `mount-point-overlaps`, `already-mounted` (with `data.mount_id`), `no-access` | Show the message; nothing was mounted |
| `offline` | Graph cannot be reached | Retry with the same token while it is valid |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### mount.remove

Parameters: `mount_id`, and optionally `dry_run` (`true` to report and change nothing).
There is no parameter that deletes remote content: **removing a mount never deletes anything
on the server** (ADR-0015, J15). Each fully local file is kept in the mount point, which
becomes a plain directory (ADR-0021); placeholders are not kept.

Result: `mount_id`; `removed` (`false` for a dry run); `mount_point`, where the local copies
are kept; `kept_files`, the fully local files kept there; and `unsynced_files`, the files whose
changes are not yet on the server. A client calls with `dry_run` first and warns when
`unsynced_files` is not zero. What the daemon does with unsynced changes on removal is
decided by F12; it never drops them silently.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 46, "method": "mount.remove", "params": {"mount_id": "example-site-documents", "dry_run": true}}
```

<!-- example: response mount.remove -->
```json
{"jsonrpc": "2.0", "id": 46, "result": {"mount_id": "example-site-documents", "removed": false, "mount_point": "/home/user/Example/Site - Documents", "kept_files": 140, "unsynced_files": 0}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 47, "method": "mount.remove", "params": {"mount_id": "example-site-documents"}}
```

<!-- example: response mount.remove -->
```json
{"jsonrpc": "2.0", "id": 47, "result": {"mount_id": "example-site-documents", "removed": true, "mount_point": "/home/user/Example/Site - Documents", "kept_files": 140, "unsynced_files": 0}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 48, "method": "mount.remove", "params": {"mount_id": "no-such-mount"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 48, "error": {"code": 1013, "message": "no mount has the ID no-such-mount"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters | Fix the request |
| `unknown-mount` | No mount has this ID | List mounts again |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### sync.pause

Parameters, both optional: `mount_id`, the mount to pause (absent for every mount), and
`duration_secs`, how long, from 1 to 604,800 seconds (7 days; absent to pause until
`sync.resume`). A longer pause is a pause with no duration. Pausing a paused mount replaces
its end time. While paused, a mount sends no uploads or deletes and runs no delta; reading
and hydrating still work, and local changes are kept for later (J15).

Result: `paused_mounts`, how many mounts this request paused, and `paused_until`, when the
pause ends, in seconds since the Unix epoch (UTC), absent for a pause with no duration.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 49, "method": "sync.pause", "params": {"duration_secs": 7200}}
```

<!-- example: response sync.pause -->
```json
{"jsonrpc": "2.0", "id": 49, "result": {"paused_mounts": 4, "paused_until": 1794790800}}
```

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 50, "method": "sync.pause", "params": {"duration_secs": 0}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 50, "error": {"code": -32602, "message": "invalid params for sync.pause: duration_secs is 0; give 1 to 604800 seconds, or leave it out to pause until resumed"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters, or `duration_secs` out of range | Fix the request |
| `unknown-mount` | No mount has this ID | List mounts again |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### sync.resume

Parameters: optional `mount_id`, the mount to resume (absent for every mount). Result:
`resumed_mounts`, how many paused mounts this request resumed. Resuming a mount that is not
paused does nothing and is not an error.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 51, "method": "sync.resume", "params": {}}
```

<!-- example: response sync.resume -->
```json
{"jsonrpc": "2.0", "id": 51, "result": {"resumed_mounts": 4}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 52, "method": "sync.resume", "params": {"mount_id": "no-such-mount"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 52, "error": {"code": 1013, "message": "no mount has the ID no-such-mount"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters | Fix the request |
| `unknown-mount` | No mount has this ID | List mounts again |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

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

### item.import

Offered only while `hello` advertises `item-import-export`; otherwise `method-not-found`.

Parameters: `source`, a [local path](#conventions) to a regular file; `destination`, the
file's path in a mount, whose parent folder must exist; and `mode` (open, request only;
required, with no default):

- `create`: the destination must not exist (`refused`, `already-exists`, if it does);
- `replace`: the destination must be an existing file (`unknown-path` if there is none,
  `refused` `not-a-file` for a folder). **A file with changes not yet on the server is never
  replaced**: that is `refused` with reason `not-in-sync` and `data.sync`, and nothing
  changes.

The content is copied into the mount and the change is recorded durably before the response
is sent; the upload follows in the background and is reported by `status.changed` (F04
step 18). Result: `path` (the destination), `item` (its state, normally sync
`pending-upload`) and `bytes_copied`.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 53, "method": "item.import", "params": {"source": "/home/user/Downloads/report.pdf", "destination": "/home/user/OneDrive/Documents/report.pdf", "mode": "create"}}
```

<!-- example: response item.import -->
```json
{"jsonrpc": "2.0", "id": 53, "result": {"path": "/home/user/OneDrive/Documents/report.pdf", "item": {"status": "syncing", "modifiers": [], "hydration": "full", "pin": "inherit", "effective_pin": "unpinned", "sync": "pending-upload"}, "bytes_copied": 482113}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 54, "method": "item.import", "params": {"source": "/home/user/Downloads/missing.pdf", "destination": "/home/user/OneDrive/Documents/missing.pdf", "mode": "create"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 54, "error": {"code": 1014, "message": "cannot read /home/user/Downloads/missing.pdf: no such file", "data": {"reason": "not-found"}}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 55, "method": "item.import", "params": {"source": "/home/user/Downloads/notes.md", "destination": "/home/user/OneDrive/Documents/notes.md", "mode": "replace"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 55, "error": {"code": 1004, "message": "/home/user/OneDrive/Documents/notes.md has changes not yet uploaded and was not replaced", "data": {"reason": "not-in-sync", "sync": "pending-upload"}}}
```

A source that is not an absolute, canonical path:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 56, "method": "item.import", "params": {"source": "~/Downloads/report.pdf", "destination": "/home/user/OneDrive/Documents/report.pdf", "mode": "create"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 56, "error": {"code": -32602, "message": "invalid params for item.import: path \"~/Downloads/report.pdf\" is not a valid protocol path: it is not absolute"}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters, a path that is not absolute and canonical, or an unknown `mode` | Fix the request |
| `method-not-found` | `hello` did not advertise `item-import-export` | Do not offer import |
| `not-in-a-mount` | `destination` is outside every mount | Fix the destination |
| `unknown-path` | The destination's folder, or the file to replace, does not exist | Fix the destination |
| `local-file-error` | The source cannot be used: `not-found`, `not-a-file`, `permission-denied`, `inside-mount`, `io-error` | Show the message |
| `refused` | `already-exists`, `not-a-file`, `not-in-sync` (never replaced), `name-not-allowed`, `insufficient-space` | Show the message; nothing changed |
| `read-only` | The mount or folder is read-only | Show the message |
| `blocked-by-policy` | Organisation policy refuses access | Show the message |
| `needs-sign-in` | Replacing needs the server and the account must sign in again | Offer `account.login.start` |
| `offline` | Replacing needs the server and the daemon is offline | Retry later |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

### item.export

Offered only while `hello` advertises `item-import-export`; otherwise `method-not-found`.

Parameters: `source`, a file in a mount; `destination`, a [local path](#conventions) whose
directory exists; and `mode` (open, request only; required, with no default): `create` when
nothing may exist at the destination (`local-file-error`, `already-exists`, if something
does), or `replace` to replace what is there. The daemon hydrates the file first if it is not
local (F03), then writes a new file beside the destination and renames it into place, so a
failed export never leaves a partly written file at the destination. Nothing in the mount
changes, and the item's pin is not touched.

Result: `path` (the source), `destination` and `bytes_copied`.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 57, "method": "item.export", "params": {"source": "/home/user/OneDrive/Documents/budget.xlsx", "destination": "/home/user/Desktop/budget.xlsx", "mode": "create"}}
```

<!-- example: response item.export -->
```json
{"jsonrpc": "2.0", "id": 57, "result": {"path": "/home/user/OneDrive/Documents/budget.xlsx", "destination": "/home/user/Desktop/budget.xlsx", "bytes_copied": 91337}}
```

From a daemon that does not advertise `item-import-export`:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 58, "method": "item.export", "params": {"source": "/home/user/OneDrive/Documents/budget.xlsx", "destination": "/home/user/Desktop/budget.xlsx", "mode": "create"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 58, "error": {"code": -32601, "message": "method \"item.export\" is not offered by this daemon: hello did not advertise the capability item-import-export"}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 59, "method": "item.export", "params": {"source": "/home/user/OneDrive/Documents/budget.xlsx", "destination": "/home/user/Desktop/budget.xlsx", "mode": "create"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 59, "error": {"code": 1014, "message": "/home/user/Desktop/budget.xlsx already exists; export with mode replace to replace it", "data": {"reason": "already-exists"}}}
```

| Error | When | What the client does |
|---|---|---|
| `invalid-params` | Malformed parameters, a path that is not absolute and canonical, or an unknown `mode` | Fix the request |
| `method-not-found` | `hello` did not advertise `item-import-export` | Do not offer export |
| `not-in-a-mount` | `source` is outside every mount | Fix the source |
| `unknown-path` | No item at `source` | Fix the source |
| `local-file-error` | The destination cannot be written: `not-found` (its directory), `already-exists`, `not-a-file`, `permission-denied`, `inside-mount`, `no-space`, `io-error` | Show the message |
| `refused` | `not-a-file` (the source is a folder), `remote-changed`, `insufficient-space` | Show the message |
| `blocked-by-policy` | Organisation policy refuses access | Show the message |
| `needs-sign-in` | The file must be downloaded and the account must sign in again | Offer `account.login.start` |
| `offline` | The file must be downloaded and the daemon is offline | Retry later |
| `hello-required` | Before `hello` | Fix the client |
| `not-ready` | The daemon is starting | Retry shortly |
| `internal` | A daemon fault | Report it |

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
| -32601 | `method-not-found` | No such method in this version, or `item.import` or `item.export` while `hello` did not advertise `item-import-export` | | top level |
| -32602 | `invalid-params` | Parameters malformed, including any relative or non-canonical path, a cursor not issued for these parameters, a confirmation token that cannot be used, a file given as `directory`, an unknown request value, or a request over the `hello` limits | `reason` for a cursor or a confirmation token | top level |
| -32603 | `internal` | A daemon fault | | both |
| 1001 | `not-ready` | The daemon is still starting; retry later | | top level |
| 1002 | `unknown-path` | Under a mount point, but no such item | | both |
| 1003 | `not-in-a-mount` | Not under any mount point | | both |
| 1004 | `refused` | Understood and refused; nothing changed | `reason`, `sync` for `not-in-sync`, `mount_id` for `already-mounted` | both |
| 1005 | `needs-sign-in` | The account must sign in again | | both |
| 1006 | `blocked-by-policy` | Organisation policy refuses access | `reason` | both |
| 1007 | `read-only` | The item or its mount is read-only | `reason` | both |
| 1008 | `offline` | The request needs the server and the daemon is offline | | both |
| 1009 | `unsupported-version` | `hello` offered no supported version | `supported_versions` | top level |
| 1010 | `hello-required` | A request other than `hello` before `hello` completed; a client bug, retrying does not help | | top level |
| 1011 | `sign-in-failed` | A sign-in could not start or finish; the message is fixed per reason and never repeats a request value | `reason` | top level |
| 1012 | `unknown-account` | No account has this `account_id` | | top level |
| 1013 | `unknown-mount` | No mount has this `mount_id` | | top level |
| 1014 | `local-file-error` | A local path given to `item.import` or `item.export` could not be used | `reason` | top level |

### Where each error can appear

For each method, the codes it can return at the top level and per path. Any request can also
get `parse-error` and `invalid-request` (envelope problems, a second `hello`, a request on a
subscribed connection) and `method-not-found` for a method that does not exist; those are not
repeated below.

| Method | Top level | Per path |
|---|---|---|
| `hello` | `invalid-params`, `unsupported-version`, `internal` | — |
| `daemon.status` | `invalid-params`, `hello-required`, `internal` | — |
| `account.list` | `invalid-params`, `hello-required`, `not-ready`, `internal` | — |
| `account.login.start` | `invalid-params`, `sign-in-failed`, `hello-required`, `not-ready`, `internal` | — |
| `account.login.finish` | `invalid-params`, `sign-in-failed`, `blocked-by-policy`, `offline`, `hello-required`, `not-ready`, `internal` | — |
| `account.logout` | `invalid-params`, `unknown-account`, `hello-required`, `not-ready`, `internal` | — |
| `mount.list` | `invalid-params`, `hello-required`, `not-ready`, `internal` | — |
| `mount.add.preview` | `invalid-params`, `unknown-account`, `needs-sign-in`, `blocked-by-policy`, `refused`, `offline`, `hello-required`, `not-ready`, `internal` | — |
| `mount.add.confirm` | `invalid-params`, `unknown-account`, `needs-sign-in`, `blocked-by-policy`, `refused`, `offline`, `hello-required`, `not-ready`, `internal` | — |
| `mount.remove` | `invalid-params`, `unknown-mount`, `hello-required`, `not-ready`, `internal` | — |
| `sync.pause` | `invalid-params`, `unknown-mount`, `hello-required`, `not-ready`, `internal` | — |
| `sync.resume` | `invalid-params`, `unknown-mount`, `hello-required`, `not-ready`, `internal` | — |
| `item.status` | `invalid-params`, `not-in-a-mount`, `unknown-path`, `hello-required`, `not-ready`, `internal` | `not-in-a-mount`, `unknown-path`, `internal` |
| `item.pin` | `invalid-params`, `hello-required`, `not-ready`, `internal` | `not-in-a-mount`, `unknown-path`, `refused`, `needs-sign-in`, `blocked-by-policy`, `read-only`, `offline`, `internal` |
| `item.unpin` | `invalid-params`, `hello-required`, `not-ready`, `internal` | `not-in-a-mount`, `unknown-path`, `refused`, `needs-sign-in`, `blocked-by-policy`, `read-only`, `offline`, `internal` |
| `item.hydrate` | `invalid-params`, `hello-required`, `not-ready`, `internal` | `not-in-a-mount`, `unknown-path`, `refused`, `needs-sign-in`, `blocked-by-policy`, `read-only`, `offline`, `internal` |
| `item.dehydrate` | `invalid-params`, `hello-required`, `not-ready`, `internal` | `not-in-a-mount`, `unknown-path`, `refused`, `needs-sign-in`, `blocked-by-policy`, `read-only`, `offline`, `internal` |
| `item.import` | `invalid-params`, `method-not-found`, `not-in-a-mount`, `unknown-path`, `local-file-error`, `refused`, `read-only`, `blocked-by-policy`, `needs-sign-in`, `offline`, `hello-required`, `not-ready`, `internal` | — |
| `item.export` | `invalid-params`, `method-not-found`, `not-in-a-mount`, `unknown-path`, `local-file-error`, `refused`, `blocked-by-policy`, `needs-sign-in`, `offline`, `hello-required`, `not-ready`, `internal` | — |
| `status.subscribe` | `invalid-params`, `hello-required`, `not-ready`, `internal` | — |

The codes are the outcome a client acts on. A CLI maps them to its exit status: success to
0; a top-level error to a failure; a result whose per-path entries hold errors to "partly
done"; and a connection that cannot be made to "daemon not running". With `--json` it prints
the `result` object on success and the `error` object on failure.

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
