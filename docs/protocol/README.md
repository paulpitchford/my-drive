# my-drive IPC protocol, version 1

The wire protocol between `mydrived` and every client: the CLI, the UI, the tray and the
file-manager adapters. Decided in [ADR-0009](../adr/0009-ipc-versioned-json-rpc-over-unix-socket.md);
the Rust types are in `crates/mydrive-ipc`. How adapters use it is shown in
[F11](../flows/F11-status-for-file-managers.md).

Every JSON example on this page is parsed by a test (`crates/mydrive-ipc/tests/protocol_examples.rs`)
into the matching Rust type and serialised back to the same JSON, and every request marked
as rejected must produce exactly the error example that follows it. An example that drifts
from the code fails the build. The `<!-- example: ... -->` comments label the examples for
that test.

## Transport and framing

- A Unix stream socket at `$XDG_RUNTIME_DIR/mydrive/daemon.sock`. The directory has mode
  `0700` and the socket `0600`; only the user's own processes can connect.
- JSON-RPC 2.0. Each message, in either direction, is **one JSON object on one line**,
  terminated by `\n`. Encoders emit compact JSON, so newlines inside strings are always
  escaped.
- A line is at most **1 MiB** (1,048,576 bytes, not counting the `\n`). A longer line is
  discarded up to the next `\n` and answered with `invalid-request`; the connection stays
  open and the next line is read normally. Lines that are not UTF-8 or not one JSON document
  get `parse-error`. These errors carry `"id": null`.
- Blank lines are ignored.
- Parameters are always passed by name, as an object. Positional (array) parameters get
  `invalid-params`.

## Conventions

- Field names are `snake_case`. Enumerated values are lowercase kebab-case strings, never
  objects or tagged unions.
- Item state uses the wire forms of `docs/architecture/overview.md` ("Wire forms"):
  hydration `placeholder`, `partial`, `full`; pin `inherit`, `pinned`, `unpinned`; effective
  pin `pinned`, `unpinned`; sync `in-sync`, `pending-download`, `pending-upload`,
  `uploading`, `conflict`, `error`, `blocked`; display status `blocked`, `error`, `syncing`,
  `pinned`, `local`, `online-only`.
- **Paths** are absolute UTF-8 paths under a mount point, never backing-directory paths. A
  path that is empty, relative, contains a NUL byte, or has a `.` or `..` component is
  rejected with `invalid-params`. A path outside every mount gets `not-in-a-mount`.
- Optional fields are omitted when they have no value, rather than sent as `null`.
- **Unknown fields are ignored**, by the daemon and by clients, at every level. Clients also
  ignore unknown notification methods and unknown capabilities.
- Clients draw from `status` and `modifiers`. They never derive a status from the axes
  themselves; the axes are there for clients that want detail.

## Versioning

- The protocol has an integer **major version**. This page describes version `1`.
- Within a major version, changes are **additive only**: new methods, new optional
  parameters, new result fields, new notification methods, new capabilities, new error codes
  and new refusal reasons. Clients must tolerate all of these.
- Removing anything, renaming anything, making an optional parameter required, or changing
  what a field or value means requires a new major version.
- The first request on every connection is `hello`. The client lists the major versions it
  supports; the daemon picks the highest one both support, or answers `unsupported-version`
  with the list it supports.
- A daemon may support several major versions at once and must keep each working for as long
  as it is supported.

## Connections

1. Connect and send `hello`. Any other request first gets `not-ready` and has no effect.
   A second `hello` on the same connection gets `invalid-request`.
2. Send requests and read responses. Responses carry the request's `id`.
3. A connection that calls `status.subscribe` becomes a change stream: from then on it
   carries `status.changed` notifications only, and any request on it gets
   `invalid-request`. A client that also needs requests opens a second connection.

## Methods

| Method | Does |
|---|---|
| [`hello`](#hello) | Handshake: negotiate the protocol version |
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
client supports). Result: `protocol_version` (the chosen major) and `capabilities` (strings
naming optional features; version 1 defines none, and clients ignore names they do not know).

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client_name": "mydrive-cli", "protocol_versions": [1]}}
```

<!-- example: response hello -->
```json
{"jsonrpc": "2.0", "id": 1, "result": {"protocol_version": 1, "capabilities": []}}
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

No parameters. Result: `accounts`, each with `account_id`, `kind` (`personal` or `work`),
`tenant_id` (work accounts only) and `state` (`signed-in` or `needs-sign-in`).

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

