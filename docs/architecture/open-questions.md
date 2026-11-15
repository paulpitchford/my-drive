# Open questions

Things that are undecided, unverified, or where a decision goes against the research. Raised
while writing the ADRs, flows and journeys. Each has an owner and the point by which it must
be settled. Settled items move into an ADR or the overview and are deleted from here.

## Decisions that depart from the research

The owner should know these were choices, not findings.

| # | Decision | What the research suggested | Why it was decided this way |
|---|---|---|---|
| D1 | Mount point and backing directory are separate paths (ADR-0005) | FUSE stacked over the sync directory itself, as OpenCloud's openvfs does | When the daemon is down the user sees an empty or erroring directory, never placeholders that read as zeros |
| D2 | One FUSE filesystem per mount (ADR-0005, ADR-0007) | As few FUSE mounts as possible | Isolation: one library failing or resyncing cannot affect another. Costs one mount per library |
| D3 | No token file fallback; sign-in is refused without a Secret Service (ADR-0010) | An encrypted-file fallback for headless sessions | No plaintext-equivalent secrets on disk. Headless users must run a keyring; revisit if this blocks real use |
| D4 | Rust (ADR-0003) | Nothing: languages were not compared | Lead's judgement; Go is the serious alternative |
| D5 | Session upload above 4 MiB (ADR-0012) | Microsoft's guidance is reported as 10 MiB; no source for 4 MiB | Conservative default, configurable. Affects cost only, not correctness |

## To settle before the milestone shown

