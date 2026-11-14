# Existing OneDrive clients for Linux (state as of 14 November 2026)

Method note: repository metadata (stars, last push, latest release, licence, language) was read from the GitHub REST API on 2026-11-14; Arch/AUR package data from the AUR RPC and archlinux.org package search on the same day. The Arch Wiki OneDrive page could not be read (Anubis bot protection returned "Access Denied"). Reddit and Hacker News threads were not reached. Anything marked "unverified" comes from the researcher's background knowledge, not from a source read in this session.

## Q1. Per-client architecture, language, licence, activity, popularity

### Takeaway
Only two mature open-source options exist: abraunegg/onedrive (D, full two-way sync, very actively maintained, no on-demand yet) and jstaf/onedriver (Go, FUSE on-demand, sporadically maintained with 161 open issues). Everything else is either a generic tool (rclone, gvfs), a paid closed product (Insync, ExpanDrive, odrive), dead (skilion, oxalica/onedrive-fuse, Celeste), or one of a wave of very young 2026 "Files On-Demand for Linux" projects with single-digit stars.

### Cited Findings

Summary table (GitHub API / AUR RPC / Arch package search, 2026-11-14):

| Project | Architecture | Lang | Licence | Stars | Latest release | Last push | Arch availability |
|---|---|---|---|---|---|---|---|
| abraunegg/onedrive | Full sync daemon (local folder mirror), no mount | D | GPL-3.0 | 13,000 | v2.5.12, 2026-10-24 | 2026-11-14 | AUR `onedrive-abraunegg` 2.5.12-1 (154 votes) |
| jstaf/onedriver | FUSE network filesystem, on-demand download | Go (cgo, webkit2gtk-4.1) | GPL-3.0 | 2,485 | v0.15.0, 2026-01-31 | 2026-01-31 | AUR `onedriver` 0.15.0 (32 votes) |
| rclone | CLI sync/copy/bisync + FUSE mount with VFS cache | Go | MIT | 61,003 | v1.76.0, 2026-11-06 | 2026-11-14 | official `extra/rclone` 1.76.0 |
| bpozdena/OneDriveGUI | GUI front end for abraunegg/onedrive | Python | GPL-3.0 | 1,330 | v2.4.0, 2026-10-24 | 2026-10-26 | AUR `onedrivegui` 2.4.0-2 (11 votes), `onedrivegui-git` |
| skilion/onedrive | Full sync (ancestor of abraunegg) | D | GPL-3.0 | 3,942 | none | 2021-11-04, archived | not packaged |
| oxalica/onedrive-fuse | FUSE mount | Rust | GPL-3.0 | 93 | v0.2.8, 2024-06-04 | 2024-12-26, archived, "[not actively maintained]" | not in AUR |
| GNOME gvfs onedrive backend (+ msgraph, GOA) | GVfs backend, browse via Nautilus | C | (not checked) | n/a | gvfs-onedrive 1.62.0 | package 2026-10-07 | official `extra/gvfs-onedrive` 1.62.0, `extra/msgraph` 0.3.5, `extra/gnome-online-accounts` 3.58.1 |
| kio-onedrive (Bernardo Negri) | KIO worker for Dolphin, modelled on kio-gdrive | C++ (modules) | (not checked) | n/a | n/a | incubation requested Jan 2026 | not in official repos |
| Insync | Closed-source full/selective sync, GUI + headless | closed | proprietary | n/a | 3.9.11 | AUR pkg 2026-07-14 | AUR `insync` 3.9.11.60043-1 (324 votes) |
| ExpanDrive | Closed-source mount (stream + cache + pin) | closed | proprietary | n/a | 2026.6.11 | AUR pkg 2026-07-18 | AUR `expandrive` 2026.6.11.862-1 (1 vote); `expandrive-bin` flagged out of date |
| odrive | Closed-source sync agent with `.cloud`/`.cloudf` placeholder files | closed | proprietary | n/a | n/a | n/a | no `odrive-bin` in AUR |
| Celeste | GTK4/libadwaita GUI over rclone, two-way sync | Rust | (not checked) | n/a | 1.4.0 (AUR) | upstream archived Nov 2025 | AUR `celeste-bin` 1.4.0.0-3 (1 vote) |

