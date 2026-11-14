# my-drive — a OneDrive client for Linux with Files On-Demand

Side project. This file is the entry point: read it first when coming back.

**Status (2026-11-14):** research complete; architecture decided and documented; project
infrastructure in place; waves 1 and 2 of milestone M0 merged (core types, name validation,
quickXorHash, FUSE test in CI, socket protocol, state store, Graph simulator, sign-in,
architecture checks). No daemon yet.
**Next action:** owner go-ahead for wave 3 (`docs/backlog/README.md`), owner review of the
decisions in `docs/architecture/open-questions.md`, and the Microsoft app registration.

## Project documents

| Start here | For |
|---|---|
| [docs/architecture/overview.md](docs/architecture/overview.md) | Components, vocabulary, state model, decisions, gates, milestones |
| [docs/adr/](docs/adr/) | The 18 architecture decision records |
| [docs/flows/](docs/flows/) | Flow and state diagrams |
| [docs/journeys/](docs/journeys/) | User journeys with acceptance criteria |
| [docs/backlog/](docs/backlog/) | Work packages, waves and validation gates |
| [docs/process/](docs/process/) | How work is built, reviewed and merged by agents |
| [docs/architecture/open-questions.md](docs/architecture/open-questions.md) | What is still undecided or unverified |
| [CLAUDE.md](CLAUDE.md) | Rules every agent follows |

The rest of this file is the research summary the decisions were based on.

## Goal

A OneDrive client that replicates the Windows client as closely as possible, above all
**placeholders / Files On-Demand**: files and folders appear with real sizes, download on
first access, can be pinned ("Always keep on this device") and evicted ("Free up space"),
and show status icons in the file manager.

Requirements:

- First target **Omarchy** (Arch + Hyprland); later all Linux distros.
- Sync the user's **OneDrive data** and a **user-selected set of SharePoint libraries**.
- Flexible across filesystems, distros, desktop environments and file managers.

## Where everything is

| File | What it holds |
|---|---|
| `reports/OneDrive Linux on demand clients.md` | The synthesised report (~6,600 words, with source links). Read this for the argument. |
| `research_notes/OneDrive Linux on demand clients/existing_clients.md` | Survey of every Linux OneDrive client: architecture, licence, activity, gaps |
| `research_notes/.../kernel_ondemand_mechanisms.md` | FUSE, fanotify pre-content events, other mechanisms; how Nextcloud, rclone, pCloud etc. do it; Windows/macOS state models |
| `research_notes/.../file_manager_integration.md` | Extension APIs per file manager, the Nextcloud socket protocol, Omarchy's installed components, packaging |
| `research_notes/.../graph_api_and_windows_parity.md` | Graph auth, delta, uploads, notifications, throttling, quirks, terms of use, Windows feature inventory |
| `research_notes/.../sharepoint_libraries.md` | Library discovery, scopes, tenant policy, Sync vs shortcut, read-only libraries, naming limits |

Each notes file is organised by question, with Takeaway / Cited Findings / Inferences /
Gaps. The **Gaps** sections list what was not verified; check them before relying on a claim.

## Findings in brief

### Existing clients

No established Linux client combines placeholders, pinning, free-up-space and
file-manager status icons.

| Client | Strength | Weakness |
|---|---|---|
| abraunegg/onedrive (D, GPL-3.0) | Best Graph coverage: Personal, Business, SharePoint, shared folders, WebSocket notifications, Intune SSO; very active | Full mirror sync only. On-demand request #757 open since 2019; draft FUSE PR #3600 put "On Hold" 2026-11-11. One config dir, sign-in and service per SharePoint library |
| jstaf/onedriver (Go, GPL-3.0) | Only established FUSE on-demand client | No pinning, status icons, SharePoint or shared items; files held in memory; write-path corruption reports; maintainer largely absent (auth broke Apr 2026, no reply) |
| rclone mount (MIT) | Best cache design: sparse files with byte-range tracking; reaches SharePoint; very active | No pin or status API; age/size eviction only; one remote per library; polls remote changes |
| GNOME gvfs-onedrive | In Arch `extra`, works in Nautilus | Browse/stream only; account-specific crashes and 401s |
| KDE kio-onedrive | — | Incubating; build problems; not in Arch repos |
| Insync / ExpanDrive / odrive | Commercial | Closed; no verified real placeholders |
| twodrive (Rust, MIT), konedrive (Rust, GPL) + ~10 others from 2026 | Closest to the Windows experience: FUSE + daemon + emblems + pin/release. konedrive fetches thumbnails from the cloud | Alpha, single-digit stars. Specifications, not foundations |