| # | Question | Owner | By |
|---|---|---|---|
| Q1 | Does `Range` work on authenticated `/content` (gate G1)? | WP-011, owner signs in | M1 |
| Q2 | Does device-code sign-in work for personal accounts (gate G5)? | WP-011 | M1 |
| Q5 | Do FUSE mmap page faults reach the daemon as reads in every case we rely on? | M2 VFS work | M2 |
| Q6 | What counts as "one operation" for the mass-delete guard (`rm -rf` is many unlinks)? | Lead, in the M2 write-path WP | M2 |
| Q7 | Conflict copies: what if `<name>-<hostname>.<ext>` already exists or is an invalid name? When exactly is sync `Conflict` set and cleared? | Lead, in the M1 conflict WP | M1 |
| Q8 | Save-via-rename when the temporary file's quiet period expired and it was already uploaded | Lead, in the M2 write-path WP | M2 |
| Q9 | Fairness between mounts sharing one tenant request budget | Lead, in the M1 scheduler WP | M1 |
| Q10 | A mount of a folder inside a business drive may need a whole-drive delta filtered locally | Lead, in the M1 delta WP | M1 |
| Q11 | Which scope does browsing Teams and channels need beyond the baseline? | Experiment with a work account | M1 |
| Q12 | How are read-only libraries detected through Graph? | Experiment with a work tenant (with G2) | M1 |
| Q13 | Graph errors under unmanaged-device policy; shape of shortcut stubs in delta (gate G2) | Experiment with a work tenant | M1 / M5 |
| Q14 | Layout when two accounts are in the same tenant (both want `~/<Org>`); the `<Site> - <Library>` folder naming is unverified against Windows | Lead | M1 |
| Q15 | How apps' own `user.*` xattrs on files are handled | Lead, in the M2 VFS WP | M2 |
| Q16 | Files plugin: actions are limited to fixed folders but mount points change. Likely answer: the daemon regenerates the plugin manifest when mounts change | Lead, in the M3 plugin WP | M3 |
| Q17 | Will Files accept a per-file status hook and a thumbnail hook (gate G4)? | Owner approves the upstream issue | M3 |
| Q18 | Is borrowed code from abraunegg and onedriver GPL-3.0-only or -or-later? (Affects per-file licence headers) | Checked per file when borrowing (ADR-0002) | When borrowing |
| Q19 | Freedesktop thumbnail-cache layout and Graph thumbnail endpoint details for seeding placeholder thumbnails | M2 hydration-policy WP | M2 |
| Q20 | Is the crawler deny list workable in practice (pid races, sandboxed apps)? ADR-0013 is an unproven design | Experiment in M2 | M2 |
| Q21 | Product name | Owner | Before M3 packaging |
| Q22 | Publisher verification of the app registration. The app was registered on 2026-11-15 (client ID in `mydrive-graph`'s `auth/config.rs`): public client, no secret, any organisation plus personal accounts, redirect URIs `http://localhost` and the native-client URI, delegated permissions `Files.ReadWrite.All`, `Sites.Read.All`, `User.Read`, `offline_access`. Until the publisher is verified, users in other organisations see an "unverified" consent screen and some tenants will require admin consent | Owner | Before a public release |
| Q23 | Can Graph return the same item ID in two casings (as it does for drive IDs since June 2025)? `ItemId` compares exactly | Simulator and gate work | Before the M1 delta WP |
| Q24 | Which tenants refuse `#` and `%` in names, and how the engine learns it (`hash_and_percent_allowed`) | Lead, in the M1 upload WP | M1 |
| Q25 | A path can grow past the protocol's 4,096-byte cap (renaming a parent to a longer name). The VFS must refuse creates and renames that would exceed OneDrive's own path limit, and mount points long enough to make that possible must be refused | Lead, in the M2 VFS WP | M2 |
| Q26 | How a folder's display status is aggregated from its contents (the protocol says only that the daemon computes it) | Lead; amend ADR-0008 | M1 |
| Q28 | The store does not yet record local renames and moves as pending, so a remote metadata apply or an upload completion can revert one; and it lacks "adopt a server item" and "remote deleted while locally modified" operations | Lead, in the M1 engine WP (schema v2, pending-operations records) | M1 |
| Q29 | The Secret Service client uses the `plain` session, so a token crosses the session bus unencrypted (same-user processes only) | Lead | Before M3 packaging |
| Q31 | When Graph reports only a deleted folder (not its contents), an item moved into that folder just before is never reported at all, so no client can learn of it from the feed. Unknown whether real Graph does this (it is one of the simulator's harder settings). The engine must handle a folder delete from its own store's tree and may need a targeted listing | Experiment with a real account; lead, in the M1 delta WP | M1 |
| Q32 | Accepted risk: a broken or hostile server or TLS-intercepting proxy can send bytes beyond a response's declared length on a reused connection, which are then read as the next response. Not prevented in the Graph client (it would cost a TLS handshake per ranged read). The engine must verify the whole-file quickXorHash when a file becomes fully hydrated | Lead, in the M2 hydration WP | M2 |
| Q33 | A file replaced by different content of exactly the same size passes the client's size checks; ranged reads of it can mix versions until the eTag or hash is checked. Depends on whether ranged responses carry a usable version tag (gate G1) | WP-011, then the M2 hydration WP | M2 |
| Q34 | Gate G3 passed with changes (`docs/gates/G3.md`): ADR-0005 must be amended before M2 for pitfalls another project hit. Chiefly: never mix content versions within an open handle or a partially hydrated file (invalidate extents on a remote change; check a ranged response against the version on disk); file managers get no change events for writes the daemon makes behind the mount, so the VFS must invalidate or replay them; hydration must not block other requests on the mount; renaming or deleting a placeholder never hydrates it; a backing file missing at start-up never becomes a remote delete; tell a dead, hung or live stale mount apart before unmounting; the systemd user unit must not hide the mount in a private namespace; pinned files are unreachable while the daemon is down | Lead: a new ADR amending ADR-0005 | Before M2 |
| Q35 | The M1 draft plan (`docs/backlog/drafts/M1-plan.md`, section 5) lists 20 places where ADRs, flows, the overview and the code contradict each other or are missing something M1 needs (for example: the daemon cannot reach the store except through the engine, which no document says; the protocol has no methods for sign-in or adding a mount; the store has no operation to return an unchanged file to in-sync; two types named `ByteRange` with different meanings). Each must be fixed in the documents before the package that depends on it is written | Lead, in the M1 preparation | Before each M1 wave |
| Q36 | Proposed gate G6: write-side behaviour on a real account (simple upload and `If-Match`, SharePoint rewriting, `#`/`%` refusal, item-ID casing, folder-delete reporting, the notification protocol). Needs a scratch folder and write permission | Owner decides | Before the M1 upload and conflict packages are final |
| Q37 | Account identity is not defined in any document: which value is the account ID, how the tenant is read (no reader for the ID token's claims exists yet), and how a personal account's tenant value maps to "no tenant". Plan ruling D14: account ID from `/me`, tenant from the `tid` claim, kind from the drive type of `/me/drive` | Lead, in the M1 accounts and identity WPs (027, 032) | M1 wave 8 |
| Q38 | Operations the flows need that the store lacks, to be designed in the second store migration (WP-018): returning unchanged or server-equal content to in-sync while keeping the bytes; the atomic conflict split; adopting a server item; a dirty item deleted remotely; keeping a pinned file readable while its new version downloads (M2). `store-schema.md`'s conflict order ("copy the bytes, insert, discard") is replaced by the single-transaction split | Lead, when writing WP-018 | M1 wave 7 |
| Q39 | Journeys J01 and J14 are M2 journeys (they need the mounted folder), so M1 needs acceptance criteria of its own for sign-in, mirroring and single-file transfer | Lead, when writing the M1 acceptance WP (044) | M1 wave 12 |
| Q40 | J05 says a held mass-delete pauses the mount; F12 keeps the mount active. To reconcile in M2 | Lead | M2 |
| Q41 | ADR-0003 R6 says library crates do not depend on `anyhow`. `mydrive-daemon` now has a library target beside its binary (so tests can run the daemon in-process) and the package depends on `anyhow`. Lead's ruling until an ADR says otherwise: `anyhow` may appear only in `main.rs`; the daemon's library target defines its errors with `thiserror` and has no `anyhow` type in any signature. The daemon package (WP-042) must carry this as a constraint with a test or check | Lead, when writing WP-042 | M1 wave 11 |
| Q30 | Partly answered: the registration allows `http://localhost` (any port) and the native-client URI. Still open, for gate G1 to test: whether a loopback redirect to `127.0.0.1` rather than `localhost` is accepted (RFC 8252 recommends the literal address), and which redirect the headless flow uses (the native-client URI or a loopback one). The code takes both as parameters | With the app registration (Q22) | Before WP-011 |

## Detail questions in the flows and journeys

Every file in `docs/flows/` and `docs/journeys/` ends with its own "Open questions". There
are about 150 between them, mostly product details (what happens when `~/OneDrive` already
exists, exit codes, retry intervals, how a held mass-delete is confirmed). They are not
copied here. The rule: **a work package is not `ready` until the open questions in the flows
and journeys it names are answered**, by the lead in the WP, or by the owner where the
answer is a product choice. Default values the flows propose (timeouts, chunk sizes,
polling intervals, concurrency limits) are collected in `docs/flows/README.md` and are
provisional until an ADR or WP confirms them.

Where a flow had to choose in order to be drawn, it chose the reading closest to principle 1
and recorded the choice in its own open questions. Those choices are proposals, not
decisions.

## Not yet enforced

Rules the ADRs state that CI does not check yet.

| Rule | Status |
|---|---|
| ADR-0004 R3: crate dependencies match the overview's diagram | enforced (WP-012) |
| ADR-0017 R3: every ADR rule is named by a test | reported by `just rule-coverage` (52 of 160 after wave 2); becomes a failing check during M1 |
| ADR-0017 R4: every journey acceptance ID has exactly one journey test | From M2, when journey tests exist |
