# ADR-0014: File-manager adapters are thin clients; the Omarchy Files plugin is first

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** G4 (see `docs/architecture/overview.md`)

## Context

Dropbox, Nextcloud/ownCloud, Insync and wusel share one architecture: the daemon owns state
behind a local socket and thin per-file-manager plugins query it and render
([socketapi.cpp](https://raw.githubusercontent.com/nextcloud/desktop/master/src/gui/socketapi/socketapi.cpp),
[wusel status protocol](https://wusel.itbh.at/wusel/latest/explanation/status-protocol.html)).
Nautilus cannot hook file activation, so hydration has to live below the file manager
([wusel](https://wusel.itbh.at/wusel/0.4.0/explanation/desktop-integration.html)); a file manager
without an adapter still gets working on-demand files (principle 4).

What each host can render (research note `file_manager_integration.md`, section 1):

- Nautilus, Nemo, Caja: emblems and dynamic menus; Nextcloud generates its Nemo and Caja
  extensions from one Python source
  ([CMakeLists](https://raw.githubusercontent.com/nextcloud/desktop/master/shell_integration/nautilus/CMakeLists.txt)).
- Dolphin: two compiled plugins (`kfileitemaction`, `overlayicon`) that must match the KDE
  Frameworks major version
  ([Insync forum](https://forums.insynchq.com/t/insync-dolphin-does-not-work-with-plasma-6-new-version-available/19800?page=4),
  [openSUSE 1221923](https://lists.opensuse.org/archives/list/bugs@lists.opensuse.org/thread/MUO5Z6HLHN3L2K5ZNBSQXDANE4RGXVBZ)).
- Thunar: no emblem function in `ThunarxFileInfo`
  ([reference](https://developer.xfce.org/thunar/ThunarxFileInfo.html)).
- yazi: an async "fetcher" Lua plugin can show per-file status
  ([yazi-rs/plugins](https://github.com/yazi-rs/plugins)).
- Adapters must answer from a cache: about 230 µs per query, 46 ms for a cold 200-file folder
  ([wusel status protocol](https://wusel.itbh.at/wusel/latest/explanation/status-protocol.html));
  Nextcloud's script blocks Nautilus for up to 100 ms waiting for menus
  ([syncstate.py](https://raw.githubusercontent.com/nextcloud/desktop/master/shell_integration/nautilus/syncstate.py)).

Omarchy is replacing Nautilus with Files (`folio`, github.com/omacom/folio); PR #18378 was
merged on 2026-11-10 and the migration removes `nautilus` and `nautilus-python` (README,
"Omarchy is replacing Nautilus with Files"). Files plugin interface v1 (`docs/plugins.md`) is a
folder with `manifest.json` plus commands in any language that print JSON. It has `actions`
(limited to paths under a folder), `details`, `poll`/`places`, `section` and `view` hooks, and
no per-file badge on tiles or list rows. Open issue #77 asks for a push-style watch command.

Neutral surfaces: StatusNotifierItem tray, `org.freedesktop.Notifications`, GTK bookmarks, and
libcloudproviders, which gives one account-level sidebar entry in Nautilus and GTK file
choosers and is not consumed by KDE
([libcloudproviders README](https://raw.githubusercontent.com/GNOME/libcloudproviders/main/README.md)).
GIO `metadata::emblems` does not refresh live
([Thunar issue 630](https://gitlab.xfce.org/xfce/thunar/-/issues/630)). Omarchy's tray needed a
workaround for submenus, so its menus should stay flat.

## Decision

An adapter draws status and menu entries and sends commands, either over the IPC socket
(ADR-0009) or by running the `mydrive` CLI. It holds no state of its own beyond a cache of what
the daemon reported, and contains no sync logic. It never blocks the host's UI: it answers from
its cache and every call to the daemon has a bounded timeout.

The first adapter is a plugin for Omarchy's Files: a folder with `manifest.json` and scripts.

- `actions`, restricted to paths under the mount points: "Always Keep on This Device" (pin),
  "Free Up Space" (dehydrate), "Copy Link" and "View Online".
- `details`: the selected file's display status (overview, State model).
- `poll` / `places`: mount status on the mount's sidebar row.

Files plugin interface v1 has no per-file badge, so per-file icons on Omarchy need an upstream
change to Files. Gate G4 asks for it.

Later adapters, in order: one Python source for Nautilus, Nemo and Caja (emblems and menu); two
compiled C++ plugins for Dolphin; a Lua fetcher plugin for yazi; a menu-only Thunar plugin. Each
adapter ships as its own optional package (ADR-0016).

Desktop-neutral surfaces used by the daemon or UI: a StatusNotifierItem tray icon with a flat
menu, `org.freedesktop.Notifications`, GTK bookmarks, and libcloudproviders for an
account-level sidebar entry. GIO `metadata::emblems` is not used.

## Alternatives considered

**Nautilus adapter first.** Lowest cost on Omarchy 4.5.0, but its host is being removed from
stock Omarchy. Kept for other distros.

**Adapters that hold state or decide policy.** Duplicates logic per file manager and breaks
principle 3. Rejected.

**GIO `metadata::emblems` as a neutral emblem mechanism.** No live refresh. Rejected.

**Speaking Nextcloud's socket protocol.** Its status vocabulary cannot tell online-only from
local from pinned. Not adopted as the primary protocol.

## Consequences

- On stock Omarchy, users see per-file status only in the details pane until Files adds a
  badge hook.
- `poll` has a minimum 5 s interval, so sidebar status lags until a push hook exists.
- Each new host costs one small adapter and one package; Dolphin plugins must be rebuilt per
  KDE Frameworks major version.
- Open concern: the `actions` hook is limited to paths under a folder, and mount points are
  per user and change when libraries are added. How the plugin's `under` paths track
  `mydrive mount add` is not settled here and must not need root.
- Open concern: "Copy Link" is read here as copying the item's web link, which grants no new
  access; creating a sharing link would change permissions and needs its own decision.

## Rules for implementers

- **R1.** No adapter writes to the state store, backing directories or `config.toml`.
- **R2.** Every adapter call to the daemon or CLI has a timeout; on timeout the adapter shows the
  cached or an "unknown" status and does not retry in the UI thread.
- **R3.** Adapters use the change stream (ADR-0009) or the host's poll hook; none queries the
  daemon once per file per repaint.
- **R4.** The Files plugin's actions appear only for paths under a mount point.
- **R5.** Status words and icons come from the display status in the overview; adapters do not
  derive status from the three axes themselves.
- **R6.** Removing every adapter package leaves on-demand files working.
- **R7.** The tray menu has no submenus.

## Revisit when

- G4 is answered, or Files adds a per-file badge, a watch command (#77) or a thumbnail hook.
- Omarchy ships Files as default (4.6) and real usage shows gaps.
- A desktop-neutral per-file status mechanism appears.