Microsoft has announced no Linux client.

**Recommendation:** start fresh, borrow designs. This project is GPL-3.0, so code from
abraunegg, onedriver and konedrive (GPL-3.0) or rclone and twodrive (MIT) can be reused.

### On-demand mechanism

- **FUSE** is the only mechanism that is unprivileged, works on any backing filesystem and
  is proven in production (rclone, onedriver, pCloud, SeaDrive, Nextcloud 35).
- Recommended pattern: **FUSE stacked over the real sync directory** (OpenCloud's openvfs,
  adopted by Nextcloud Desktop 35). Files are sparse files plus xattrs on the user's own
  filesystem; FUSE only blocks `open()`/reads until content is fetched. Combine with
  rclone-style byte-range caching.
- **fanotify `FAN_PRE_ACCESS`** (Linux 6.14+, ext4/btrfs/xfs) is the closest analogue to
  the Windows mechanism but needs `CAP_SYS_ADMIN` and does not yet cover directories.
  Optional later backend only. The sparse-file-plus-xattr format would be shared.
- State model to copy from Windows/macOS: hydration (placeholder / partial / full) and pin
  policy (pinned / unpinned / unspecified) are separate axes, plus an in-sync flag.
- Suggested persistence: SQLite as source of truth, `user.*` xattrs as the public
  interface, sparse extents as the cache. Report cloud size as file size, local bytes as
  blocks.