No parameters. Result: `mounts`, each with `mount_id`, `account_id`, `drive_id` (lowercase),
`mount_point`, `state` (`active`, `paused`, `blocked` or `resyncing`), `blocked_reason`
(only when `blocked`) and `read_only`.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 4, "method": "mount.list"}
```

<!-- example: response mount.list -->
```json
{"jsonrpc": "2.0", "id": 4, "result": {"mounts": [
  {"mount_id": "personal", "account_id": "acct-personal-1", "drive_id": "0123456789abcdef", "mount_point": "/home/user/OneDrive", "state": "active", "read_only": false},
  {"mount_id": "example-site-documents", "account_id": "acct-work-1", "drive_id": "b!exampledriveid", "mount_point": "/home/user/Example/Site - Documents", "state": "blocked", "blocked_reason": "the organisation does not allow access from unmanaged devices", "read_only": true}
]}}
```

### item.status

Parameters: exactly one of

- `paths`: a list of paths. The result's `entries` has one entry per path, in request order.
- `directory`: one directory. The result's `directory` is the directory's own entry and
  `entries` has one entry per child, all in this one response. Adapters fetch a folder this
  way and never ask once per file.

Each entry has `path` and either `item` or `error`. `item` holds `status` (display status),
`modifiers` (`read_only`, `shortcut`), and the axes `hydration`, `pin` (as set on the item),
`effective_pin` (after inheritance) and `sync`. `error` is an [error object](#errors):
`not-in-a-mount` for a path outside every mount, `unknown-path` for a path in a mount that
names no known item.

Status queries are answered from the daemon's state store. They never touch the mount point
and never cause a download.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 5, "method": "item.status", "params": {"paths": ["/home/user/OneDrive/Documents/notes.md", "/home/user/Downloads/x.pdf"]}}
```

<!-- example: response item.status -->
```json
{"jsonrpc": "2.0", "id": 5, "result": {"entries": [
  {"path": "/home/user/OneDrive/Documents/notes.md", "item": {"status": "syncing", "modifiers": {"read_only": false, "shortcut": false}, "hydration": "full", "pin": "inherit", "effective_pin": "unpinned", "sync": "pending-upload"}},
  {"path": "/home/user/Downloads/x.pdf", "error": {"code": 1003, "message": "/home/user/Downloads/x.pdf is not under any mount point"}}
]}}
```

A directory:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 6, "method": "item.status", "params": {"directory": "/home/user/OneDrive/Pictures/2026"}}
```

<!-- example: response item.status -->
```json
{"jsonrpc": "2.0", "id": 6, "result": {
  "directory": {"path": "/home/user/OneDrive/Pictures/2026", "item": {"status": "pinned", "modifiers": {"read_only": false, "shortcut": false}, "hydration": "full", "pin": "pinned", "effective_pin": "pinned", "sync": "in-sync"}},
  "entries": [
    {"path": "/home/user/OneDrive/Pictures/2026/IMG_2001.jpg", "item": {"status": "syncing", "modifiers": {"read_only": false, "shortcut": false}, "hydration": "placeholder", "pin": "inherit", "effective_pin": "pinned", "sync": "pending-download"}},
    {"path": "/home/user/OneDrive/Pictures/2026/IMG_2002.jpg", "item": {"status": "pinned", "modifiers": {"read_only": false, "shortcut": false}, "hydration": "full", "pin": "inherit", "effective_pin": "pinned", "sync": "in-sync"}},
    {"path": "/home/user/OneDrive/Pictures/2026/Shared album", "item": {"status": "online-only", "modifiers": {"read_only": true, "shortcut": true}, "hydration": "placeholder", "pin": "unpinned", "effective_pin": "unpinned", "sync": "in-sync"}}
  ]
}}
```

A relative path is rejected:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 7, "method": "item.status", "params": {"paths": ["OneDrive/notes.md"]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 7, "error": {"code": -32602, "message": "invalid params for item.status: path \"OneDrive/notes.md\" is not a valid protocol path: it is not absolute"}}
```

### Item actions

`item.pin`, `item.unpin`, `item.hydrate` and `item.dehydrate` all take `paths` and return
`entries`: one per path, in request order, each with `path` and, when the action was not
accepted for that path, `error`. An entry without `error` was accepted. Acceptance means the
request was recorded; downloads that follow happen in the background and are reported by
`status.changed`. A refused action changes nothing for that path.

- `item.pin`: set the pin to `pinned`; the daemon hydrates the item (and, for a folder, its
  contents) and keeps it hydrated.
- `item.unpin`: set the pin back to `inherit`. Nothing is dehydrated.
- `item.hydrate`: download now, without changing the pin.
- `item.dehydrate`: free up space. This is the user-facing action: it first checks the item
  is `in-sync` (refusing with `not-in-sync` otherwise, before anything changes), then sets
  the pin to `unpinned` and releases the local content.

Refusals use code `refused` with `data.reason`: `not-in-sync` (with `data.sync`), `pinned`,
`in-use`, `remote-changed` or `insufficient-space`. Clients must accept reasons they do not
know.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 8, "method": "item.pin", "params": {"paths": ["/home/user/OneDrive/Pictures/2026"]}}
```

<!-- example: response item.pin -->
```json
{"jsonrpc": "2.0", "id": 8, "result": {"entries": [{"path": "/home/user/OneDrive/Pictures/2026"}]}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 9, "method": "item.unpin", "params": {"paths": ["/home/user/OneDrive/Pictures/2026"]}}
```

<!-- example: response item.unpin -->
```json
{"jsonrpc": "2.0", "id": 9, "result": {"entries": [{"path": "/home/user/OneDrive/Pictures/2026"}]}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 10, "method": "item.hydrate", "params": {"paths": ["/home/user/OneDrive/Documents/budget.xlsx", "/home/user/Example/Site - Documents/plan.docx"]}}
```

<!-- example: response item.hydrate -->
```json
{"jsonrpc": "2.0", "id": 10, "result": {"entries": [
  {"path": "/home/user/OneDrive/Documents/budget.xlsx"},
  {"path": "/home/user/Example/Site - Documents/plan.docx", "error": {"code": 1006, "message": "the organisation does not allow access from unmanaged devices"}}
]}}
```

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 11, "method": "item.dehydrate", "params": {"paths": ["/home/user/OneDrive/Videos/2025", "/home/user/OneDrive/Documents/notes.md"]}}
```