- Sources for the table: [GitHub API abraunegg/onedrive](https://api.github.com/repos/abraunegg/onedrive), [jstaf/onedriver](https://api.github.com/repos/jstaf/onedriver), [rclone](https://api.github.com/repos/rclone/rclone), [OneDriveGUI](https://api.github.com/repos/bpozdena/OneDriveGUI), [skilion/onedrive](https://api.github.com/repos/skilion/onedrive), [oxalica/onedrive-fuse](https://api.github.com/repos/oxalica/onedrive-fuse); [AUR RPC](https://aur.archlinux.org/rpc/v5/info?arg[]=onedrive-abraunegg); [Arch package search](https://archlinux.org/packages/search/json/?name=gvfs-onedrive).

abraunegg/onedrive
- Self-described as "A fully featured, free, open-source and actively maintained Microsoft OneDrive client"; 13.0k stars, 916 forks, only ~10 open issues and 8 PRs — [README](https://github.com/abraunegg/onedrive)
- Supports OneDrive Personal, Business, SharePoint libraries, shared folders/files; "Near real-time processing of cloud-side changes using native WebSocket support", with webhooks as a manual-setup fallback — [README](https://github.com/abraunegg/onedrive)
- README "What's missing" lists "Support for Windows 'On-Demand' functionality so file is only downloaded when accessed locally"; also no on-the-fly encryption — [README](https://github.com/abraunegg/onedrive)
- Release cadence: v2.5.6 (5 Jun), v2.5.7 (23 Sep), v2.5.8 (5 Nov), v2.5.9 (6 Nov), v2.5.10 (30 Jan), v2.5.11 (30 Jun), v2.5.12 (24 Oct 2026). Years for v2.5.6–v2.5.9 are presumed 2025 and v2.5.10–11 presumed 2026; only v2.5.12's date was confirmed via API — [Releases](https://github.com/abraunegg/onedrive/releases)
- Recent features: Intune SSO via D-Bus and OAuth2 device flow and local recycle bin (v2.5.6); resumable downloads (v2.5.7); WebSocket notifications and GNOME/KDE desktop integration (v2.5.8); Microsoft Identity Broker 3.0.1, loopback OAuth callback, handling of Graph `sharedWithMe` deprecation (v2.5.11); DB schema 18 to 19 forcing full re-validation (v2.5.12) — [Releases](https://github.com/abraunegg/onedrive/releases)

jstaf/onedriver
- "A native Linux filesystem for Microsoft OneDrive"; a network filesystem, not a sync client; 2.5k stars, 106 forks, 150 open issues, 11 open PRs; packaged for Fedora COPR, openSUSE OBS, Debian/Ubuntu, AUR, Gentoo, NixOS — [README](https://github.com/jstaf/onedriver)
- v0.15.0 (31 Jan 2026): refreshed UI, fixed auth bug after Microsoft changed token format, moved to webkit2gtk-4.1. Prior releases: v0.14.1 (18 Oct; fixed file corruption introduced by v0.14.0), v0.14.0 (25 Jul; quickXorHash for both account types, content cache moved from boltdb to local filesystem), v0.13.0 (3 Nov; GUI rewritten in Go, multipart downloads >10 MB), v0.12.0 (internal rewrite "to talk to the kernel directly") — [Releases](https://github.com/jstaf/onedriver/releases)
- No fork with meaningful traction: top forks by stars have 1–2 stars (NBA2K1/OneDriver 2, perezosoman/onedriver 1) — [GitHub forks API](https://api.github.com/repos/jstaf/onedriver/forks?sort=stargazers)
- Auriora/OneMount is a Go "Linux Access to OneDrive made simple" project (GPL-3.0, 0 stars, 85 open issues, created 2025-04-26, last push 2026-07-19) — [GitHub API](https://api.github.com/repos/Auriora/OneMount)

rclone
- OneDrive backend supports drive types personal, business, documentLibrary; setup can target a SharePoint site, drive ID or site ID — [rclone OneDrive docs](https://rclone.org/onedrive/)
- "Business and SharePoint users need rclone v1.76 or later before the April 2027 cutoff for pre-authenticated URLs" (paraphrase of docs) — [rclone OneDrive docs](https://rclone.org/onedrive/)

OneDriveGUI
- "A GUI for Linux OneDrive Client with multi-account support"; v2.4.0 released the same day as onedrive v2.5.12 (2026-10-24) — [GitHub API](https://api.github.com/repos/bpozdena/OneDriveGUI)

GNOME (GOA + gvfs + msgraph)
- OneDrive support arrived with GNOME 46 and relies on the new `msgraph` library; Ubuntu enabled it for 24.04 via a feature-freeze exception — [Launchpad bug 2061857](https://answers.launchpad.net/bugs/2061857)

KDE
- kio-onedrive is a KIO worker by Bernardo Negri modelled on kio-gdrive; incubation into KDE was requested on 27 January 2026 — [kde-devel](https://mail.kde.org/pipermail/kde-devel/2026-January/004090.html)
- A reply noted it uses C++ modules, has build problems with current Qt and GCC 15 and builds only with clang 18+; the default OAuth client ID is tied to the author's personal Microsoft account — [kde-devel thread](https://mail.kde.org/pipermail/kde-devel/2026-January/004101.html)
- KDE bug 485737 "Add support for Microsoft Accounts" (KAccounts), filed April 2024, was still in REPORTED status per search snippet — [bugs.kde.org](https://bugs.kde.org/show_bug.cgi?id=485737)
- Nicolas Fella wrote (Nov 2025) that KAccounts is hard to iterate on and a replacement online-accounts system was being considered — [planet.kde.org](https://planet.kde.org/nicolas-fella-2025-11-15-a-new-online-accounts-system)
- Earlier forum announcement "Kio-onedrive is ready (for basic usage)" — [discuss.kde.org](https://discuss.kde.org/t/kio-onedrive-is-ready-for-basic-usage/17261)

Insync
- Pricing: "Insync" desktop licence $39.99 one-time per cloud account (discounted from $49.99), includes 1 year "Insync Care" (renewal $19.99/yr), unlimited machines, command-line syncing for Linux; "Insync Office" $159.99/yr for server/work use; OneDrive is a supported cloud and each cloud account is licensed separately — [insynchq.com/pricing](https://www.insynchq.com/pricing)

ExpanDrive
- Vendor describes it as a mount: streams files when opened, caches what is used, lets you pin folders for offline access; native on Ubuntu, Debian, Fedora, RHEL; headless server edition; OneDrive and SharePoint — [ExpanDrive OneDrive on Linux](https://www.expandrive.com/integrations/onedrive/linux), [ExpanDrive blog](https://www.expandrive.com/blog/onedrive-and-sharepoint-on-linux-in-2023)

odrive
- Linux sync agent creates a `.cloud` placeholder file per file and `.cloudf` per directory; `odrive sync` on a placeholder pulls content (2017 description) — [Scott Lowe blog](https://blog.scottlowe.org/2017/08/14/using-odrive-cloud-storage-linux/)
- Unsync (turn files back into placeholders to reclaim space) is a Premium feature — [odrive docs](https://docs.odrive.com/docs/free-vs-premium-capabilities)
- odrive pricing page banner: "The free consumer odrive accounts are sunsetting March 31, 2026" — [odrive.com/pricing](https://odrive.com/pricing)

Celeste
- GTK4/libadwaita GUI backed by rclone; upstream GitHub repo archived November 2025; a Gitea fork modernises the toolchain. Supported providers listed are Dropbox, Google Drive, Nextcloud, Owncloud, pCloud, Proton Drive, WebDAV — OneDrive was only a roadmap item — [Gitea fork](https://gitea.com/unrivaled014/celeste), [OMG! Linux](https://www.omglinux.com/celeste-gui-file-sync-app-for-linux/)

New projects 2025–2026 (all from GitHub API on 2026-11-14 unless noted)

| Repo | Lang / licence | Stars | Created | Last push | Description |
|---|---|---|---|---|---|
| digit1024/ondrivefs_bmideas | Rust / MIT | 17 | 2025-06-27 | 2026-05-29 | FUSE OneDrive, built for Pop!_OS/COSMIC |
| LumiaBlack51/twodrive (also DavidXile/twodrive, 0 stars) | Rust / MIT | 5 | (DavidXile copy 2026-10-22) | 2026-11-12 | "Linux OneDrive Files-On-Demand client for GNOME" |
| kartas39/konedrive | Rust / GPL-3.0-or-later per README | 4 | 2026-09-25 | 2026-10-26 | KDE client with Windows-style Files On-Demand |
| franzjeger/OneDriveForLinux | Rust / MIT | 1 | 2026-03-29 | 2026-09-24 | FUSE on-demand, tray, Dolphin overlay icons |
| StrangeDaysTech/lnxdrive | Rust | 0 | 2026-03-05 | 2026-08-18 | "hexagonal architecture" sync client |
| Dandiccf/cirrove | Rust / Apache-2.0 | 0 | n/a | 2026-11-12 | "Foundation for a Linux cloud filesystem service: on-demand access ... OneDrive first" |
| Kaike-png/craftDriver | C# (Avalonia, FUSE) / MIT | 2 | 2026-11-04 | 2026-11-12 | OneDrive + SharePoint with on-demand files |
| startsecurity/open-od | C# / MIT | 0 | 2026-10-13 | 2026-10-13 | Files On-Demand (FUSE, GNOME) |
| Breedible/onedrive-linux-ondemand | Python / GPL-3.0 | 0 | 2026-10-30 | 2026-11-05 | On-demand for Linux Mint (Nemo) built on rclone |
| Danielxpj/OneDriveUI | Python | 2 | 2026-09-01 | 2026-11-09 | "reproduces the Windows 11 OneDrive sync client, built on rclone" |
| dzuritaa/cloudmarks | Python / MIT | 0 | 2026-10-20 | 2026-11-08 | "OneDrive status badges and Keep offline controls for GNOME Files (Nautilus)" |
| danoddo88/onedrive-linux-sync | Shell / MIT | 0 | n/a | 2026-10-23 | on-demand + "always keep on this device" scripts |
| dyle71/rclone-cloud-sync | Python / MIT | 0 | n/a | 2026-11-04 | rclone bisync + FUSE mounts in systemd units |

- Source: [GitHub repository search](https://api.github.com/search/repositories?q=onedrive+linux+fuse+pushed:>2025-06-01&sort=stars), [second search](https://api.github.com/search/repositories?q=onedrive+%22on-demand%22+linux+pushed:>2025-01-01&sort=stars)
- None of the new projects is in the AUR (`twodrive`, `onemount`, `onedrive-fuse` all return no result) — [AUR RPC](https://aur.archlinux.org/rpc/v5/info?arg[]=twodrive)

### Inferences
- The onedriver v0.14.1 release ("18 Oct") was almost certainly October 2023, not 2025: the orphaned AUR `onedriver-git` package is at `0.14.1.r8` with a last-modified date of 2024-05-16, so 0.14.1 predates May 2024. That implies a gap of roughly 27 months before v0.15.0 (Jan 2026), and no pushes in the 9.5 months since. "Stalled/sporadic maintenance" is a fair description.
- The cluster of 10+ new on-demand OneDrive repositories created between September and November 2026 (several in C#/Python/Rust with near-zero stars and very fast initial commits) suggests a wave of AI-assisted personal projects; none shows evidence of a user base. They are useful as design references (especially twodrive and konedrive, which target exactly the Windows feature set) but not as proven foundations.
- For Omarchy (Arch + Nautilus), the zero-effort options today are `extra/gvfs-onedrive` + GOA (browse-only GVfs), `extra/rclone`, and AUR `onedrive-abraunegg` / `onedriver`. No packaged client delivers Windows-style on-demand with Nautilus status emblems.

### Gaps
- Arch Wiki OneDrive page was blocked by Anubis; its recommendations could not be captured.
- gvfs/msgraph licence, stars and upstream commit activity on GNOME GitLab were not fetched; only Arch package versions are confirmed.
- kio-onedrive's current repository location, licence and whether KDE incubation completed are unverified. It is not in Arch official repos; AUR presence was not checked under other names.
- Insync's current feature list for OneDrive (SharePoint, shared-with-me) and whether it offers any on-demand mode on Linux were not confirmed; unverified background: Insync is a full/selective sync client without placeholders on Linux.
- ExpanDrive current pricing/licence terms were not confirmed (unverified background: it moved to free-for-personal-use around 2023).
- odrive current Linux agent status and consumer pricing after the March 2026 free-tier sunset are unverified; aggregator prices conflict ($150/yr vs $240/user/yr).
- Which twodrive repository is the original (LumiaBlack51 vs DavidXile) is unclear.

## Q2. On-demand, pinning, free-up-space, status icons, SharePoint/shared, account types, conflicts, change notification, large files, listing/thumbnail performance

### Takeaway
No established Linux client reproduces the Windows Files On-Demand triad (online-only placeholders with real sizes, per-item "Always keep on this device", "Free up space") together with file-manager status emblems. onedriver gives on-demand but no pinning, no status and no SharePoint; abraunegg gives the best Graph coverage and real-time notification but full sync only, with an on-demand FUSE branch in draft and currently "On Hold"; rclone gives a tunable sparse cache but no pin/state API.

### Cited Findings

abraunegg/onedrive
- On-demand: feature request #757 "On-Demand Files" opened 25 Dec 2019; moved to "In Progress" 25 Oct 2025; milestone slipped v2.5.9/10 → v2.5.11 (28 Jan 2026) → v2.5.12 (29 May 2026) → v2.5.13 (7 Aug 2026); on 7 Aug 2026 the maintainer said that, due to a client re-architecture to resolve #3751 and #3775, there is "no reasonable pathway to include this in v2.5.12"; on 11 Nov 2026 the status changed from "In Progress" to "On Hold" — [Issue #757](https://github.com/abraunegg/onedrive/issues/757)
- Draft PR #3600 "Implement FR #757: Implement On-Demand Functionality (DO NOT USE)", opened 9 Jan 2026, 67 commits, adds a FUSE library build dependency, a `src/fused/fuse.d` module and an ADR titled "FUSE overlay and local storage" with a PlantUML state machine; last visible activity 25 Oct 2026 — [PR #3600](https://github.com/abraunegg/onedrive/pull/3600)
- Groundwork already merged into releases: v2.5.10 "to support 'on-demand' development work" (EINTR/EBUSY retry handling for safeRemove/safeRename/safeBackup), v2.5.11 "stream hashing for 'on-demand' development work" — [Releases](https://github.com/abraunegg/onedrive/releases)
- Change notification: native WebSocket support (v2.5.8), fixed for SharePoint libraries in v2.5.10; webhooks as fallback — [Releases](https://github.com/abraunegg/onedrive/releases), [README](https://github.com/abraunegg/onedrive)
- Shared content: Personal and Business shared folders/files, SharePoint libraries; had to adapt to Microsoft's `sharedWithMe` deprecation for `--sync-shared-files` in v2.5.11 — [Releases](https://github.com/abraunegg/onedrive/releases)
- Data safety: safeBackup of local files on conflict/replacement, local recycle bin option, "large delete" threshold that suspends sync with GUI notification, resumable uploads/downloads — [Releases](https://github.com/abraunegg/onedrive/releases)
- Desktop integration: GNOME/KDE integration (sidebar entry, notifications) requires a GUI session; Intune SSO supported — [README](https://github.com/abraunegg/onedrive)
- Maintainer guidance for a ~4 TB OneDrive on a small disk is selective sync filtering or an alternative such as onedriver — [Discussion #3502](https://github-users.yhajoa.workers.dev/abraunegg/onedrive/discussions/3502)

jstaf/onedriver
- On-demand: files downloaded only when a program uses them; metadata and content cached in memory and on disk; previously opened files usable offline; "the filesystem becomes read-only without internet and re-enables writes on reconnect" (paraphrase) — [README](https://github.com/jstaf/onedriver)
- Conflicts: remote changes are reflected locally; local copy takes priority when both sides change — [README](https://github.com/jstaf/onedriver)
- Large files: files are loaded into memory, very large files work poorly; README recommends rclone for multi-gigabyte transfers — [README](https://github.com/jstaf/onedriver)
- Thumbnails: "File browsers such as Nautilus may download whole directories to generate thumbnails" (paraphrase of README disclaimer) — [README](https://github.com/jstaf/onedriver)
- Missing Windows-parity features tracked as open issues: "Always keep on this device" (#274, Aug 2022), "Status Indicator + Free up space" (#407, Jul 2024), "Purge files from cache if not used in last X days" (#131), "Periodically prune disk and memory caches" (#50), "Icon Tray" (#393) — [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc)
- SharePoint/shared: "Support sharepoint/teams drives" (#107, open since Feb 2021), "Add support for working with shared items" (#70, open since Apr 2020), "Shared Drives show as binary files" (#170) — [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc)
- No symlinks (ENOSYS); recycle bin not exposed — [README](https://github.com/jstaf/onedriver)

rclone (mount + VFS cache)
- `--vfs-cache-mode full`: all reads/writes buffered to disk; cache files are sparse and rclone "will keep track of which bits of the files it has downloaded"; "should support all normal file system operations" — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- Eviction: `--vfs-cache-max-age` (default 1h, based on access time), `--vfs-cache-max-size`, `--vfs-cache-min-free-space`; quotas checked only every `--vfs-cache-poll-interval`; "open files cannot be evicted from the cache"; least-accessed evicted first — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- Change detection: `--poll-interval` default 1m ("If the backend supports polling, changes will be picked up within the polling interval"), `--dir-cache-time` default 5m — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- Writes: files "are written back to the remote only when they are closed"; un-uploaded files after a crash "will be uploaded next time rclone is run with the same flags"; failed uploads retried with exponential backoff up to 1 minute in writes/full modes — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- No pinning, per-file status, or file locking is described in the mount docs — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- OneDrive specifics: "Shared with me files is not supported by rclone" (workaround: add shortcut to My files); max file 250 GiB; path < 400 chars; listings error at ~100,000 files per folder; every change creates a new version counting against quota (`no_versions`, `rclone cleanup`); QuickXorHash default since 1.62; `--onedrive-delta` enables delta listing; delta + `rclone rc vfs/refresh recursive=true` quickly populates mount metadata; iOS Live Photo `.heic` files can cause "unexpected EOF"/416 errors with `--cache-mode=full` — [rclone OneDrive docs](https://rclone.org/onedrive/)

GNOME gvfs onedrive backend
- Mounts appear in Nautilus via GNOME Online Accounts; reported defects are account-specific: gvfsd-onedrive crash when listing files, making Nautilus unable to browse — [Ubuntu bug 2064147](https://lists.ubuntu.com/archives/ubuntu-sponsors/2025-January/083007.html)
- "Failure to get any items from within OneDrive (error 401)" traced to download URLs on a different Microsoft domain — [gvfs issue 736](https://gitlab.gnome.org/GNOME/gvfs/-/issues/736)
- "Database is read only" error on mount, attributed by a commenter to the plural `/me/drives` endpoint — [GNOME Discourse](https://discourse.gnome.org/t/database-read-only-when-trying-to-open-onedrive-in-nautilus/38213)
- "Invalid mount spec" when the GOA account name is customised; fixed in gnome-online-accounts 3.54.5 — [GNOME Discourse](https://discourse.gnome.org/t/goa-onedrive-invalid-mount-spec/31318)

ExpanDrive / odrive / Insync
- ExpanDrive: stream on open, cache, pin folders for offline (vendor claim) — [ExpanDrive](https://www.expandrive.com/integrations/onedrive/linux)
- odrive: explicit placeholder files with `.cloud`/`.cloudf` suffixes rather than transparent placeholders; unsync gated behind Premium — [Scott Lowe blog](https://blog.scottlowe.org/2017/08/14/using-odrive-cloud-storage-linux/), [odrive docs](https://docs.odrive.com/docs/free-vs-premium-capabilities)
- Insync: pricing page describes no on-demand/placeholder feature — [insynchq.com/pricing](https://www.insynchq.com/pricing)

New projects implementing the Windows model
- twodrive (Rust, GNOME): read/write FUSE mount backed by local cache plus background Graph sync; durable retry queues for pending local changes; resumable large uploads; placeholders show names/sizes; "Release space" removes local cache; "Always keep" protects from automatic cleanup; five Nautilus emblem states (Online-only, In progress, Available locally, Always keep, Needs attention); context menu (Release space, Always keep, Sync now, View status); a "Release pending" state when release is requested during writes/uploads; ships daemon, CLI, Nautilus extension, tray app; OAuth 2.0 PKCE with Microsoft's public application ID since 0.2.8; tokens stored unencrypted in a 0600 JSON file; amd64 .deb for Ubuntu/Zorin only; self-described experimental — [twodrive README](https://github.com/LumiaBlack51/twodrive)
- twodrive stated limitations: newly discovered files stay online-only even under a pinned folder; folder emblems summarise descendants and don't prove all files are cached; "Pause sync" only changes the tray display; POSIX ownership/special bits not preserved — [twodrive README](https://github.com/LumiaBlack51/twodrive)
- konedrive (Rust, KDE): daemon `konedrived` serves the sync folder as a FUSE mount over a backing directory, with a store holding one row per item; GUI and `konedrivectl` talk to the daemon over D-Bus (`org.konedrive.*`); Dolphin plugin provides emblems (online-only, downloading, downloaded, kept), right-click menu, and previews fetched from OneDrive without downloading files; "Always keep on this device" and "Free up space"; personal accounts only (work/school "not supported yet"); uploads off until enabled per account; alpha; RPMs for Fedora 44/45 only — [konedrive README](https://github.com/kartas39/konedrive)
- konedrive's repo tagline says "without FUSE" but its README says the folder is "a FUSE mount the daemon serves as you" — [konedrive README](https://github.com/kartas39/konedrive); [GitHub API description](https://api.github.com/repos/kartas39/konedrive)

### Inferences
- Feature matrix against the Windows client (from the findings above):
  - Transparent on-demand hydration: onedriver yes; rclone mount yes (sparse, range-based); ExpanDrive yes (vendor claim); gvfs effectively stream-on-open, not a POSIX path cache; abraunegg no (draft only); Insync no evidence; odrive only via explicit placeholder files.
  - Per-item pinning: none of onedriver, rclone, gvfs, abraunegg. ExpanDrive (folders), twodrive/konedrive (alpha).
  - Free up space on demand: none of the mature open-source tools expose it per-file (rclone has only global LRU/age eviction; onedriver has open requests #131/#407).
  - File manager emblems: none of the mature tools; only the alpha projects (twodrive for Nautilus, konedrive/OneDriveForLinux for Dolphin) and the cloudmarks add-on.
  - Real-time remote change push: only abraunegg (WebSocket). rclone polls (1 min default). onedriver's mechanism was not confirmed in this session (unverified background: periodic delta polling).
  - SharePoint + shared-with-me: abraunegg yes; rclone SharePoint yes but shared-with-me no; onedriver neither.
- Microsoft's deprecation of `sharedWithMe` (hit abraunegg in v2.5.11) and the April 2027 pre-authenticated download URL cutoff for Business/SharePoint (rclone docs) are two live Graph API changes any new client must design around.
- konedrive's approach of fetching thumbnails from the Graph API in the file-manager plugin, rather than letting thumbnailers read file content, is the only design seen that directly addresses the "thumbnailer triggers mass download" problem; onedriver's README simply warns about it.
- twodrive's documented edge cases (pin not propagating to newly discovered children, release-pending while handles are open, local save vs cloud completion) are a ready-made checklist of semantics the Windows client gets right and a clone must define.

### Gaps
- abraunegg's on-demand design details (placeholder representation, pin/evict controls, xattrs) are in the ADR inside PR #3600, which was not read; reason for "On Hold" on 11 Nov 2026 is not stated on the issue page (many timeline items collapsed).
- onedriver's remote-change mechanism and polling interval were not verified from source.
- rclone bisync maturity for OneDrive was not researched in this session (the OneDrive backend page does not mention bisync). Unverified background: bisync is a periodic two-way sync command, not continuous, and unrelated to on-demand.
- gvfs onedrive backend capabilities (write support, thumbnails, shared/SharePoint, performance) were not confirmed; search surfaced no OneDrive-specific slowness reports (the well-known gvfs slowness issue 724 concerns Google Drive). Unverified background: the gvfs backend exposes a `gio` URI and its FUSE bridge path, has no offline cache or pinning.
- ExpanDrive, Insync and odrive behaviour for SharePoint, conflict handling and emblems on Linux not verified beyond vendor marketing.
- Office co-authoring / `~$` lock-file handling: no source found for any client in this session.

## Q3. Most common user complaints and open issues

### Takeaway
onedriver's pain points are authentication breakage with no maintainer response, missing SharePoint/shared support, memory use, hangs, and file managers/indexers triggering mass downloads; gvfs's are account-specific failures and crashes; abraunegg's main gap is simply the absence of on-demand (a 7-year-old request) plus disruptive full re-validations on schema changes.

### Cited Findings
- onedriver #478 "onedriver stopped working" (opened 5 May 2026): from late April 2026 authentication failed with `invalid_grant` / AADSTS70000 across Mint, Debian 13, Raspberry Pi OS; the headless `-n` flow also broke; no maintainer response in the thread; users found v0.15.0 fixes it, but Debian 13 ships v0.14.1 and the openSUSE download link returned 403 for one user — [onedriver #478](https://github.com/jstaf/onedriver/issues/478)
- onedriver auth: "The embedded web browser authentication window displays as blank" (#378, Jan 2024, the highest-count issue in the list at 19); "No login GUI, undocumented CLI" (#427); request for GNOME Online Accounts integration (#310) — [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc)
- onedriver stability: README admits "the filesystem may occasionally hang, which requires unmounting and remounting" (paraphrase); "Mountpoint disappearing after a few minutes after rebooting" (#480, Jun 2026); "System fails to sleep, can't wake from suspend, with KDE, when Onedriver is mounted" (#416) — [README](https://github.com/jstaf/onedriver), [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc)
- onedriver resource use and mass downloads: "High memory usage" (#49, open since Feb 2020); "Block OneDriver (Nautilus?) from syncing everything ahead of time" (#405); "Disable thumbnailing for mount" (#24, open since 2019); "Disable Baloo indexing on mountpoints for KDE users" (#211, #358) — [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc)
- onedriver write-path bugs: "OnlyOffice not always saving files" (#419); "File creation and renaming fails if the operations are performed before the file is uploaded" (#158); v0.14.0 introduced file corruption fixed in v0.14.1; earlier versions had races replacing uploads with 0-byte files — [onedriver issues](https://github.com/jstaf/onedriver/issues?q=is%3Aissue+is%3Aopen+sort%3Areactions-%2B1-desc), [Releases](https://github.com/jstaf/onedriver/releases)
- gvfs/GOA: integration "does not work for some OneDrive accounts"; gvfsd-onedrive crash on listing; 401 errors; one user reports it "only works until I shutdown or reboot"; workaround is remove/re-add account — [Launchpad](https://answers.launchpad.net/bugs/2061857), [Ubuntu bug 2064147](https://lists.ubuntu.com/archives/ubuntu-sponsors/2025-January/083007.html), [gvfs #736](https://gitlab.gnome.org/GNOME/gvfs/-/issues/736), [OMG! Ubuntu](https://www.omgubuntu.co.uk/2024/05/fix-ubuntu-onedrive-account-error)
- abraunegg: schema changes force full re-validation "that may take a long time" (v2.5.10, v2.5.12); past serious bugs fixed include "a failed download that could lead to online deletion" (v2.5.7), very high CPU/memory with `--upload-only` and unexpected deletion of empty nested folders with `sync_list` + `--resync` (v2.5.9), monitor mode missing remote changes and deletions (v2.5.12) — [Releases](https://github.com/abraunegg/onedrive/releases)
- abraunegg supports only the current release or newer master builds — [README](https://github.com/abraunegg/onedrive)
- rclone: folder listings fail around 100,000 files; SharePoint throttling may need an explicit `--user-agent`; version accumulation eats quota — [rclone OneDrive docs](https://rclone.org/onedrive/)

### Inferences
- Authentication is the single most fragile area across projects: onedriver broke twice on Microsoft-side changes (token format, AADSTS70000) and relies on an embedded WebKit view that often renders blank; abraunegg has moved to loopback-listener OAuth, device code flow and Identity Broker. A new client should use system-browser + loopback PKCE with device-code fallback and avoid embedded web views.
- Distro packages lag badly for a slow-moving upstream (Debian 13 shipped a non-working onedriver 0.14.1 for months), so users experience upstream stalls as hard outages.
- FUSE-specific complaints cluster around anything that walks or reads the tree without user intent: thumbnailers, Baloo, suspend/resume, and apps that save via create-rename sequences before upload completes. These need first-class design (process-aware hydration policy, cloud-provided thumbnails, non-blocking offline errors).

### Gaps
- Reddit, Hacker News and forum sentiment was not sampled; complaint weighting here is derived from issue trackers only.
- Specific evidence on "apps hanging on FUSE reads when offline" beyond onedriver's generic hang disclaimer and #416 was not found.
- No sourced data on Office lock files (`~$name.docx`), file locking or co-authoring behaviour in any client.
- Insync/ExpanDrive/odrive user complaints not researched.
- abraunegg issues #3751 and #3775 (which triggered the re-architecture that delayed on-demand) were not read.

## Q4. Official Microsoft statement on a Linux OneDrive client

### Takeaway
No official Microsoft Linux OneDrive client exists and no announcement or roadmap for one was found; Microsoft channels direct Linux users to the web UI or third-party clients.

### Cited Findings
- A Microsoft Q&A moderator answer (March 2026 per search summary) states OneDrive support pages cover Windows, Mac, iOS, Android and browsers but no Linux desktop app — [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/a/12695313)
- May 2026 Microsoft Q&A: "there is currently no official announcement from Microsoft regarding a plan or timeline to release a native Microsoft 365 (Office) desktop client for Linux" (concerns Office, not OneDrive) — [Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/5891967/microsoft-office-on-linux)
- Microsoft Tech Community thread "OneDrive on Linux" lists abraunegg's client, rclone, Insync and ExpanDrive as the options — [Tech Community](https://techcommunity.microsoft.com/discussions/microsoft-365/onedrive-on-linux/1318644)
- abraunegg README notes Microsoft's official Windows and macOS clients offer Files On-Demand while the Linux client does not — [README](https://github.com/abraunegg/onedrive)

### Inferences
- Microsoft Q&A answers are community-moderator statements, not corporate policy; the accurate summary is "absence of any announcement", not "Microsoft has stated it will never ship one".
- Microsoft does invest in adjacent Linux identity plumbing (Intune / Microsoft Identity Broker, which abraunegg integrates), so enterprise Linux is acknowledged, but that has not extended to a sync client.

### Gaps
- No primary Microsoft roadmap/blog statement was located either way. The Microsoft Q&A pages were seen only as search summaries, not fetched in full.

## Q5. Best base to fork or learn from

### Takeaway
There is no ideal fork base: abraunegg/onedrive is the best reference for Graph API correctness and edge cases (and may ship its own FUSE on-demand), onedriver is the best reference for a working Go FUSE on-demand design and its failure modes, and rclone's VFS cache is the best reference for sparse/range caching — while the alpha projects twodrive and konedrive are the closest functional specs of the Windows UX on Linux.

### Cited Findings
- abraunegg/onedrive: GPL-3.0, D, 13k stars, releases every 3–5 months, ~10 open issues; covers Personal/Business/SharePoint/shared, WebSockets, Intune SSO, national clouds; in-progress FUSE on-demand in draft PR #3600 with an ADR — [GitHub API](https://api.github.com/repos/abraunegg/onedrive), [README](https://github.com/abraunegg/onedrive), [PR #3600](https://github.com/abraunegg/onedrive/pull/3600)
- onedriver: GPL-3.0, Go, 2.5k stars, 161 open issues, last push 2026-01-31; talks to the kernel FUSE interface directly since v0.12.0; content cache on local filesystem since v0.14.0; whole-file-in-memory model — [GitHub API](https://api.github.com/repos/jstaf/onedriver), [Releases](https://github.com/jstaf/onedriver/releases), [README](https://github.com/jstaf/onedriver)
- rclone: MIT, Go, 61k stars, v1.76.0 on 2026-11-06; sparse-file VFS cache with range tracking and parallel chunk streams — [GitHub API](https://api.github.com/repos/rclone/rclone), [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- twodrive: MIT, Rust, Cargo workspace with mock backend for isolated testing, daemon + CLI + Nautilus extension + tray — [twodrive README](https://github.com/LumiaBlack51/twodrive)
- konedrive: GPL-3.0-or-later, Rust, daemon + D-Bus API + Dolphin plugin with cloud thumbnails — [konedrive README](https://github.com/kartas39/konedrive)
- oxalica/onedrive-fuse: Rust, GPL-3.0, archived and flagged "[not actively maintained]" — [GitHub API](https://api.github.com/repos/oxalica/onedrive-fuse)

### Inferences
- Licence constraint: abraunegg, onedriver, konedrive, oxalica are all GPL-3.0; forking or porting code from them makes the new client GPL-3.0. rclone and twodrive are MIT and can be borrowed from under any licence.
- Forking abraunegg is technically unattractive for a Windows-clone (D language limits contributor pool; architecture is a mirror-sync engine being retrofitted), but its issue history and release notes are the most valuable catalogue of Graph API quirks (shared-folder shortcuts, `sharedWithMe` deprecation, QuickXorHash on retransmitted fragments, national clouds, SharePoint file-size-changes-on-upload). Its on-demand branch is a direct competitor risk: if v2.5.13 ships FUSE on-demand, the "gap" narrows substantially, although "On Hold" status as of 11 Nov 2026 makes timing uncertain.
- Forking onedriver gives a working FUSE core fastest but inherits its known structural weaknesses (in-memory file handling, no SharePoint/shared drives, no pin/evict model, embedded-WebKit auth, 161 open issues) and an absent maintainer to merge upstream.
- A fresh design that most closely mirrors Windows would combine: (a) a daemon with a per-item state store and D-Bus API (konedrive/twodrive pattern), (b) FUSE mount over a backing directory with sparse range hydration (rclone VFS pattern), (c) Graph delta + WebSocket/webhook notifications (abraunegg pattern), (d) a Nautilus extension for emblems and context menu (twodrive/cloudmarks pattern; Nautilus is the Omarchy default), and (e) cloud-sourced thumbnails to avoid hydration (konedrive pattern).
- For Omarchy specifically, twodrive is the most relevant prior art (Rust + GNOME/Nautilus extension), but it is days-to-weeks old, .deb-only, and unproven.

### Gaps
- No source code of any candidate was audited in this session; architectural quality judgements rest on READMEs, release notes and issue trackers.
- The content of abraunegg's "FUSE overlay and local storage" ADR is the single most valuable unread document for this project and should be read directly from the PR branch.
- Whether FUSE is even the right mechanism versus alternatives (e.g. fanotify pre-content hooks merged into recent kernels) was outside this survey's scope; no surveyed project was found using a non-FUSE mechanism (konedrive's "without FUSE" tagline contradicts its own README).