- Known FUSE hazards to design for:
  - Write-path corruption (onedriver's top issue class): write to a durable local file
    first, upload asynchronously.
  - Unintended mass hydration by thumbnailers, previewers, indexers, backup tools,
    `grep -r`. FUSE has no "don't hydrate for this reader" signal. Omarchy autostarts
    `localsearch` and ships `sushi` and thumbnailers. Proposed (unproven) defences:
    identify the calling process, circuit-breaker on mass hydration, seed thumbnails from
    Graph.
  - Offline: the daemon must reply quickly with an errno; kernel request timeouts (6.15)
    abort the whole connection.
  - FUSE passthrough and io_uring both need root, so no help for an unprivileged daemon.

### Many file managers

- Hydration lives in the filesystem layer, so **every file manager, terminal and app gets
  on-demand files with no adapter**. Adapters only draw icons and menus.
- Consensus architecture (Dropbox, Nextcloud/ownCloud, Insync): **daemon owns state and
  exposes a local socket; thin per-file-manager plugins query it.**
- Per file manager:
  - Nautilus / Nemo / Caja: emblems and menus; one Python source serves all three.
  - Dolphin: two compiled C++ plugins tied to the KDE Frameworks major version.
  - Thunar: no emblem function in the public API; menu only.
  - COSMIC Files: static user-configured actions only.
  - yazi: async "fetcher" Lua plugin can show per-file status.
  - Files / Flea on Omarchy: see the Omarchy section below (supersedes the report, which
    predates the Files merge).
- Nextcloud's socket protocol (line-based text over `$XDG_RUNTIME_DIR/<App>/socket`)
  already has `MAKE_AVAILABLE_LOCALLY` / `MAKE_ONLINE_ONLY`, but its status vocabulary
  cannot express online-only vs local vs pinned. Own a richer versioned protocol;
  Nextcloud compatibility is optional.
- No desktop-neutral per-file status mechanism exists. Neutral surfaces: StatusNotifierItem
  tray, desktop notifications, sidebar bookmarks, Secret Service, libcloudproviders
  (account-level sidebar entry in Nautilus/GTK only).
- Packaging: ship native packages (AUR first, then deb/rpm). Flatpak/snap/AppImage lose
  file-manager integration and have FUSE trouble.
- Omarchy 4.5.0 (inspected locally): Nautilus 51.1 with nautilus-python 4.2.0, fuse3,
  gnome-keyring, Quickshell bar/tray/notifications (Waybar and Mako removed in 4.0),
  portals hyprland + gtk. Omarchy's own Nautilus Python extensions live in
  `~/.local/share/nautilus-python/extensions/`. Omarchy apps are Qt Quick, so Qt6/QML is
  the natural settings UI. Keep the tray menu flat.

### Omarchy is replacing Nautilus with Files (checked 2026-11-14)

- DHH merged "Replace Nautilus with Files" (omacom/omarchy PR #18378) into the `quattro`
  branch on 2026-11-10. Secondary coverage says it ships as the default in Omarchy 4.6;
  no release date. 4.5.0 (this machine) still has Nautilus.
- **Files** (`folio`, github.com/omacom/folio, C++/Qt Quick, MIT) is Omarchy's own file
  manager. Flea was a contender but was not chosen; it stays installable.
- The migration **removes `nautilus`, `nautilus-python`, `sushi` and LocalSearch**. A
  Nautilus Python adapter will have no host on stock Omarchy after that. On the plus side,
  the indexer and Sushi previewer hydration risks go away.
- Files still makes thumbnails "when a tile needs one" using installed thumbnailers, and
  Space opens a full preview, so both will hydrate placeholders unless defended against.
- Files plugin interface v1 (`docs/plugins.md`): a folder with `manifest.json` plus
  commands in any language that print JSON; Files never loads plugin code. Hooks:
  - `actions`: right-click entries, limited to paths `under` a folder. Enough for
    "Always keep on this device" / "Free up space".
  - `details`: rows in the details pane for one file. Enough to show a file's state.
  - `poll` / `places`: status word and icon on a sidebar row (min 5 s interval).
  - `section`, `view`: sidebar section and main-pane view of plugin items.
  - **No per-file badge or emblem on tiles or list rows.** Open issue #77 asks for a
    "watch command" so plugins can push changes instead of polling.
- Flea (github.com/thisisgm/flea, Rust) now has a documented plugin system (`PLUGINS.md`:
  menu-action, place, rail-section, previewer, send-target, shelf); no per-file badge kind
  was seen in its headings (not read in full).
- Consequence: the first Omarchy adapter should be a **Files plugin** (a manifest and
  small scripts calling this project's CLI), with the Nautilus/Nemo/Caja adapter kept for
  other distros. Per-file status icons on Omarchy need an upstream change to Files.

### Microsoft Graph

- Graph delta, upload sessions and ranged downloads are sufficient for an on-demand client.
- Engine rules: track items by ID, never path; `quickXorHash` is the only hash on both
  account types; business delta omits `cTag`; handle `410 Gone` resyncs; upload fragments
  in multiples of 320 KiB, under 60 MiB; whole files are always re-sent.
- Change notification: `subscriptions/socketIo` (WebSocket, delegated permissions,
  personal and work) as a wake-up, then delta; slow polling as fallback.
- Throttling (business): 3,000 requests / 5 min per user; back off on `Retry-After` only;
  User-Agent `ISV|Company|App/Version`. A filesystem issuing one request per small read
  will hit the cap: read-ahead and chunk caching required.
- Auth: system browser with loopback redirect (not an embedded web view). Refresh tokens
  last 90 days and roll. rclone and abraunegg ship a shared client ID with user override.
- Not feasible through public Graph: Personal Vault, differential upload, Office desktop
  co-authoring.
- Terms of use: no clause against third-party clients. Stay on documented endpoints; do
  not market as a system backup tool; use a distinct name with "for OneDrive".

### SharePoint libraries

- A library is just another drive with its own delta stream. **The unit of sync is the
  drive, not the account**: key the state store by (drive ID, item ID) from the start.
- One sign-in holding many drives is what Windows does and no Linux client offers.
- `Files.ReadWrite.All` is enough to sync a library once its drive ID is known.
  `Sites.Read.All` is only needed for site search / followed sites; request it on demand.
- Discovery without the site scope: paste a sharing link (`/shares/{encoded-url}`), browse
  Teams (`filesFolder`), or enter a drive ID. No single "list all my libraries" call.
- **Biggest adoption risk is tenant policy**: SharePoint unmanaged-device controls can
  block app access from machines that are not Intune-compliant. Needs a per-library
  "blocked by organisation policy" state. Intune broker sign-in (as abraunegg does) is the
  known answer.
- Sync a library as its own root first. "Add shortcut to My files" needs a second change
  stream, is unreliable in business delta, and has dangerous delete semantics. Rules:
  removing a mount directory must never become a recursive remote delete.
- Read-only libraries (required check-out, validation columns, draft restrictions): detect
  per library and refuse writes at the filesystem layer.
- SharePoint rewrites uploaded Office files, changing size and hash. Store two hashes per
  item (local at upload, server afterwards) rather than disabling validation.
- Validate names before upload (reserved characters, `~$`, `_vti_`, case collisions);
  400-character path limit; Microsoft advises at most 300,000 synced items in total.
- Throttling budget is per tenant: one scheduler across all drives.
- Suggested layout: `~/OneDrive - <Org>` per work account, `~/<Org>/<library>` for
  libraries. Emblems add a lock (read-only) and chain-link (shortcut).

## Deadlines from Microsoft

| Date | Change | Impact |
|---|---|---|
| After Nov 2026 | `/me/drive/sharedWithMe` stops returning data | Shared content only via "Add shortcut to My files" |
| 2027-04-01 | Pre-authenticated download/upload URLs retired (Message Center MC1492958, read via a mirror) | Ranged reads currently depend on them; behaviour of `Range` on authenticated `/content` is unknown |

## Before building

Four checks, each of which could change the architecture:

1. Test `Range` on authenticated `/content` requests for personal and work accounts.
2. Test against a work tenant with unmanaged-device restrictions enabled; observe the Graph
   errors, and how a shortcut stub appears in delta.
3. Read the FUSE design record in abraunegg PR #3600 and find out why it went on hold.
4. Ask on omacom/folio for a per-file status hook (badge on tiles and list rows) in the
   Files plugin interface, and ideally a way for a plugin to supply thumbnails.

## Proposed build order

Recommended by the research; not yet decided.

1. Daemon, state store built around mounted drives, and CLI. Prove on the personal drive
   plus one SharePoint library as its own root, under one sign-in.
2. Stacked FUSE layer, with read-only enforcement for locked libraries.
3. Files (`folio`) plugin for Omarchy, tray, settings UI.
4. Shortcuts, site search, adapters for Nautilus/Nemo/Caja, Dolphin and yazi.

## Undecided

- Implementation language (Rust, Go and C++/Qt all have precedent; not researched as a
  comparison).
- Project/product name.
- Own app registration vs shared client ID.
- One FUSE mount per account vs per drive.

## Licence

GPL-3.0-or-later (decided 2026-11-14; ADR-0002); see `LICENSE`. This allows reusing code from the other
GPL-3.0 clients surveyed (abraunegg/onedrive, onedriver, konedrive) as well as MIT ones.

## Caveats on the research

- No client's source code was audited; judgements rest on READMEs, docs and issue trackers.
- Reddit and Hacker News were not sampled.
- The Windows feature inventory and per-feature feasibility are largely assessment, not
  sourced.
- Thumbnailer/indexer defences are design proposals, not observed practice.
- Omarchy facts come from this one machine (4.5.0) plus the 4.0.0 release notes.
- See the report's SharePoint section and each notes file's Gaps for the full list of
  unverified items.