<!-- example: response item.dehydrate -->
```json
{"jsonrpc": "2.0", "id": 11, "result": {"entries": [
  {"path": "/home/user/OneDrive/Videos/2025"},
  {"path": "/home/user/OneDrive/Documents/notes.md", "error": {"code": 1004, "message": "dehydrate refused: sync state is pending-upload, not in-sync", "data": {"reason": "not-in-sync", "sync": "pending-upload"}}}
]}}
```

### status.subscribe

Parameters: `paths`, the mount points or folders to watch; empty or absent means every
mount. Result: an empty object. From then on the connection carries `status.changed`
notifications only.

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 12, "method": "status.subscribe", "params": {"paths": ["/home/user/OneDrive"]}}
```

<!-- example: response status.subscribe -->
```json
{"jsonrpc": "2.0", "id": 12, "result": {}}
```

A request on a subscribed connection:

<!-- example: request -->
```json
{"jsonrpc": "2.0", "id": 13, "method": "mount.list"}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 13, "error": {"code": -32600, "message": "mount.list refused: this connection is subscribed and carries notifications only; open a second connection for requests"}}
```

### status.changed

A notification (no `id`, no response) on a subscribed connection. Parameters: `entries`, in
the same shape as `item.status` entries, with each changed item's new state. An entry whose
`error` is `unknown-path` means the item no longer exists.

<!-- example: notification -->
```json
{"jsonrpc": "2.0", "method": "status.changed", "params": {"entries": [
  {"path": "/home/user/OneDrive/Pictures/2026/IMG_2001.jpg", "item": {"status": "pinned", "modifiers": {"read_only": false, "shortcut": false}, "hydration": "full", "pin": "inherit", "effective_pin": "pinned", "sync": "in-sync"}},
  {"path": "/home/user/OneDrive/Pictures/2026/old.jpg", "error": {"code": 1002, "message": "no item at /home/user/OneDrive/Pictures/2026/old.jpg"}}
]}}
```

## Errors

Errors are JSON-RPC error objects: `code` (integer), `message` (for people; it may change)
and optional `data`. Programs act on `code` and `data`, never on `message`. The codes are
stable: once released, a code never changes meaning.

| Code | Name | Meaning | `data` |
|---|---|---|---|
| -32700 | `parse-error` | The line was not UTF-8 or not one JSON document | |
| -32600 | `invalid-request` | Not a valid JSON-RPC message; line over 1 MiB; second `hello`; request on a subscribed connection | |
| -32601 | `method-not-found` | No such method in this version | |
| -32602 | `invalid-params` | Parameters malformed, including any relative path | |
| -32603 | `internal` | A daemon fault | |
| 1001 | `not-ready` | `hello` has not completed on this connection, or the daemon is still starting | |
| 1002 | `unknown-path` | Under a mount point, but no such item | |
| 1003 | `not-in-a-mount` | Not under any mount point | |
| 1004 | `refused` | Understood and refused; nothing changed | `reason`, and `sync` for `not-in-sync` |
| 1005 | `needs-sign-in` | The account must sign in again | |
| 1006 | `blocked-by-policy` | Organisation policy refuses access | |
| 1007 | `read-only` | The item or its mount is read-only | |
| 1008 | `offline` | The request needs the server and the daemon is offline | |
| 1009 | `unsupported-version` | `hello` offered no supported version | `supported_versions` |

An unknown method:

<!-- example: request rejected -->
```json
{"jsonrpc": "2.0", "id": 14, "method": "item.delete", "params": {"paths": ["/home/user/OneDrive/a.txt"]}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 14, "error": {"code": -32601, "message": "method \"item.delete\" is not part of this protocol version"}}
```

A request before `hello`:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 15, "error": {"code": 1001, "message": "daemon.status refused: the first request on a connection must be hello"}}
```

A line that is not JSON:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "line 3 is not valid JSON: expected value at line 1 column 1"}}
```

The remaining codes, which also appear as per-path `error` objects inside results:

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 16, "error": {"code": 1005, "message": "account acct-work-1 needs to sign in again"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 17, "error": {"code": 1007, "message": "/home/user/Example/Site - Documents is a read-only library"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 18, "error": {"code": 1008, "message": "cannot confirm the online copy while offline"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 19, "error": {"code": -32603, "message": "internal error while reading the state store"}}
```

<!-- example: error -->
```json
{"jsonrpc": "2.0", "id": 20, "error": {"code": 1004, "message": "dehydrate refused: the file is open", "data": {"reason": "in-use"}}}
```
