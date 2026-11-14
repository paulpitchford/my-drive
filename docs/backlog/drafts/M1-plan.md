# Milestone M1 (Engine): draft plan

- **Status:** draft. Not a set of work packages yet; nothing here is `ready`.
- **Date:** 2026-11-15
- **Written by:** a planning agent that read every ADR, flow, the open questions and the
  public API of each crate as merged after wave 3; reviewed by the lead.
- **Next step:** owner go-ahead for M1 and for the items under "Needs the owner"; then the
  lead fixes the document contradictions listed in section 5, records the architecture
  changes as ADRs, and writes the work packages wave by wave, re-reading each against the
  code as it then stands.

This folder is skipped by the docs cross-reference check, because the package numbers below
do not exist as files yet.

## Lead rulings on the plan's decisions (section 6)

These are the lead's positions. Those marked ADR change the architecture and will be
recorded as ADRs before any package depending on them is written.

| # | Decision | Ruling |
|---|---|---|
| D1 | Backing directory layout | **Accept (ADR).** Content files are keyed by a stable local ID, not by name. Renames and moves become store-only and the conflict split becomes atomic. Amends ADR-0005 and flows F02, F05, F07, F08; to be written together with the G3 amendments (Q34). |
| D2 | How delta is applied | **Accept (ADR).** Every page is staged durably and last occurrences are applied when the delta link arrives. Resolves the contradiction between F02 and F05. |
| D3 | Mount and account state | **Accept.** Store facts, derive state; never write `Blocked` into every item. |
| D4 | Does login create the mount | **Accept.** No. The CLI shows the signed-in account and asks (Q27). F01 and J01 to be amended. |
| D5 | What "download" and "upload" mean in M1 | **Accept, owner to confirm.** `item.hydrate` plus `item.import` / `item.export` behind a capability, so M1 delivers its stated outcome without the filesystem. |
| D6 | Local delete propagation | **Accept.** Deferred to M2 with the mass-delete guard; the schema carries the `delete` kind and `held` flag now. |
| D7 | Uploads to existing items | **Accept.** Always an upload session, where `If-Match` is documented, until a real-account experiment shows the simple upload honours it. Needs the new gate below. |
| D8 | Teams resolution | **Accept.** Out of M1; link and drive ID only. |
| D9 | socket.io notifications | **Defer.** The protocol Graph speaks is undocumented. M1 uses polling. The three notification packages (024, 028, 040) leave M1 and wait for the real-account observation. No websocket dependency is added now. |
| D10 | Migrations | **Accept.** Two migrations; rebuild `items` in the second only if the constraint cannot be added otherwise, verified by a test first. |
| D11 | Engine facade and end-to-end tests | **Accept.** The facade is the daemon's only window and is frozen early; end-to-end tests live in the daemon crate. |
| D12 | Pause and resume | **Accept.** |
| D13 | Folder mounts | **Accept.** M1 mounts drive roots only (Q10). |
| D14 | Account identity | **Accept.** |
| D15 | Persisting the upload URL | **Reject.** An upload URL is a bearer capability and stays out of the state store (in the spirit of ADR-0010 R6). A session interrupted by a crash restarts; that costs a re-upload and loses nothing. |
| D16 | Rule-coverage gate | **Accept.** Fails only for a named list of rules M1 implements. |
| D17 | Wave size | **Accept.** No wave has more than five packages or more than two foundation packages. |
| D18 | Small rulings | **Accept the proposals** for Q7, Q14, a `429` without `Retry-After` (a fixed hold), files with no hash, and CLI exit codes and `--json` errors; Q26 (folder status aggregation) deferred to before M3. |

## Needs the owner

1. **Go-ahead to start M1.**
2. **A new validation gate, G6: write-side behaviour on a real account.** Several unknowns can
   only be answered by writing: whether a simple upload honours `If-Match`; what SharePoint's
   post-upload rewriting looks like; how `#` and `%` are refused; item-ID casing; how a folder
   delete is reported; the notification protocol. This needs a scratch folder in a real
   account and write permission, which the existing experiment package (WP-011) deliberately
   excludes. The owner decides whether and where.
3. **The app registration (Q22)**, without which nothing can sign in to a real account.
4. **Confirm D5**, since it defines what M1 lets a user do.

## Size

31 packages as planned; 28 after D9. On the evidence of waves 2 and 3, foundation packages
take three to five review rounds, so M1 is several times the agent effort of M0. The critical
path runs through the two store migrations, upload, conflicts and recovery.

---


Nothing was written or changed in the repository. Everything below is from reading the docs and the crate sources. Where I am unsure I say so.

## 0. Findings that shape the plan

These are facts about the code as it stands today that the M1 outline in `docs/backlog/README.md` does not account for.

- **The Graph client is read-only.** `GraphClient` has `get_drive`, `get_root`, `get_item`, `delta_from_root`, `delta_from_link` and `download`, and nothing else. There is no `/me`, `/me/drive`, upload, create folder, PATCH, DELETE, `/shares`, Teams or socketIo call. `GraphErrorKind` has no kind for 412, 409, 423 or 507; they all arrive as `UnexpectedStatus`.
- **Nobody can derive an account or tenant.** `RefreshingTokenProvider::signed_in(account: AccountId, ...)` takes the account ID from the caller. `IdToken` is an opaque secret with no claims reader, so F01 step 6 ("tenant from the `tid` claim") has no code behind it.
- **The daemon may not depend on `mydrive-graph` or `mydrive-store`** (overview diagram, enforced by `scripts/check_arch.py`). So sign-in orchestration, the token store and every store read for `item.status` must be reached through `mydrive-engine`. The doc comment in `crates/mydrive-ipc/src/paging.rs` ("the daemon backs `KeysetSource` with store queries") is only true via the engine.
- **The store is one `rusqlite::Connection`** and `Store::transaction` takes `&mut self`. Many mount workers plus IPC status reads need one owner for it. The "read-only reader connection" is deferred in `store-schema.md`.
- **The store cannot page a directory.** `Queries::children` returns everything; there is no child-by-name lookup and no `accounts()` listing. The index is `items_by_parent (drive_id, parent_item_id)` without `name`, although ADR-0020's Consequences say "the store's index on (parent, name) serves this".
- **`mydrive-core` has no `Clock` or `Hydrator`**, though the overview's crate table says it owns them.
- **The protocol catalogue has ten methods** (`methods::method::ALL`): none for login, logout, mount add or remove, pause, or any upload/download-to-a-directory operation. `AccountInfo` has no display name; `MountInfo` has no item count.
- **The simulator has no token endpoint, `/me` or `/me/drive`.** The fake authority lives in `crates/mydrive-graph/tests/auth_support.rs`, which engine and daemon tests cannot reuse.
- **No websocket crate is in `[workspace.dependencies]`.** socket.io needs one on both the simulator and the client side, so it needs your approval and a `cargo deny` pass.
- **Rule coverage is 60 of 160** (I ran `scripts/rule_coverage.py`); `open-questions.md` still says 52.

## 1. Work packages

There are 31 packages, WP-014 to WP-044. "Adversarial" marks the ones other code will trust; plan two or three review rounds for those. Acceptance criteria use the real names I read; names marked *(new)* are proposed by that package.

### Preparation and foundations

**WP-014 `m1-shared-file-prep`** (lead chore, no behaviour)
- Goal: make every shared file ready so no later package touches a manifest, a module list or a router.
- In scope:
  - `Cargo.toml` dependencies for engine, daemon, CLI and ipc (tokio for the server); `base64` and `serde` for the simulator.
  - `mydrive-engine/src/lib.rs` with every module in section 4 declared as a documented stub.
  - A `lib.rs` target for `mydrive-daemon` beside `main.rs`, so tests can run the daemon in-process.
  - Split the simulator so features live in their own files: router composed from per-feature routers, `impl GraphSim` blocks per feature file, README index linking per-feature docs.
  - Split `mydrive-graph/src/client/mod.rs` into declared sub-modules (`write`, `session`, `identity`, `discover`, `notify`) and add every `Operation` variant M1 needs, with its `Display` arm.
- Out of scope: any behaviour; any new error kind (WP-025).
- Owned paths: root `Cargo.toml`, `Cargo.lock`, every crate `Cargo.toml`, the `lib.rs` and `mod.rs` files named above, `crates/mydrive-graph-sim/README.md`.
- Depends on: none. ADRs: 0003, 0004, 0018.
- Acceptance:
  1. `just check` passes, with the stubs compiling.
  2. `scripts/check_arch.py` passes with the new dependencies.
  3. Every existing simulator self-test and client test passes unchanged; the split is a pure move.
  4. No public signature of an existing item changed (diff of `cargo doc` item lists, or reviewer check).
- Open: whether to add `tokio-tungstenite` now (decision D9).

**WP-015 `core-clock-and-conflict-names`** (small, pure)
- Goal: the injected clock every engine timer uses, and the conflict-copy name rule (ADR-0017 puts conflict naming in core).
- In scope:
  - `clock.rs`: `trait Clock` *(new)* with monotonic `now()`, `wall_now()` in Unix seconds, and `sleep_until()` returning a boxed future. `ManualClock` *(new)* whose `advance()` wakes sleepers. No tokio in core.
  - `conflict.rs`: `conflict_copy_name(name, host, context, is_taken)` *(new)*.
- Out of scope: a system clock implementation (WP-029, in the engine, on tokio).
- Owned paths: `crates/mydrive-core/src/clock.rs`, `conflict.rs`; the two `pub mod` lines in `lib.rs` come from WP-014.
- Depends on: WP-014. ADRs: 0012 R7, 0017. Flows: F07.
- Acceptance:
  1. `report.docx` with host `laptop` gives `report-laptop.docx`; `Makefile` gives `Makefile-laptop`; the split is at the last dot.
  2. A taken name gives `-2`, then `-3`; property test: the result is never in the taken set.
  3. Property test: for any host string and any name that passes `validate_name`, the result passes `validate_name` with the same `NameContext` and is at most 255 UTF-8 bytes, cut at a character boundary.
  4. A host made only of `FORBIDDEN_CHARACTERS` gives a fixed fallback word, never an empty segment.
  5. `ManualClock`: a sleeper for T wakes exactly when `advance` reaches T; two sleepers wake in deadline order.
- Open, Q7: proposed answer is criteria 1 to 4. Needs your ruling on the replacement character (`_`) and the fallback word.

