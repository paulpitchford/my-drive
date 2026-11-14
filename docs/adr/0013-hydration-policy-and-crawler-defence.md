# ADR-0013: Hydration policy: defend against crawlers, thumbnailers and indexers

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

## Context

This is an unproven design. No Linux client was found that identifies the reading process
to refuse hydration, or that applies a circuit breaker to mass hydration; only konedrive does
anything like seeding thumbnails from the cloud
([report, FUSE section](../../reports/OneDrive%20Linux%20on%20demand%20clients.md),
[konedrive README](https://github.com/kartas39/konedrive)). We are building it without prior
art and expect to tune it.

- FUSE has no "do not hydrate for this reader" signal; the Windows and macOS backends do
  ([wusel](https://wusel.itbh.at/wusel/0.3.0/explanation/vfs-first.html)). Anything that walks
  the tree and reads content hydrates it.
- onedriver warns that file managers can download whole directories to build thumbnails
  ([onedriver README](https://github.com/jstaf/onedriver)). "Local files only" thumbnailing
  has historically still covered kernel-mounted paths
  ([Launchpad 293311](https://bugs.launchpad.net/ubuntu/+source/nautilus/+bug/293311)).
- Baloo refuses FUSE network mounts
  ([KDE Discuss](https://discuss.kde.org/t/baloo-refuses-to-index-fuse-network-mounts-e-g-rclone-with-no-opt-in-override/50530));
  LocalSearch appears to skip some non-local mounts, unconfirmed.
- abraunegg's SharePoint guide names desktop indexers as data-loss triggers, which makes this
  a data-safety concern inside library roots, not only a bandwidth one
  ([abraunegg SharePoint guide](https://raw.githubusercontent.com/abraunegg/onedrive/master/docs/sharepoint-libraries.md)).
- FUSE requests carry the caller's pid in the request header; that pid is 0 for requests from
  other pid namespaces such as Flatpak sandboxes (research note
  `kernel_ondemand_mechanisms.md`, section 6; the second point is background knowledge,
  unverified).
- Omarchy is replacing Nautilus with Files (`folio`). The migration removes `sushi` and
  LocalSearch, but Files still makes thumbnails "when a tile needs one" with installed
  thumbnailers, and Space opens a full preview (README, "Omarchy is replacing Nautilus with
  Files"). Omarchy 4.5.0 still autostarts `localsearch` and ships `sushi`,
  `ffmpegthumbnailer`, glycin, papers and evince thumbnailers (research note
  `file_manager_integration.md`, sections 2 and 6, local inspection).
- The freedesktop thumbnail cache layout (`~/.cache/thumbnails/{normal,large,...}`, MD5 of the
  URI, `Thumb::URI` and `Thumb::MTime` keys, a `fail/` directory) is background knowledge,
  unverified. The Graph thumbnails endpoint was not read during research, and MC1492958 lists
  thumbnails among the APIs losing pre-authenticated URLs
  ([mirror](https://mc.merill.net/message/MC1492958)).

## Decision

`readdir`, `stat`, `open` and xattr operations never hydrate (ADR-0005). Reads hydrate, with
two exceptions.

**Deny list.** For each FUSE read the VFS identifies the calling process from the request's
pid through `/proc/<pid>/comm` and `/proc/<pid>/exe`. A configurable deny list in `config.toml`
matches either value. The default list covers:

- indexers: `localsearch` / `tracker-miner-*`, `baloo_file`, `updatedb` / `plocate`;
- thumbnailers: `gdk-pixbuf-thumbnailer`, `ffmpegthumbnailer`, `totem-video-thumbnailer`,
  `evince-thumbnailer` and similar;
- previewers: `sushi`.

A denied read of content that is not local fails with `EIO`. Content that is already local is
served normally.

**Circuit breaker.** If one process triggers hydration of more than N files or M bytes within
T seconds (defaults 50 files, 1 GiB, 60 s, all configurable), further hydration by that process
is paused and the user is notified with two choices: allow once (for the life of that process)
or always allow (adds the process to an allow list). While paused, that process's reads of
non-local content fail with `EIO` rather than blocking.

**Thumbnail seeding.** For placeholders, the engine fetches thumbnails from Graph's thumbnails
endpoint and writes them into the freedesktop thumbnail cache (`~/.cache/thumbnails`), so file
managers that use the shared cache, including Omarchy's Files, show previews without
downloading the file.

**Exemptions.** Items whose effective pin is `Pinned`, and content that is already local, are
never restricted by the deny list or the breaker.

## Alternatives considered

**No defence.** One folder view or indexer pass can download a whole library and burn the
request budget. Rejected.

**Marker files only** (`.trackerignore`, `.nomedia`, as wusel ships). Cheap, but thumbnailers
ignore them and Baloo needs separate configuration. Not enough on its own.

**Allow list only.** Deny hydration to every process not listed. Breaks editors, terminals and
applications nobody listed, against principle 4. Rejected.

## Consequences

- Known limits: the pid lookup is racy (the process may exit or the pid may be reused between
  request and lookup); sandboxed applications may report a different name or pid 0 and escape
  the list; a thumbnailer that receives `EIO` writes a failure marker, so the file manager may
  not retry once the file is hydrated.
- The breaker can interrupt a legitimate bulk operation such as `cp -r`; the notification is
  the user's way out.
- Seeded thumbnails depend on the Graph thumbnails endpoint and must survive the April 2027
  URL retirement (ADR-0011).
- We need experiments on Omarchy (Files) and GNOME to tune the list and the defaults.

## Rules for implementers

- **R1.** `readdir`, `lookup`, `getattr`, `open`, `getxattr` and `listxattr` never start a download.
- **R2.** A read by a process whose `comm` or `exe` matches the deny list returns `EIO` for any
  range that is not local, and returns data for ranges that are local.
- **R3.** The deny list is read from `config.toml`; an empty list disables it.
- **R4.** When one process exceeds N files or M bytes of hydration within T seconds, its next
  hydrating read fails with `EIO` and exactly one notification is raised.
- **R5.** "Allow once" lets that process continue until it exits; "always allow" persists the
  process to the allow list.
- **R6.** Reads of pinned items and of local content are never refused by R2 or R4.
- **R7.** A seeded thumbnail is written for a placeholder under the path and keys the shared cache
  expects, and never causes hydration.
- **R8.** If the pid lookup fails, the read is treated as from an unknown process: not denied by
  the list, still counted by the breaker.

## Revisit when

- Experiments show the deny list misfires (blocks a real user action or misses a crawler).
- Files (`folio`) gains a plugin hook for thumbnails.
- A FUSE or kernel mechanism appears that tells the daemon a read is a crawl.
- The fanotify backend (ADR-0005) is built, since this policy depends on identifying the
  caller and fanotify reports it differently.