**WP-016 `ipc-protocol-m1-methods`** (adversarial: wire contract)
- Goal: add to protocol v1 the methods M1 needs, before v1 is ever released.
- In scope, in `methods.rs`, `values.rs`, `error.rs`, `paging.rs` and `docs/protocol/README.md`:
  - `account.login.start` (mode browser or headless; returns attempt ID and authorise URL), `account.login.finish` (optionally a pasted URL; returns `AccountInfo` and a proposed default mount; creates nothing), `account.logout`.
  - `mount.add.preview` (target is a drive ID, a link or the account's own drive; returns names, item count if known, read-only flag, proposed mount point and a confirmation token), `mount.add.confirm`, `mount.remove`.
  - `sync.pause` and `sync.resume` (decision D12).
  - `item.import` and `item.export`, behind a capability string (decision D5).
  - Additive fields: `AccountInfo.username` and `display_name`; `MountInfo.items` and `drive_type`.
- Out of scope: `activity.*`; any server code.
- Owned paths: `crates/mydrive-ipc/src/{methods,values,error,paging}.rs`, `crates/mydrive-ipc/tests/**`, `docs/protocol/README.md`.
- Depends on: WP-014. ADRs: 0009 (R5, R6, R9), 0019, 0020, 0010 R6. Flows: F01, F09, F11. Journeys: J01, J14, J15.
- Acceptance:
  1. Every new method is in `method::ALL` and parses through `MethodCall::parse`; `Session::admit` refuses each before `hello` with `hello-required`.
  2. Every new method has a catalogue entry with request and response examples that the existing `readme_examples` and `protocol_examples` tests check.
  3. No result type can carry a token, authorisation code or verifier; a test serialises every new result built from secret-bearing inputs and searches the bytes (ADR-0010 R6).
  4. Every new enumerated field is in `VALUE_SETS` and marked open or closed (ADR-0019 R7).
  5. The `tests/bounds.rs` suite covers every new message: none can exceed `MAX_LINE_BYTES` (ADR-0020 R4).
  6. A confirmation token used with a different target is refused with `invalid-params`.
- Open:
  - Q27: proposed answer is that login never creates a mount; the CLI shows the account and asks. This contradicts F01 step 7 and J01 step 4, which you must amend.
  - J14 questions 4 to 6: proposed exit codes are 0 ok, 1 failed, 2 usage, 3 daemon not running, 4 partly done; `--json` errors are a JSON object on standard output plus a non-zero exit; `--json` output is the IPC result object. Product choice, needs you or the owner.
  - J01 question 6: accept either the account ID or a unique username.

**WP-017 `store-schema-v2-mounts-delta`** (adversarial)
- Goal: schema version 2. Design in section 3.
- In scope: account and mount facts, delta run state and the staged-delta table, the listing index and queries, mount removal, `ItemKind` for packages if you accept it.
- Out of scope: pending operations (WP-018).
- Owned paths: `crates/mydrive-store/**`, `docs/architecture/store-schema.md`.
- Depends on: WP-014. ADRs: 0006 (R3, R4), 0007 (R2, R4), 0011 R2, 0015 R8, 0016 R6, 0020 (R1, R2). Flows: F02, F05, F12, F13.
- Acceptance:
  1. A copy of a version 1 database with accounts, mounts, items and ranges opens at `SCHEMA_VERSION` 2 with every row intact and `PRAGMA foreign_key_check` empty.
  2. `children_after(parent, after_name, limit)` *(new)* returns names in byte order strictly after `after_name`, and `EXPLAIN QUERY PLAN` shows the new index. Over 50,000 children, a loop of 256-row pages delivers each exactly once while other children are inserted and deleted between pages (ADR-0020 R1, R2).
  3. `child_by_name` *(new)* is exact and byte-wise.
  4. Staging the same item ID twice in one run keeps only the later row (ADR-0011 R2). `stage_delta_page` *(new)* commits its rows and the next link together; a dropped `Transaction` leaves neither.
  5. `finish_delta_run` *(new)* sets `delta_token`, clears the run columns and deletes the staged rows in one savepoint. `delta_token` is unchanged by anything else in a run.
  6. `remove_mount` *(new)* refuses, naming the items, when any item is dirty, unless told to keep local-only content; it never touches another drive's rows.
  7. The existing 800,000-step model test still passes, extended with the new operations.
- Open: decisions D1 (backing layout decides whether `local_id` or `materialised` is added), D2 (staging) and D3 (facts versus a stored state column).

**WP-018 `store-schema-v3-pending-operations`** (adversarial; the riskiest package in M1)
- Goal: schema version 3, closing Q28. Design in section 3.
- In scope: pending operation records, upload session records, local move as pending, adopt server item, remote-deleted-while-dirty, clearing an unchanged mark, atomic conflict split, retry bookkeeping.
- Owned paths: as WP-017 (so it is sequenced after it).
- Depends on: WP-017. ADRs: 0006, 0008 R7, 0012 (R3, R6, R7, R8, R11). Flows: F04, F05, F07, F12, F13.
- Acceptance:
  1. With a pending move, none of `apply_remote_metadata`, `complete_upload` or `discard_local_changes` changes `parent_item_id` or `name`; the server's location is recorded on the operation instead.
  2. A remote report that places the item where the local move put it drops the move operation (our own echo).
  3. `delete_item` with `PendingChanges::Refuse` refuses an item, or a subtree containing an item, that has any pending operation.
  4. `set_sync` refuses `in-sync` while an operation is pending.
  5. `adopt_server_item` *(new)*: starting from the documented recovery state (a provisional dirty row plus a clean server row), one call leaves one row under the server ID with the provisional row's content, ranges, children and dirty mark, and the server row's children re-parented. It refuses if the server row is dirty.
  6. `detach_from_server` *(new)* on a dirty item gives it a new provisional ID, keeps size, ranges and generation, and clears server identity. Neither `ServerConfirmedLocalDelete` nor `discard_local_changes` is involved.
  7. `clear_unchanged_local(key, expected_generation, verified_hash)` *(new)* refuses on a generation mismatch, on a provisional item, and when the hash equals neither `local_hash` nor (if `local_hash` is `NULL`) `server_hash`. With `remote_changed` set it leaves the item `pending-download`, never `in-sync`.
  8. `split_conflict` *(new)* is one savepoint: afterwards the copy is a provisional dirty item holding the local content and the original is a clean placeholder of the server version. Killed at any statement, the store holds either the before or the after state.
  9. Model-based test: random sequences of every public operation never reach a state where a dirty item's content is unreachable from a dirty row.
- Open:
  - Q28: answered by this package.
  - F04 question 5 (both sides renamed): proposed answer is that the local move wins.
  - Decision D10 (rebuild `items` or not) and decision D15 (store the upload URL or not).

### Simulator

All simulator packages keep the README's rule: document the source of each behaviour, and where it is unverified add a switch whose default is the harder case.

**WP-019 `sim-write-side`**
- Goal: the simulator accepts writes.
- In scope:
  - Simple `PUT .../items/{id}/content` and `PUT .../items/{parent}:/{name}:/content`.
  - `POST .../children` (create folder), `PATCH .../items/{id}` (rename, move), `DELETE .../items/{id}`.
  - `If-Match` with `412`; `@microsoft.graph.conflictBehavior` with `409 nameAlreadyExists`; a quota that gives `507`; `423` by switch.
  - `fileSystemInfo` on upload.
  - SharePoint post-upload rewriting, by switch on library drives: for chosen extensions the stored size and hash differ from what was sent, either in the response or only in a later delta.
  - A crate-internal `commit_upload` function that WP-020 calls.
- Owned paths: `crates/mydrive-graph-sim/src/model.rs`, `src/write.rs`, `tests/write_*.rs`, `docs/write.md`.
- Depends on: WP-014. ADRs: 0017 (R1, R5), 0012.
- Acceptance:
  1. An upload with a stale `If-Match` gets `412` and changes nothing; with the current eTag it succeeds and the eTag and cTag change.
  2. A create with `conflictBehavior=fail` on a name that differs only in case gets `409 nameAlreadyExists`.
  3. Every write appears in a later delta exactly as a control-API change does.
  4. With rewriting on, the quickXorHash served afterwards differs from the hash of the uploaded bytes, and downloads return the rewritten bytes.
  5. `DELETE` with a stale `If-Match` gets `412`; a deleted item then appears in delta with the `deleted` facet.
  6. The request log records `if-match` for every write.
- Open, needing a real account: whether simple `PUT` honours `If-Match` (F04 question 3). Give it a switch, default "not honoured" as the harder case. Whether rename changes `lastModifiedDateTime`.

**WP-020 `sim-upload-sessions`**
- In scope: `createUploadSession` by item ID and by parent and name; fragments with `Content-Range`; `202` with `nextExpectedRanges`; `416`; `GET` session status; `DELETE` session; expiry giving `404`; a switch for whether the upload URL accepts, requires or refuses `Authorization`; fragment-size rule (multiples of 327,680 bytes, refused otherwise); `If-Match` checked at creation and again at commit.
- Owned paths: `crates/mydrive-graph-sim/src/session.rs`, `tests/session_*.rs`, `docs/session.md`.
- Depends on: WP-019.
- Acceptance:
  1. A fragment that is not a multiple of 327,680 bytes (other than the last) is refused.
  2. An out-of-order fragment gets `416`, and `GET` returns the true `nextExpectedRanges`.
  3. A session whose item changed between creation and the last fragment ends in `412`, with the item unchanged.
  4. An expired or deleted session answers `404`.
  5. The final fragment returns the driveItem, and delta shows it once.
  6. In "refuses Authorization" mode, a fragment sent with a bearer token gets `401`.
- Open: what real Graph does with `Authorization` on the upload URL after April 2027 is unknown; keep the switch.

**WP-021 `sim-delta-model-2`**
- In scope:
  - `410 Gone` as a model: `expire_delta_tokens(drive, kind)` *(new)* makes the next delta call with an old token answer `410` with the chosen code and a working `Location` restart link.
  - Ancestor folders in delta when a child changes, with the `deltaExcludeParent` header honoured.
  - A switch for mixed-case drive IDs across fields, and one for item IDs differing in case between `id` and `parentReference.id` (Q23).
  - `package` and `remoteItem` entries in delta.
  - A fault that drops the connection before any response.
- Owned paths: `src/model.rs` (WP-019 is merged by then), `src/delta.rs`, `tests/delta2_*.rs`, `docs/delta.md`.
- Depends on: WP-019.
- Acceptance:
  1. After expiry the restart link enumerates current state and ends in a new delta link.
  2. Changing a file three levels deep puts its three ancestors in the next delta unless `deltaExcludeParent` was sent.
  3. With the mixed-case drive switch on, the existing client still returns only lowercase `DriveId`s (ADR-0007 R1).
  4. A package item appears with the `package` facet, and `DriveItem::from_json` yields `ItemKind::Package`.
  5. The drop fault produces `GraphErrorKind::Transport` in the client.
- Open: Q23 and Q31 need the real-account experiment; the switches make both cases testable meanwhile.

**WP-022 `sim-throttling-model`**
- In scope: tenants as a grouping of drives (reading the registry WP-023 creates); resource units per request (1, 2 or 5, per F10 step 9); a per-tenant window limit on the simulator's own clock; `429` with `Retry-After` when over; throttled requests count against the limit; optional `RateLimit-*` headers so ADR-0011 R7 can be tested.
- Owned paths: `src/throttle.rs`, `tests/throttle_*.rs`, `docs/throttle.md`.
- Depends on: WP-014, WP-023.
- Acceptance:
  1. N+1 requests in a window give exactly one `429` with the configured `Retry-After`.
  2. `advance_clock` past the window restores service with no real sleep.
  3. Two tenants have independent budgets.
  4. With `RateLimit-*` headers switched on, the header values are present.
  5. Same script gives the same result on every run (ADR-0017 R5).
- Open: none blocking. The real limits for personal accounts are unpublished.

**WP-023 `sim-identity-and-discovery`**
- In scope:
  - A fake authority: authorise step and token endpoint with PKCE check, refresh with rotation, `invalid_grant` on revoke, ID tokens with `tid` and `oid`.
  - `/me`, `/me/drive`, `/organization`.
  - `/shares/{encoded}/driveItem`, recording which `Prefer` value was sent and granting durable access only for `redeemSharingLink`.
  - `/me/joinedTeams`, team channels and `filesFolder`.
  - Read-only library signals and blocked libraries, both marked provisional.
- Owned paths: `src/identity.rs`, `src/authority.rs`, `src/discovery.rs`, `tests/identity_*.rs`, `tests/discovery_*.rs`, `docs/identity.md`.
- Depends on: WP-014. ADRs: 0010, 0015 (R2, R3, R4), 0017.
- Acceptance:
  1. A code redeemed with the wrong verifier is refused.
  2. Refresh returns a new refresh token; a revoked one gets `invalid_grant`.
  3. A link resolved with `redeemSharingLinkIfNecessary` does not make the drive reachable by a later `GET /drives/{id}`; `redeemSharingLink` does.
  4. `filesFolder` for a "migrated private channel" returns a drive root in another drive.
  5. A blocked library answers `403` on every route with the configured code, while another drive of the same tenant works.
  6. The existing `LoopbackSignIn` and `PasteSignIn` tests can run against this authority (prove with one test each).
- Open: Q11, Q12 and Q13 (gate G2) need a work tenant. The error codes and read-only signals here are guesses and must say so.

**WP-024 `sim-socketio`**
- In scope: `GET /drives/{id}/root/subscriptions/socketIo` returning a notification URL on a third origin; a socket.io endpoint over websocket only; a notification event per change; switches to drop the connection and to refuse an expired URL.
- Owned paths: `src/notify.rs`, `tests/notify_*.rs`, `docs/notify.md`.
- Depends on: WP-014 and the dependency approval. ADRs: 0011 R9.
- Acceptance:
  1. A connected client receives one event per control-API change to that drive and none for other drives.
  2. The drop switch closes the socket.
  3. An expired URL is refused, and a new subscription call yields a working one.
- Open: the engine.io and socket.io protocol versions Graph speaks, and the event payload, are undocumented (F05 question 4). Everything here is a guess until a real account is observed. This is the most speculative package in M1.

### Graph client

**WP-025 `graph-write-side`** (adversarial: error kinds are an API contract)
- Goal: typed write calls, and the error kinds the engine needs to react differently to.
- In scope:
  - New `GraphErrorKind` variants *(new)*: `PreconditionFailed`, `NameConflict`, `Locked`, `InsufficientStorage`.
  - `upload_small_replace(drive, item, if_match, bytes)`, `upload_small_create(drive, parent, name, bytes)` with `conflictBehavior=fail`, `create_folder`, `update_item` (rename, move, `If-Match`), `delete_item` (`If-Match`) *(all new)*.
  - `fileSystemInfo` with whole-second times.
- Out of scope: sessions (WP-026).
- Owned paths: `crates/mydrive-graph/src/client/write.rs`, `client/error.rs`, `tests/client_write*.rs`.
- Depends on: WP-014, WP-019. ADRs: 0011 (R5, R8), 0012 (R6, R7). Flows: F04, F07, F12.
- Acceptance:
  1. `412` maps to `PreconditionFailed` and `409 nameAlreadyExists` to `NameConflict`. Any other `409` stays `UnexpectedStatus`; a test proves the two are distinct.
  2. No replace or delete call can be made without an eTag: `if_match` is a required non-optional parameter.
  3. No call sends `conflictBehavior=replace`; a test greps the request log.
  4. `Throttled`, the single-refresh `401` rule and the User-Agent hold for every new call, exactly as the existing tests show for reads.
  5. A response item parses through `DriveItem::from_json`, and a response with no hash yields `quick_xor_hash() == None`, never a default.
  6. A raw-socket test covers a write whose response is cut off: the result is an error that says "outcome unknown", distinct from "refused".
- Open: F04 question 3 again. Decision D7 makes the engine independent of the answer.

**WP-026 `graph-upload-sessions`**
- In scope: `create_upload_session` (by ID with `If-Match`; by parent and name with fail), `put_fragment`, `session_status`, `cancel_session` *(new)*; an `UploadSession` type whose `Debug` redacts the URL; the per-session decision on sending `Authorization` (ADR-0011).
- Owned paths: `client/session.rs`, `tests/client_session*.rs`.
- Depends on: WP-020, WP-025. ADRs: 0011 R12, 0012 (R5, R6).
- Acceptance:
  1. A fragment length that is not a multiple of 327,680 bytes, or is 60 MiB or more, is refused before any request (ADR-0011 R12).
  2. The bearer token is never sent to an upload URL on another origin.
  3. After a `401` from an upload URL in "requires Authorization" mode, the session retries with the token once and remembers the choice.
  4. `416` is followed by `session_status`, and the caller gets the true next offset.
  5. `404` maps to a distinct "session gone" kind.
  6. The upload URL appears in no `Debug`, `Display` or log output (reuse the no-secrets test pattern).

**WP-027 `graph-identity-and-discovery`**
- In scope:
  - `auth/claims.rs`: read `tid`, `oid` and `preferred_username` from an `IdToken` without verifying the signature (it came over TLS from the token endpoint). Nothing from it is logged.
  - `me()`, `my_drive()`, `organization()`.
  - `resolve_share(link, Redeem::IfNecessary | Redeem::Durable)`, with the `u!` base64url encoding as a pure function.
  - `joined_teams`, `channels`, `files_folder`.
  - `drive_details` (site and library names for the mount point).
- Owned paths: `crates/mydrive-graph/src/auth/claims.rs`, `client/identity.rs`, `client/discover.rs`, `model/identity.rs`, `tests/client_identity*.rs`, `tests/client_discover*.rs`.
- Depends on: WP-014, WP-023. ADRs: 0010 (R5, R6), 0011 R13, 0015 (R2, R3).
- Acceptance:
  1. The share encoding matches the documented example: unpadded, `/` to `_`, `+` to `-`, `u!` prefix. Property test: it decodes back.
  2. `Redeem::IfNecessary` sends exactly `Prefer: redeemSharingLinkIfNecessary`, asserted from the simulator's log (ADR-0015 R2).
  3. A malformed ID token gives a typed error and no panic; a claims `Debug` shows no token text.
  4. No code path calls `sharedWithMe` (ADR-0011 R13).
  5. A drive ID in mixed case in any of these responses leaves the client lowercase.
- Open:
  - Q11, the Teams scope: needs a work account. My understanding, unverified, is `Team.ReadBasic.All` and `Channel.ReadBasic.All`.
  - F01 question 2, the organisation name: I believe `GET /organization` works with `User.Read`; unverified. Fallback is the domain part of the username.
  - Decision D8: I recommend not building the Teams calls in M1 at all.

**WP-028 `graph-change-notifications`**
- In scope: `socket_io_endpoint(drive)`; a `ChangeSocket` that connects with the websocket transport and yields "something changed" and "closed"; no reconnect policy (the engine owns that).
- Owned paths: `client/notify.rs`, `tests/client_notify*.rs`.
- Depends on: WP-024. ADRs: 0011 R9.
- Acceptance:
  1. An event from the simulator yields one wake-up.
  2. A dropped socket yields `closed`, not an error loop.
  3. The notification URL is never logged.
  4. The bearer token is not sent to the notification origin.
- Open: as WP-024.

### Engine

**WP-029 `engine-foundations`** (adversarial for the facade)
- Goal: the pieces every engine module stands on, and the frozen facade the daemon is written against.
- In scope:
  - `db.rs`: `Db` *(new)*, the one owner of the `Store` on its own thread; `db.call(|store| ...)` runs a closure and returns its result. A panic in a closure is contained and reported.
  - `backing.rs`: `Backing` *(new)* over a mount's backing directory: create sparse, write at offset, sync, read, snapshot (reflink, else copy), hash through `mydrive_core::hash::hash_reader`, remove, list orphans.
  - `error.rs`, `events.rs` (a bounded broadcast of `EngineEvent`), `config.rs`, a tokio `SystemClock`.
  - `api.rs`: the `Engine` facade with **every method M1 will have, as real signatures** with stub bodies returning a typed "not available yet" error. Its view types are built from core types only, so the daemon never names a store or graph type.
- Out of scope: any sync behaviour.
- Owned paths: `crates/mydrive-engine/src/{db,backing,error,events,config,api,clock}.rs`, `tests/foundations_*.rs`.
- Depends on: WP-014, WP-015, WP-017. ADRs: 0004, 0005 R2, 0006 R1, 0007 R5.
- Acceptance:
  1. Two tasks calling `db.call` concurrently never interleave inside a `Transaction`; a 1,000-task test checks a counter invariant.
  2. A closure that returns `StoreError::TransactionAborted` leaves the `Db` usable.
  3. `Backing` refuses any path that escapes `mounts/<mount_id>/`; property test over hostile names.
  4. The mount ID is part of every backing path (ADR-0007 R5).
  5. A snapshot is byte-identical to the file at the moment it was taken, even if the file is written afterwards.
  6. A write then sync then simulated kill leaves the bytes readable.
  7. The public API of `mydrive-engine` mentions no type from `mydrive-store` or `mydrive-graph` except those deliberately re-exported and listed in the package.
- Open: decision D1 (layout) must be made first. Decision D11 (facade as the daemon's only window).

**WP-030 `engine-request-scheduler`** (adversarial)
- In scope: `scheduler/queue.rs`, a pure state machine *(new)* with `submit`, `next(now)` returning dispatch or wait-until, and `on_result`; seven classes from F10; round-robin by mount within a class; 8 in flight, at most 2 of classes 6 and 7; a tenant hold on `GraphErrorKind::Throttled`. `scheduler/mod.rs`, the async handle that runs a re-runnable request closure under a permit, driven by `Clock`.
- Owned paths: `crates/mydrive-engine/src/scheduler/**`, `tests/scheduler_*.rs`.
- Depends on: WP-014, WP-015. ADRs: 0011 (R6, R7), 0007 R3, 0005 R4. Flows: F10.
- Acceptance:
  1. Pure test: with a hold until T, nothing is dispatched before T for that tenant, and another tenant's queue is untouched (ADR-0011 R6).
  2. A `Throttled { retry_after_raw: Some(http_date) }` is interpreted against `Clock::wall_now`, capped at `MAX_RETRY_AFTER`.
  3. A throttled request is requeued, never dropped; property test: every submitted request eventually completes or is cancelled by its caller.
  4. A class 1 request submitted during a hold longer than its deadline fails at once with a "throttled" error instead of waiting (F10 step 5).
  5. With one mount holding 10,000 queued class 6 requests and another submitting one, the second is dispatched within one round (Q9).
  6. No more than 2 background requests are in flight even with nothing else queued.
  7. Against the simulator with a scripted `Fault::status(429).retry_after(30)`, zero requests reach it for that tenant until `ManualClock` passes 30 s, asserted from `GraphSim::requests()`.
- Simulator needs: scripted faults exist today. The WP-022 model is used later, in WP-044.
- Open:
  - Q9: proposed answer is round-robin by mount within a class, strict order between classes.
  - F10 question 1, `429` with no `Retry-After`: proposed fixed 10 s hold, not a computed back-off. Needs your ruling against ADR-0011.
  - F10 question 4: `ServerError` and `Transport` are retried by the caller with a capped delay and never hold the tenant.
  - F10 questions 2 and 3: accept the proposals.

**WP-031 `engine-delta-apply`** (adversarial)
- Goal: mirror a drive's metadata into the store, first time and incrementally.
- In scope:
  - `delta/convert.rs`: `DriveItem` to `RemoteMetadata`, pure.
  - `delta/plan.rs`: pure `plan(existing, pending, staged_entry, context) -> Vec<StoreAction>`.
  - `delta/run.rs`: page loop through the scheduler into the staged table, then apply parents first and deletes last, then `finish_delta_run`.
  - Hook calls into `conflict::` and `delta::resync::`, which are stubs with safe bodies until WP-036 and WP-037.
- Out of scope: `410` handling beyond calling the stub; backing-file changes for hydrated content beyond clearing ranges.
- Owned paths: `crates/mydrive-engine/src/delta/{convert,plan,run}.rs`, `tests/delta_*.rs`.
- Depends on: WP-017, WP-029, WP-030. ADRs: 0006 R4, 0007 (R1, R2), 0011 (R1, R2, R4, R5), 0012 (R8, R11), 0008 R7. Flows: F02, F05.
- Acceptance:
  1. A 14,000-item simulated drive ends with every item in the store, `delta_token` set, and zero content requests in `GraphSim::requests()` (F02 invariant).
  2. With `delta_repeats = EveryChange` and a delete-then-restore of one item inside one page set, the final state is "exists", and the item's hydrated ranges were never cleared (ADR-0011 R2).
  3. Kill after any page: restart resumes from the saved next link and reaches the same final store as an uninterrupted run. If the saved link is refused, enumeration restarts and still converges.
  4. A `DeltaEntry::Problem` never becomes a guess: the item is refetched with `get_item`; if that fails the item gets `last_error` and the run still completes.
  5. A delete for a dirty item or an item with a pending operation is not applied (the stub leaves it flagged). `delete_item` is only ever called with `PendingChanges::Refuse`.
  6. A folder delete under `folder_deletes = FolderOnly` removes the clean subtree from the store's own tree, and keeps any dirty descendant and its ancestor chain.
  7. A page fetched before a `complete_upload` committed is not applied to that item afterwards; the item is refetched (the rule in `store-schema.md`). Tested with an injected ordering.
  8. A report whose hash equals the stored `server_hash` after our upload leaves the item `in-sync` with no range cleared (ADR-0012 R8).
- Simulator needs: all present today.
- Open:
  - Q10: proposed answer is that M1 mounts drive roots only.
  - Q23: needs the experiment. Proposed interim: an unknown parent whose ID matches a stored one except for case puts the mount in error with a distinct reason; never guess.
  - Q31: proposed handling is criterion 6, plus treating a `404` on a later item fetch as an authoritative delete. Whether real Graph behaves this way needs a real account.
  - F02 question 5: proposed, packages are stored as an opaque kind that is never hydrated or written; shortcut stubs are skipped and counted. I am unsure whether a package's children appear in delta.
  - F05 question 1: remote mass deletes are not guarded in M1.

**WP-032 `engine-accounts-sign-in`**
- In scope: `accounts/`: the registry of accounts with their `RefreshingTokenProvider`, `GraphClient` and tenant scheduler; sign-in attempts wrapping `LoopbackSignIn` and `PasteSignIn`; identity from claims and `/me` and `/me/drive`; `upsert_account`; resume on start with `RefreshingTokenProvider::resume`; sign-out (`TokenStore::delete`, `close`); needs-sign-in detection from `GraphErrorKind::Token(e)` where `e.is_needs_sign_in()`.
- Owned paths: `crates/mydrive-engine/src/accounts/**`, `tests/accounts_*.rs`.
- Depends on: WP-027, WP-029, WP-030. ADRs: 0010 (R1 to R10), 0007. Flows: F01. Journeys: J01, J14, J15.
- Acceptance:
  1. A completed sign-in against the WP-023 authority yields one account row with the right kind, and one entry in a `MemoryTokenStore`.
  2. A `TokenStore` whose `save` fails yields no account row and no provider (ADR-0010 R7).
  3. Signing in twice as the same identity replaces the tokens and creates no second account.
  4. After the simulator revokes the refresh token, the next request marks that account needs-sign-in, every later request for it fails without reaching the simulator, and a second account keeps working (ADR-0010 R9).
  5. Two work accounts in one tenant share one scheduler; a personal account has its own (ADR-0011 R6).
  6. No token, code or verifier appears in any `EngineEvent`, error or log line (planted-secret test, as in `auth_no_secrets.rs`).
  7. An attempt expires after `ATTEMPT_LIFETIME` on `ManualClock`.
- Open:
  - Decision D14: account ID and kind source.
  - F01 question 1: use the native-client redirect for headless, and fix J14's text.
  - Q14: proposed, a numeric suffix on collision, with `--at` to override. Needs your ruling.
  - Q2 (gate G5): device code is not offered; nothing here depends on it.

**WP-033 `engine-mount-manager`**
- In scope: `mount/manager.rs` (add by drive ID or own drive, remove, pause, resume, list with derived state); `mount/check.rs`, pure mount-point checks (exists and empty, not nested in another mount, not under the data directory); `mount/worker.rs`, the per-mount task with a pure `MountMachine` (events in, actions out) and a supervisor that restarts a panicked worker; a polling wake source behind `trait WakeSource` *(new)*; the 300,000-item warning.
- Owned paths: `crates/mydrive-engine/src/mount/**`, `tests/mount_*.rs`.
- Depends on: WP-031, WP-032. ADRs: 0007 (R2 to R6), 0005 R2, 0015 (R1, R7, R8). Flows: F02, F12 (removal only), F13.
- Acceptance:
  1. Adding a mount whose root equals an existing one fails, naming the existing mount (`StoreError::MountRootTaken` surfaced; ADR-0007 R4).
  2. A mount point inside another mount, non-empty, or under `$XDG_DATA_HOME/mydrive/mounts/` is refused before anything is created (ADR-0005 R2).
  3. A new mount reports `resyncing` until its first run finishes, then `active`.
  4. A worker made to panic is restarted; another mount's delta run in the same tenant completes meanwhile (ADR-0007 R3).
  5. `remove` issues no request with method `DELETE`, `PATCH` or `POST` (from the request log), and every `full` file ends up in the mount point directory at its path (ADR-0015 R8).
  6. `remove` with a dirty item is refused, naming it.
  7. While paused, no class 4 to 7 request is submitted; resume runs a delta first.
  8. Polling fires at 5 minutes on `ManualClock` and not before.
- Open:
  - F13 question 2 is already settled by the overview ("First enumeration").
  - F13 question 3 and decision D3: do not write `Blocked` into every item.
  - F12 question 2: proposed as criterion 5.
  - F05 question 2: accept 5 and 30 minutes, configurable.
  - J01 question 2: refuse a non-empty default and say to use `--at`.
  - J01 question 5: progress is the staged item count; there is no total to report.

**WP-034 `engine-download`**
- In scope: `transfer/download.rs`: fetch a range or the whole file through the scheduler; write to the backing file; sync; then `record_hydrated_range`; on completion verify the whole-file quickXorHash against `server_hash` and `set_hydration(Full)`. Handles every `Download` and error variant. Exposed as `Engine::hydrate` and as a range call shaped for the M2 `Hydrator`.
- Owned paths: `crates/mydrive-engine/src/transfer/download.rs`, `tests/download_*.rs`.
- Depends on: WP-029, WP-030. ADRs: 0005 (R4, R5, R6), 0011 (R10, R11), 0006 R6, 0008 R7. Flows: F03 (engine half), F13.
- Acceptance:
  1. With `range = Ignored`, a ranged request that returns `Download::WholeFile` is written from offset 0 and recorded as `[0, size)` (ADR-0011 R11).
  2. Ranges are recorded only after the bytes are synced: a kill between write and record leaves the range unrecorded, never recorded but absent (F13 invariant).
  3. `Truncated` keeps the bytes received; `SizeMismatch` discards every byte of that download, clears nothing else, and refetches metadata.
  4. A whole-file hash mismatch clears all ranges and leaves hydration `placeholder` with reason `hash-mismatch`; the file is never `full` (Q32).
  5. No download is attempted for a dirty item; the call is refused before any request.
  6. The conversion from graph `ByteRange` (start and length, inclusive end) to store `ByteRange` (half-open) is tested at both ends of a file; property test: union of recorded ranges equals bytes written.
  7. No progress for 30 s on `ManualClock` fails the call (ADR-0005 R4).
  8. An item whose `server_hash` is `None` is not marked verified by default; it follows the documented rule from the open question below.
- Open:
  - Q1 (gate G1): does not block. Both outcomes are handled; the M1 CLI downloads whole files.
  - Hash-less files: proposed, mark `full` only if the size matches and the response head was verified, and record that no hash check was possible. Needs your ruling.

**WP-035 `engine-upload`** (adversarial)
- Goal: the write path from a recorded local change to `complete_upload`.
- In scope:
  - `local/`: the local-change API the M2 VFS will call: record a create, mark before a write, close, rename. M1's `import` is built on it.
  - `transfer/upload.rs`: read the generation, snapshot, hash, unchanged check, name validation, choose request, upload, map the outcome to the store.
  - Quiet period on `Clock`.
  - Session resume from stored state.
- Out of scope: conflict resolution (calls the WP-036 stub); delete propagation (decision D6).
- Owned paths: `crates/mydrive-engine/src/local/**`, `src/transfer/upload.rs`, `tests/upload_*.rs`.
- Depends on: WP-018, WP-025, WP-026, WP-029, WP-030. ADRs: 0012 (R3 to R6, R8, R11), 0011 (R5, R12), 0015 R6. Flows: F04.
- Acceptance:
  1. The mark (`mark_local_change` or a recorded create) is committed before the backing file changes: a kill injected between the two leaves a dirty item with the old bytes, never a clean item with new bytes (ADR-0012 R3).
  2. The generation is read before the snapshot: a write injected during the upload gives `UploadCompleted::LocalChangedSince` and a second upload, never `Synced`.
  3. Every replace request carries `If-Match`, asserted from the request log (ADR-0012 R6).
  4. A file over 4 MiB uses a session (ADR-0012 R5), with fragments that satisfy ADR-0011 R12.
  5. A name that fails `validate_name`, a path that fails `validate_server_path`, or a sibling case collision from `find_case_collisions` causes zero requests and leaves the item `error` with reason `name-not-allowed` (ADR-0015 R6).
  6. Unchanged content results in no request and a clean item through `clear_unchanged_local`.
  7. With simulator rewriting on, the item ends `in-sync` with `local_hash != server_hash`, and the next delta causes no download (ADR-0012 R8).
  8. Kill mid-session and restart: the upload resumes from `nextExpectedRanges`; an expired session restarts from zero.
  9. `UploadedButRemoteMovedOn` triggers `get_item` and `apply_remote_metadata`.
  10. `507`, `423` and `403` each leave the content in place and the item dirty.
- Open:
  - Decision D7 and F04 question 3.
  - Q24: proposed, default allowed; on a refusal for a name containing `#` or `%`, set the per-account flag and mark the item `name-not-allowed`. The refusal's shape is unknown and needs a work tenant.
  - D5 in `open-questions.md` (4 MiB): keep.
  - F04 questions 1 and 4 are M2. Question 2 is accepted.

**WP-036 `engine-conflicts`** (adversarial)
- In scope: `conflict/decide.rs`, pure: given the item, the last remote identity, the fresh server metadata and the local hash, return same-content, metadata-only, split, remote-deleted or folder-merge. `conflict/apply.rs`: carry it out through `split_conflict`, `adopt_server_item` and `detach_from_server`. Triggered from `412`, from `NameConflict` on create, and from delta.
- Owned paths: `crates/mydrive-engine/src/conflict/**`, `tests/conflict_*.rs`.
- Depends on: WP-018, WP-031, WP-035. ADRs: 0012 R7, 0011 (R3, R4), 0008 R7. Flows: F07, F05, F12 (table rows only). Journeys: J10.
- Acceptance:
  1. Both sides changed: afterwards the server version is under the original name and the local bytes are in `<name>-<host>.<ext>` as a provisional item that then uploads; a hash of the copy equals the hash of the local bytes before (ADR-0012 R7).
  2. Identical content on both sides produces no copy and a clean item.
  3. A remote rename only (metadata-only) produces no copy; the upload is retried and succeeds.
  4. Remote delete of a dirty file: the bytes survive as a new item uploaded at the same path.
  5. `409` on a folder create: the local folder adopts the server ID and both sets of children are under it.
  6. Kill at each step of each case and restart: the user's bytes are in exactly one dirty item.
  7. Decision table test: every row of F07's "Other both-sides cases" that M1 supports has a test naming it.
  8. With simulator rewriting on, a lost upload response yields at worst one extra copy and never a loss (F07 step 3).
- Open:
  - Q7: `Conflict` is set durably only when the split cannot complete (disk full, read-only mount); with an atomic split it is otherwise never observable. Needs your ruling.
  - F07 question 1: copy the explicit pin, as the flow proposes.

**WP-037 `engine-resync`**
- In scope: `delta/resync.rs`: on `GraphErrorKind::ResyncRequired`, run a full enumeration from `restart` (or the root) into the staged table; reconcile with an anti-join for items the server no longer has; a handler each for `Resync::ApplyDifferences` and `Resync::UploadDifferences`; `Resync::Unknown` treated as the latter.
- Owned paths: `crates/mydrive-engine/src/delta/resync.rs`, `tests/resync_*.rs`.
- Depends on: WP-021, WP-031, WP-033. ADRs: 0011 R3, 0007 R3. Flows: F05 steps 15 to 17.
- Acceptance:
  1. After `expire_delta_tokens`, the mount reports `resyncing`, then `active` with a new token, and another mount's requests are not delayed (ADR-0007 R3).
  2. An item absent from the new enumeration and clean is removed; one that is dirty is kept and re-created remotely.
  3. A clean, fully local file whose hash matches neither stored hash, with different server content, ends as two items (ADR-0011 R3).
  4. Both variants have a test naming the rule; `Unknown` discards nothing.
  5. Kill during enumeration or reconciliation: restart completes with the same result.
  6. `delta_token` is unchanged until reconciliation finishes.

**WP-038 `engine-recovery`**
- In scope: `recovery.rs`: a pure `plan_recovery(snapshot) -> Vec<RecoveryAction>` and its executor, run per mount before the worker starts: `uploading` back to `pending-upload` with `set_sync`; every item from `dirty_items()` re-queued; `pending-download` items refetched; unfinished delta runs resumed; orphan snapshots removed; an unchanged-content check where a mark was committed but nothing was written.
- Owned paths: `crates/mydrive-engine/src/recovery.rs`, `tests/recovery_*.rs`.
- Depends on: WP-035, WP-036, WP-037. ADRs: 0006 R8, 0012 R11, 0017 R6. Flows: F08 steps 10 to 14 (the parts that exist without FUSE).
- Acceptance:
  1. Random-kill test: for 1,000 seeded kill points across a scripted session, after restart every change made before the kill is on the simulated server or in `dirty_items()` (ADR-0017 R6).
  2. A dirty item in a blocked mount is still listed and kept.
  3. A missing backing file for a clean item causes no remote request other than reads; for a dirty item it gives `error` with reason `local-change-lost` and no remote delete.
  4. An orphan snapshot with no matching record is removed; one with a record is kept.
  5. Recovery finishes before the mount's first delta request (from the request log).

**WP-039 `engine-libraries`**
- In scope: `library/`: preview from a link (`Redeem::IfNecessary`) or drive ID; checks (document library, not already mounted, item count); confirm (`Redeem::Durable`) then mount through WP-033; `policy.rs`, one pure function classifying `GraphErrorKind::Forbidden { codes }` as policy block, read-only or access lost; mount-level blocked and read-only state; an hourly re-probe on `Clock`.
- Out of scope: Teams (decision D8); site search.
- Owned paths: `crates/mydrive-engine/src/library/**`, `src/policy.rs`, `tests/library_*.rs`.
- Depends on: WP-027, WP-033. Gates: G2 (provisional). ADRs: 0015 (R1, R2, R4, R7), 0007 (R3, R4). Flows: F09. Journeys: J11, J12, J13.
- Acceptance:
  1. A preview that is declined leaves no `redeemSharingLink` in the request log (ADR-0015 R2).
  2. A link to a folder in a personal drive is refused with the "shared folders" message; nothing is created.
  3. A blocked library puts only that mount in `blocked`; a delta on a second mount of the same account completes (ADR-0015 R4).
  4. In a blocked mount, `dirty_items()` still lists pending content and nothing is discarded.
  5. An upload refused as read-only marks the mount read-only and keeps the item dirty with reason `read-only`.
  6. The re-probe fires hourly on `ManualClock`; when access returns the mount is `active` and a delta runs.
  7. The classification table is one function with a test per row, each row marked provisional.
- Open: Q12, Q13 (gate G2), F09 questions 1 to 3. All need a work tenant; the classification stays provisional. Proposed for M1: reactive read-only detection only (criterion 5). I do not know a Graph v1.0 call that reports required check-out or validation settings.

**WP-040 `engine-change-wakeups`**
- In scope: `notify/`: a socket-backed `WakeSource` using `ChangeSocket`; 1 s coalescing; reconnect with back-off on `Clock`; fall back to the WP-033 poll while down; a 30-minute safety poll while up.
- Owned paths: `crates/mydrive-engine/src/notify/**`, `tests/notify_*.rs`.
- Depends on: WP-028, WP-033. ADRs: 0011 R9. Flows: F05 steps 1 to 3.
- Acceptance:
  1. Ten events within 1 s cause one delta call.
  2. An event changes no store row by itself (ADR-0011 R9); only the delta that follows does.
  3. With the socket dropped, a remote change still arrives within the polling interval.
  4. A second mount's socket failing does not affect the first.
- Open: F05 question 4. This package can slip to M2 without affecting the M1 outcome.

### IPC, daemon, CLI, acceptance

**WP-041 `ipc-server-and-client`** (adversarial: it parses untrusted local input)
- In scope: `server.rs`: a tokio `UnixListener` server generic over `trait Handler` *(new)*; one `Session` per connection; `LineDecoder` framing; a subscribed connection with a bounded queue that is closed when it falls behind (ADR-0019); directory `0700` and socket `0600`. `client.rs`: a blocking client over `std::os::unix::net::UnixStream` with `hello`, call, cursor loop and timeouts.
- Owned paths: `crates/mydrive-ipc/src/{server,client}.rs`, `tests/server_*.rs`, `tests/client_*.rs`.
- Depends on: WP-016. ADRs: 0009 (R1 to R5), 0019 (R2, R5), 0020 (R4, R5).
- Acceptance:
  1. The socket directory is `0700` and the socket `0600`; no TCP or UDP listener is opened (ADR-0009 R1).
  2. A request before `hello` gets `hello-required` and the handler is never called (ADR-0009 R3).
  3. A line over `MAX_LINE_BYTES` gets the framing error and the connection survives or closes as `FrameError::to_rpc_error` documents.
  4. A raw-socket test sends split, merged and partial lines, and bytes that are not UTF-8; no panic, no unbounded buffer.
  5. A subscriber that stops reading is closed once its queue is full; other connections are unaffected.
  6. The client ignores unknown result fields (ADR-0009 R5) and completes a 50,000-entry cursor loop against a fake handler.
  7. A second server on the same socket path fails without removing the first one's socket (ADR-0004 R5).

**WP-042 `daemon`**
- In scope: `mydrived`: XDG paths, `config.toml` (client ID, ADR-0010 R4), a single-instance lock taken before the store is opened, logging, signal handling and clean shutdown; a `Handler` that maps every method to the `Engine` facade, backs `KeysetSource` with the engine's listing, converts state to `ItemInfo::new`, and turns `EngineEvent`s into `status_changed_notifications`.
- Owned paths: `crates/mydrive-daemon/**`.
- Depends on: WP-041, WP-032, WP-033, WP-034, WP-035. ADRs: 0004 (R1, R5), 0009 (R1, R8), 0010 (R4, R7), 0008 (R2, R3), 0019, 0020.
- Acceptance:
  1. A second `mydrived` exits with a clear error and the store file's bytes are unchanged (ADR-0004 R5).
  2. `item.status` on a directory answers from the store alone: no file under the mount point or backing directory is opened (traced), and no Graph request is made (F11 invariant).
  3. A backing-directory path is rejected (ADR-0009 R8); a path outside every mount returns `not-in-a-mount`.
  4. Display status comes only from `mydrive_core::state::display_status`; no status string literal appears in the daemon outside tests (ADR-0008 R3).
  5. While the engine is recovering, requests get `not-ready` (ADR-0019 R5).
  6. With no Secret Service, `account.login.start` fails with an error naming `mydrive doctor` and nothing is persisted (ADR-0010 R7).
  7. In-process test with the simulator: login, preview, confirm, list, hydrate, import, all through the socket.
- Open: Q26: proposed, a folder reports by its own axes in M1 and aggregation is ruled on before M3. Needs your ruling and an ADR-0008 note.

**WP-043 `cli`**
- In scope: `mydrive` with clap: `login [--headless]`, `logout`, `status [path]`, `ls`, `mount list|add|remove`, `hydrate`, `download --to`, `upload`, `pause`, `resume`, `doctor` (keyring and daemon checks only); `--json` on every command; opens the browser with `xdg-open`; asks for confirmation before creating a mount.
- Owned paths: `crates/mydrive-cli/**`.
- Depends on: WP-016, WP-041. ADRs: 0004 (R1, R2), 0009 R5, 0010 (R3, R10). Journeys: J14 (AC1, AC3 to AC7 where no FUSE is needed), J01 (AC2).
- Acceptance:
  1. A test enumerates `method::ALL` and fails if a user-facing method has no command (ADR-0004 R2).
  2. `login --headless` with a fake `xdg-open` on `PATH` never invokes it (J14-AC1).
  3. After login the CLI prints the account and creates no mount unless the user confirms or passes `--yes` (Q27).
  4. With the daemon stopped, every command exits with the "daemon not running" code and says how to start it.
  5. `--json` output of each command parses, and state values equal the wire forms (J14-AC7).
  6. The CLI opens no file under `$XDG_STATE_HOME/mydrive` or `$XDG_DATA_HOME/mydrive` (ADR-0004 R1; traced or reviewed).
  7. Every command is tested against a scripted `Handler` from `mydrive-ipc`, including a response with extra fields.

**WP-044 `m1-acceptance`**
- In scope: the M1 outcome as one in-process scenario per account type, against the full simulator including the throttling model; a `just` recipe that runs the two binaries against a simulator for a manual smoke test; the rule-coverage check made failing for the rules M1 owns.
- Owned paths: `crates/mydrive-daemon/tests/m1_*.rs`, `justfile`, `scripts/rule_coverage.py` and its test, `docs/backlog/README.md` status lines.
- Depends on: all others.
- Acceptance:
  1. Personal drive: sign in, mount, enumerate, list a 6,000-entry folder through cursors, download a file and compare bytes, upload a new file and an edit, see both in the simulator.
  2. Library by link with rewriting on: the same, plus an Office file that ends `in-sync` with two different hashes.
  3. Under the throttling model with a tight budget, the scenario completes and no request is sent during a hold.
  4. Kill and restart mid-scenario: it completes.
  5. `just check` fails if an ADR rule on the M1 list has no test naming it.
- Open: decision D11 (where end-to-end tests live) and decision D16 (which rules are "M1 rules").

## 2. Wave plan

| Wave | Packages | Shared files that must be ready first |
|---|---|---|
| 5 | WP-014 | none; this is the preparation |
| 6 | WP-015, 016, 017, 019, 023 | everything from WP-014 |
| 7 | WP-018, 020, 021, 022, 025, 027, 029, 030, 041 | `store-schema.md` and `ops.rs` free (WP-017 merged); simulator `model.rs` free (WP-019 merged) |
| 8 | WP-024, 026, 031, 032, 034, 043 | websocket dependency approved and added before WP-024 |
| 9 | WP-028, 033, 035 | none |
| 10 | WP-036, 037, 039, 040, 042 | hook signatures in `conflict/`, `delta/resync.rs`, `notify/` and `api.rs` unchanged since WP-029 |
| 11 | WP-038, 044 | none |

Owned-path check within each wave:

- **Wave 6.** Core (two new files), ipc (four source files, tests, protocol README), store (whole crate and schema doc), simulator write side (`model.rs`, `write.rs`), simulator identity (three new files). No overlap, provided WP-014 really did move the simulator's router and control API into per-feature files. If it did not, WP-019 and WP-023 collide in `lib.rs`, `server.rs` and the README.
- **Wave 7.** Store (WP-018 alone in the crate). Simulator: `session.rs` (020), `model.rs` and `delta.rs` (021), `throttle.rs` (022); 020 only calls WP-019's `commit_upload`, and 022 only reads WP-023's tenant registry. Graph: `write.rs` and `error.rs` (025) against `claims.rs`, `identity.rs`, `discover.rs`, `model/identity.rs` (027); they would collide in `Operation` if WP-014 had not added the variants. Engine: 029 owns seven top-level files, 030 owns `scheduler/`. ipc: `server.rs`, `client.rs` (041). No overlap.
- **Wave 8.** Simulator `notify.rs` (024); graph `session.rs` (026); engine `delta/{convert,plan,run}.rs` (031), `accounts/` (032), `transfer/download.rs` (034); CLI crate (043). No overlap.
- **Wave 9.** Graph `notify.rs` (028); engine `mount/` (033); engine `local/` and `transfer/upload.rs` (035). No overlap.
- **Wave 10.** Engine `conflict/` (036), `delta/resync.rs` (037), `library/` and `policy.rs` (039), `notify/` (040); daemon crate (042). No overlap in files. The real coupling is the hook signatures: if any of these needs a different signature from the WP-029 stub, it must stop and report, not edit `mount/worker.rs`.
- **Wave 11.** Engine `recovery.rs` (038); daemon tests, `justfile`, script (044). No overlap.

Two cautions:

- **Wave 7 is too big to review well.** It has nine packages, three of them foundations (018, 025, 041). Wave 2 had five packages and took about 45 agent runs. I would run it as two halves: 018, 025, 029, 030, 041 first, then 020, 021, 022, 027.
- **Critical path:** 014, 017, 018, 035, 036, 038, 044. WP-018 is on it and is the package most likely to need five rounds.

Droppable without losing the M1 outcome: WP-024, 028 and 040 (socket notifications; polling covers it), WP-022 (scripted faults cover the scheduler), and the Teams calls in WP-023 and WP-027.

## 3. Store schema design sketch

I recommend two migrations rather than one, so each can be reviewed properly: version 2 in WP-017 and version 3 in WP-018.

### Version 2 (WP-017): accounts, mounts, delta runs, listing

**`accounts`**, by `ALTER TABLE ADD COLUMN`:
- `kind TEXT NOT NULL DEFAULT 'work'` (validated by the Rust type, like `items.kind`).
- `username TEXT`, `display_name TEXT`, `org_name TEXT`.
- `needs_sign_in INTEGER NOT NULL DEFAULT 0 CHECK (needs_sign_in IN (0, 1))`.
- `hash_percent_refused INTEGER NOT NULL DEFAULT 0` (Q24).

**`mounts`**, rebuilt. This is a tiny table that no foreign key references, so the rebuild is cheap and lets the new cross-column CHECKs be table constraints. New columns:
- `drive_type TEXT NOT NULL`, `display_name TEXT`.
- `paused INTEGER NOT NULL`, `paused_until INTEGER` (wall-clock seconds).
- `blocked_reason` becomes a code from the open `BlockedReason` set; add `blocked_message TEXT`.
- `read_only_reason TEXT` beside the existing `read_only`.
- `delta_run TEXT` with a CHECK list: `initial`, `incremental`, `resync-apply`, `resync-upload`; `NULL` when no run is in progress.
- `delta_next_link TEXT`: where the current run continues.
- `delta_staged_link TEXT`: the delta link received at the end of a run whose staged set is complete but not yet applied.
- `delta_token` keeps its meaning: the live token, the only one a new run may start from.

Invariants as CHECKs:
- `delta_run IS NOT NULL OR (delta_next_link IS NULL AND delta_staged_link IS NULL)`.
- `delta_next_link IS NULL OR delta_staged_link IS NULL`.
- `blocked_message IS NULL OR blocked_reason IS NOT NULL`.
- `paused = 1 OR paused_until IS NULL`.

There is **no stored mount state column** (decision D3). The reported `MountState` is derived by one pure function: blocked if `blocked_reason` is set, else paused, else resyncing if `delta_token IS NULL` or the run is a resync, else active.

**`delta_staged`**, new, `STRICT, WITHOUT ROWID`, primary key (`mount_id`, `item_id`), `mount_id` references `mounts` with `ON DELETE CASCADE`:
- `seq`, `deleted`, `problem` (text, `NULL` for a usable entry), `is_root`, `facet` (file, folder, package, remote-item).
- The server columns: `parent_item_id`, `name`, `size`, `created_at`, `modified_at`, `etag`, `ctag`, `server_hash`.

Invariants:
- Rows exist only while `mounts.delta_run` is set.
- A page and its next link commit together.
- The upsert on the key is what implements "last occurrence wins" (ADR-0011 R2).
- Applying the staged set, clearing it and making the token live are one savepoint. I am unsure whether one transaction for 300,000 rows is acceptable; if not, apply in chunks. Each item's application is idempotent and the token still moves only at the end.

**`items`**:
- Drop `items_by_parent`; create `items_by_parent_name (drive_id, parent_item_id, name)`. No rebuild.
- If you accept decision D1: `local_id INTEGER NOT NULL` with a unique index, a stable local handle that names the content file and can be the FUSE inode in M2. It cannot be added as `UNIQUE` by `ADD COLUMN`, so add it nullable, fill it in the migration and create a unique index. I have not checked whether SQLite lets a `NOT NULL` column without a constant default be added; if not, it stays nullable with the index, or it goes in with the version 3 rebuild.
- If you reject D1: F02's `materialised INTEGER NOT NULL DEFAULT 0` instead.

**New operations:** `accounts()`, `set_account_needs_sign_in`, mount updates for each fact, `children_after`, `child_by_name`, `item_count(drive)`, `begin_delta_run`, `stage_delta_page`, `staged_count`, a parents-first walk of the staged set, `unstaged_items` (the resync anti-join), `finish_delta_run`, `remove_mount`.

**Nothing in version 2 rebuilds `items`.**

### Version 3 (WP-018): pending operations

**`pending_ops`**, new, `STRICT`:
- `op_id INTEGER PRIMARY KEY AUTOINCREMENT`: recorded order, which F08's drain order needs.
- `drive_id`, `item_id`, referencing `items` with `ON UPDATE CASCADE ON DELETE CASCADE`.
- `kind` with a CHECK list: `create`, `content`, `move`, `delete` (F13's four).
- `base_etag TEXT`: frozen when the operation is recorded. Required for `delete`: F12 sends the eTag the user last saw, and the delta catch-up before a delete would otherwise refresh it and defeat the guard.
- `server_parent_item_id`, `server_name`: for `move`, where the server last had the item.
- `held INTEGER NOT NULL DEFAULT 0`: the M2 mass-delete guard, so M2 needs no rebuild.
- `attempts`, `not_before` (wall-clock seconds), `error_code`.
- `UNIQUE (drive_id, item_id, kind)`.

**`upload_sessions`**, new, primary key (`drive_id`, `item_id`), same foreign key:
- `generation`, `size`, `local_hash`, `snapshot_name`, `next_offset`, `expires_at`, and `upload_url` if you accept decision D15.

**`items`**, by `ADD COLUMN`: `local_deleted INTEGER NOT NULL DEFAULT 0`, `error_code TEXT`, `error_count INTEGER NOT NULL DEFAULT 0`, `retry_at INTEGER`.

**Invariants:**
- `local_dirty = 1` exactly when a `create` or `content` operation exists. Both are written in the same savepoint by `mark_local_change` and the create operation.
- `parent_item_id` and `name` in `items` are the **local** tree, the one the user sees and `resolve_path` walks. While a `move` operation exists, no server report writes them; the server's location goes to the operation row.
- An item with any pending operation is never `in-sync` and is never removed by a remote delete.
- An item with a `delete` operation has `local_deleted = 1` and is hidden from `children_after`, `child_by_name` and `resolve_path`.
- Restoring a refused or reverted delete is dropping the operation and clearing the flag: the row never left.

**How each requirement in the question is met:**

| Requirement | Mechanism |
|---|---|
| Pending-operation records | `pending_ops` |
| Mount and account state | version 2 facts, derived state |
| Next link versus live token | `delta_next_link`, `delta_staged_link`, `delta_token` |
| Remote-changed handling | unchanged from version 1 (`remote_changed`, `remote_*`), plus `upload_precondition(key)` so the `If-Match` rule lives in the store |
| Local rename or move as pending | `record_local_move` replaces `move_item_local`; `complete_move`; the three server-writing operations respect it |
| Adopt server item | `adopt_server_item(provisional, server_key)`: removes the clean server row, re-keys the provisional one, re-parents both sets of children, in one savepoint |
| Remote deleted while locally modified | `detach_from_server(key, new_provisional_id)`: the reverse of `rekey_item`; the item becomes provisional again with its bytes |
| Content turned out unchanged | `clear_unchanged_local` (see criterion 7 of WP-018). This is missing from the "Not in version 1" list, yet F04 step 14 and F08's recovery both need it |
| Conflict split | `split_conflict`, atomic. With `local_id` it re-points the content to the copy and gives the original a fresh handle, so no file is renamed and no crash window exists |
| Error retry count | `error_count`, `retry_at`, `error_code` |
| Mount removal | `remove_mount` (version 2) |

**Table rebuild flag.** The only reason to rebuild `items` is to make "pending operation implies not in-sync" a table CHECK. That needs a denormalised `pending_op_count` column kept by the store's operations (or by triggers, which the schema does not use yet) and `CHECK (pending_op_count = 0 OR sync <> 'in-sync')`. I believe SQLite accepts a column-level CHECK that references another column in `ADD COLUMN`, and tests existing rows against it, which would avoid the rebuild. I have not verified this; WP-018 should prove it with a five-line test first. If it fails, rebuild `items` in version 3. No released store exists and the version 1 tests already run an `items` rebuild through `Store::open`, so this is the cheapest moment there will ever be. Your wave-2 lesson ("prefer a constraint to a convention") argues for having the CHECK either way.

**Still not in the schema after M1:** per-folder aggregate counters (Q26), an activity or conflict log, the VFS reader connection.

## 4. Engine internal structure

Modules of `mydrive-engine`, all declared as stubs by WP-014:

| Module | Main types | Pure, testable without I/O | Injected |
|---|---|---|---|
| `api` | `Engine` facade; view types of core types only; `EngineEvent` | view conversion | — |
| `db` | `Db`: the single owner of `Store`, on its own thread | — | the store path |
| `backing` | `Backing`: content files under one mount's directory | path construction and escape checks | directory root; a failure-injecting wrapper for tests |
| `scheduler` | `Queue` (state machine), `Scheduler` (async handle), `Class`, `Permit` | all of `Queue`: ordering, fairness, caps, holds | `Clock` |
| `accounts` | `AccountRegistry`, `SignInAttempt`, tenant-to-scheduler map | identity mapping from claims and `/me` | `Arc<dyn TokenStore>`, authority and Graph base URLs, `Clock` |
| `mount` | `MountManager`, `MountMachine`, `Supervisor`, `trait WakeSource` | `MountMachine` (events to actions), mount-point checks, derived mount state | `Clock`, `WakeSource` |
| `delta` | `convert`, `plan` (`StoreAction`), `run`, `resync` | `convert`, `plan`, the reconcile decision | scheduler, `Db` |
| `transfer` | `Downloader`, `Uploader` | request choice (simple or session, by size and kind), range arithmetic, outcome mapping | scheduler, `Db`, `Backing`, `Clock` |
| `local` | the local-change API: record create, mark before write, close, rename | coalescing rules | `Db`, `Backing`, `Clock` (quiet period) |
| `conflict` | `decide` (`Decision`), `apply` | all of `decide` | host name |
| `recovery` | `plan_recovery`, `RecoveryAction` | the planner | `Db`, `Backing` |
| `library`, `policy` | preview, confirm, `classify_forbidden` | `classify_forbidden`, the preview checks | scheduler |
| `notify` | socket `WakeSource`, coalescer | coalescing and reconnect schedule | `Clock` |

Design points I would hold the packages to:

- **Each per-mount worker is the only writer for its mount**, sending closures to `Db`. That gives the ordering the store needs ("never apply a delta page fetched before a `complete_upload` after it committed") a single place to live: the worker stamps each fetch and each completed upload with a per-mount sequence number, and the planner refuses stale entries.
- **Sans-I/O cores.** `Queue`, `MountMachine`, `delta::plan`, `conflict::decide` and `plan_recovery` take the current time and inputs as arguments and return decisions. The async shells are thin. The verifier's model and mutation tests have something to bite on, and most tests need no runtime.
- **Where a `Clock` is needed:** the scheduler's hold and HTTP-date `Retry-After`; the quiet period; polling and safety polls; socket reconnect back-off; the blocked-mount re-probe; the download no-progress limit; sign-in attempt expiry; `paused_until`; `retry_at`.
- **Do not rely on tokio's paused time for tests that talk to the simulator.** The simulator uses real loopback sockets. With `start_paused`, the runtime auto-advances time when it has nothing ready, which can happen while a request is in flight, and fire a 30 s timeout spuriously. I have not reproduced this here; it is a known hazard of the mechanism. Use `ManualClock` and advance it explicitly. Note that `mydrive-graph`'s provider and loopback code use `tokio::time` and `SystemTime::now()` directly and are not injectable, so engine tests cannot move token expiry with `ManualClock`.
- **Other injected dependencies:** the `TokenStore`; the host name; the two base URLs; the ID generator for provisional item IDs and mount IDs (seeded in tests).
- **The M2 seam.** The `local` module is the API the VFS will reach through `mydrive-core` traits. M1's `import` must be implemented on top of it and nowhere else, so M1 exercises the M2 write path. The ranged download call in `transfer` must take a byte range and report what arrived, so M2's `Hydrator` is a thin adapter. I would leave defining the core traits themselves to M2.

## 5. Risks and sequencing

**Most likely to need several review rounds**
1. WP-018 (store version 3). It changes three existing operations that protect dirty content and adds four that clear or move it. Ask the reviewer: "list every public operation and what it can do to a dirty item or a pending move" and "find a sequence that leaves bytes with no dirty row".
2. WP-031 and WP-036 (delta planning and conflicts). The decision tables are where data loss would hide. Ask: "find an interleaving of a delta page, an upload completion and a local write that loses a version".
3. WP-016 and WP-041 (protocol and server). Review as an attacker with a raw socket, as wave 3 taught.
4. WP-025 (error kinds). Ask: "which two failures that need opposite reactions share a kind?" The "outcome unknown" write is the one I would look at first.
5. WP-030 (scheduler). Ask: "can a request be lost, or dispatched during a hold?"
6. WP-029's facade, because every later package trusts its signatures.

**Build first to retire risk**
- WP-017 then WP-018 immediately. Everything on the write path waits for them.
- WP-029's `Db` and facade, so the concurrency model and the daemon boundary are settled before six packages build on them.
- A short spike before WP-024: capture what a real socketIo endpoint speaks. Without it the three notification packages are built on a guess.

**Real-account gates**
- **G1** (Range on authenticated content): M1 does not wait. `Download` already forces both cases to be handled, and the M1 CLI downloads whole files. It matters for M2.
- **G5** (device code): nothing in M1 depends on it.
- **G2** (policy errors, read-only detection, shortcut stubs): WP-039 ships with a provisional classification table in one function and the simulator's guesses marked as such. M1's outcome ("one library") holds on the simulator. Do not call blocked or read-only detection done until G2 reports.
- **Not covered by any gate today**, and all need a write scope or a work tenant, which WP-011 excludes by design: whether simple `PUT` honours `If-Match`; what SharePoint rewriting looks like in the upload response versus a later delta; the refusal for `#` and `%` (Q24); item ID casing (Q23); folder-delete reporting (Q31); the socketIo protocol; the Teams scope (Q11). I recommend a new gate with its own experiment package against a scratch folder, with the owner's approval (decision D7).
- **App registration (Q22)** blocks only real-account smoke tests. `DEFAULT_CLIENT_ID` is all zeros, so M1 built on the simulator cannot sign in to anything real until it is set.

**Things that look wrong, contradictory or missing**

| # | Where | Problem |
|---|---|---|
| 1 | ADR-0011 R4 versus `store-schema.md` "Remote metadata" and `RemoteIdentity::same_content` | The rule says no code reads cTag for content equality; the store falls back to cTag when neither side has a hash. One of them must change. |
| 2 | ADR-0020 Consequences | Says the store's (parent, name) index serves keyset listing. The index has no `name` and `children` is unpaged. |
| 3 | Overview, Crates table | `mydrive-core` is said to own `Hydrator` and `Clock`; neither exists. |
| 4 | Overview diagram versus `paging.rs` docs and F01 | The daemon cannot reach the store, the token store or the sign-in types. Everything must go through the engine, which no document says. |
| 5 | F01 step 7 and J01 step 4 versus Q27 | Auto-creating the mount at sign-in contradicts asking for confirmation first. |
| 6 | F01 step 8 versus J14 steps 2 to 3 and J14-AC1 | Native-client redirect versus `http://localhost:<port>` for headless. The code supports either. Also check `auth/mod.rs` (says `127.0.0.1`) against `DEFAULT_LOOPBACK_HOST` (`localhost`) when Q30 is answered. |
| 7 | F01 step 6 | Tenant from the `tid` claim: no claims reader exists, and nothing defines what an `AccountId` is. |
| 8 | Overview CLI table and ADR-0009 "Methods" versus `docs/protocol/README.md` | No protocol method for login, logout, mount add or remove, pause, or activity. |
| 9 | `store-schema.md` "Writing items" versus F04 steps 14 to 15 and F08 step 11 | The flows return an unchanged item to in-sync; the store has no operation that clears `local_dirty` without an upload or a discard. |
| 10 | `store-schema.md` "What the engine must do" versus F07 step 4 | "Send `If-Match` with the stored eTag" makes a remote rename of a dirty file a 412 every time; the flow says retry with the new eTag. Sending the last reported eTag when `remote_changed` is false would be safe, but it is a rule change. |
| 11 | F02 step 12 versus F05 step 4 | Apply each page as it arrives, or accumulate and apply last occurrences. Per-page application can apply a delete that a later page reverses, against ADR-0011 R2. |
| 12 | F02 steps 15 to 17, F05 step 17, F07 step 5, F08 step 11 | All assume a name-mirrored backing tree, which F02 question 1 leaves undecided. |
| 13 | `docs/backlog/README.md` M1 outline versus J11 | Teams browsing is listed for M1; J11 puts it in M4. J01 and J14 are marked M2, so M1 cannot claim their acceptance IDs and needs its own criteria. |
| 14 | ADR-0012 R10 versus the M1 flow list | F12 is listed as touched by M1, but the mass-delete guard is M2. Any local delete propagation shipped in M1 breaks R10. |
| 15 | J01-AC1 versus the simulator README | "The Graph simulator accepts the sign-in": the simulator has no authority. |
| 16 | `Mount.blocked_reason: Option<String>` versus `BlockedReason` | Free text in the store, a code and a message on the wire. |
| 17 | Type names | The engine will import two `ByteRange`s with different meanings (graph: start and length, inclusive end; store: half-open), two `Timestamp`s, two `ItemKind`s, two `StoreError`s, and core and ipc each have an `AccountKind`. An off-by-one between the two ranges is a data-corruption bug. |
| 18 | `open-questions.md` "Not yet enforced" | Says 52 of 160; the script reports 60. "Becomes a failing check during M1" cannot mean all 160, since many rules are VFS or adapter rules. |
| 19 | ADR-0010 R6 | Covers tokens, codes and verifiers. F04 step 8 stores the upload URL, which is a bearer capability, in `state.db`. Nobody has ruled on it. |
| 20 | `store-schema.md` versus personal accounts | `tenant_id` is `NULL` for a personal account, but I believe a personal ID token still carries a `tid`. A mapping rule is needed. |

## 6. Decisions to make before writing the packages

| # | Decision | My recommendation |
|---|---|---|
| D1 | Backing directory layout (F02 question 1) | Key content files by a stable local ID, not by name. Renames and moves become store-only, the conflict split becomes atomic, there is no materialisation phase for 300,000 placeholders, and the ID can be the FUSE inode. Costs: amending F02, F05, F07, F08, and mount removal has to write a named tree. This is the decision with the widest effect. |
| D2 | How delta is applied | Stage every page durably and apply last occurrences when the delta link arrives, for initial, incremental and resync runs alike. The alternative (per-page apply first time, in-memory accumulation after) is less code but gives resync no durable "fresh set". |
| D3 | Mount and account state | Store facts and derive the state. Do not write `Blocked` into every item (F13 question 3); substitute it when computing display status. |
| D4 | Does login create the mount (Q27) | No. The CLI shows the account and asks. Amend F01 and J01. |
| D5 | What "download" and "upload" are in M1 | `item.hydrate` into the backing directory, plus `item.export` and `item.import` behind a capability string so they can be withdrawn. The alternative is no CLI upload until M2, which misses the stated outcome. |
| D6 | Local delete propagation in M1 | Defer to M2 with the guard. Put the `delete` kind and `held` in the schema now. |
| D7 | Uploads to existing items | Always use a session, where `If-Match` is documented, until a real-account experiment shows simple `PUT` honours it. Create a write-scope gate for that and the other unknowns in section 5. |
| D8 | Teams resolution | Out of M1 entirely; it belongs with the M4 picker, and its scope is unverified. Keep link and drive ID. |
| D9 | socket.io | Approve the websocket dependency or not. If yes, treat WP-024, 028 and 040 as droppable and do the protocol spike first. |
| D10 | One migration or two; rebuild `items` or not | Two migrations. Rebuild `items` in version 3 only if the `ADD COLUMN` CHECK does not work. |
| D11 | The engine facade and end-to-end tests | The facade is the daemon's only window; freeze its signatures in WP-029. Give the daemon a library target and put end-to-end tests in its `tests/`, driving the socket with the ipc client. A separate test crate would need a new edge in the overview diagram. |
| D12 | Pause and resume in M1 | Add the two methods to the protocol now; engine support is small (WP-033). |
| D13 | Folder mounts (Q10) | M1 mounts drive roots only; a link to a folder in a library mounts the library. |
| D14 | Account identity | Account ID from `/me` `id`; tenant from the `tid` claim; kind from the drive type of `/me/drive`, not from the tenant value. |
| D15 | Persisting the upload URL | Persist it with `Debug` redaction, in a `0600` file. The alternative, restarting sessions after a crash, is safe and costs re-uploads. |
| D16 | The rule-coverage gate | Fail only for a named list of rules M1 implements; keep the rest as a report. |
| D17 | Wave 7 | Split it in two by review capacity. |
| D18 | Rulings needed on small questions | Q7 (replacement character, when `Conflict` is durable), Q14 (suffix on collision), Q26 (defer folder aggregation), `429` without `Retry-After` (fixed 10 s), hash-less files after download, J14's exit codes and `--json` errors. |

## Where I am least sure

- The socket.io packages: I could not find the protocol details in the repository's research.
- The SQLite `ADD COLUMN` behaviours noted in section 3: verify before relying on them.
- Whether packages' children appear in delta.
- Whether `GET /organization` works with `User.Read`.
- The personal-account `tid` behaviour.
- The size of WP-035 and WP-036: either may need splitting once WP-018's final API is known. Re-read both against the merged store before marking them ready.
